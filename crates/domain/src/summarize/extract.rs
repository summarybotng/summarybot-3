//! Structured extraction + grounded citations + quality validation
//! (PRD §12.4; ADR-004, SUM-017 / open-question-#6).
//!
//! The model is asked to return structured JSON (summary text, key points,
//! action items, technical terms, participants, and citations that ground claims
//! in specific input messages). The host parses that JSON into a [`RawExtraction`]
//! (serde lives host-side); this module — purely — **validates structurally**
//! (Q#6: trust `finish_reason` + shape, not marker strings) and **resolves
//! citation indices** to stable message ids (ADR-004 position-index resolution).

use crate::MessageId;

/// An action item the summary surfaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionItem {
    pub text: String,
    pub assignee: Option<String>,
}

/// A citation as the model produced it: a 0-based index into the input messages
/// it grounds, plus an optional quoted span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawCitation {
    pub message_index: usize,
    pub quote: Option<String>,
}

/// A citation after resolution to a stable message id (ADR-004).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCitation {
    pub message_id: MessageId,
    pub quote: Option<String>,
}

/// Why the model stopped — the structural signal Q#6 relies on instead of
/// scanning the text for markers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    /// Completed normally.
    Stop,
    /// Hit the output token limit — the result is truncated, not trustworthy.
    Length,
    /// Blocked by a content filter.
    ContentFilter,
    /// Anything else.
    Other,
}

/// The raw, parsed-but-unvalidated extraction the host hands in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawExtraction {
    pub text: String,
    pub key_points: Vec<String>,
    pub action_items: Vec<ActionItem>,
    pub technical_terms: Vec<String>,
    pub participants: Vec<String>,
    pub citations: Vec<RawCitation>,
}

/// A validated, citation-resolved summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedSummary {
    pub text: String,
    pub key_points: Vec<String>,
    pub action_items: Vec<ActionItem>,
    pub technical_terms: Vec<String>,
    pub participants: Vec<String>,
    pub citations: Vec<ResolvedCitation>,
}

/// Structural quality failure (Q#6) — distinct so the caller can react (retry a
/// truncation on a bigger budget, surface a filter, etc.).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QualityError {
    /// `finish_reason == Length`: output was cut off mid-generation.
    Truncated,
    /// Blocked by a content filter.
    Filtered,
    /// No summary text and no key points — nothing usable.
    Empty,
    /// A citation pointed outside the input message range.
    CitationOutOfRange { index: usize, message_count: usize },
}

/// Validate an extraction structurally and resolve its citations against the
/// **ordered** input message ids. Returns the clean [`ExtractedSummary`] or the
/// first structural problem found.
pub fn finalize(
    raw: RawExtraction,
    finish_reason: FinishReason,
    message_ids: &[MessageId],
) -> Result<ExtractedSummary, QualityError> {
    match finish_reason {
        FinishReason::Length => return Err(QualityError::Truncated),
        FinishReason::ContentFilter => return Err(QualityError::Filtered),
        FinishReason::Stop | FinishReason::Other => {}
    }
    if raw.text.trim().is_empty() && raw.key_points.is_empty() {
        return Err(QualityError::Empty);
    }
    let mut citations = Vec::with_capacity(raw.citations.len());
    for c in raw.citations {
        let id = message_ids
            .get(c.message_index)
            .ok_or(QualityError::CitationOutOfRange {
                index: c.message_index,
                message_count: message_ids.len(),
            })?;
        citations.push(ResolvedCitation {
            message_id: id.clone(),
            quote: c.quote,
        });
    }
    Ok(ExtractedSummary {
        text: raw.text,
        key_points: raw.key_points,
        action_items: raw.action_items,
        technical_terms: raw.technical_terms,
        participants: raw.participants,
        citations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: usize) -> Vec<MessageId> {
        (0..n)
            .map(|i| MessageId::parse(format!("m{i}")).unwrap())
            .collect()
    }

    fn raw(text: &str, citations: Vec<RawCitation>) -> RawExtraction {
        RawExtraction {
            text: text.into(),
            key_points: vec!["decided to ship".into()],
            action_items: vec![ActionItem {
                text: "write the changelog".into(),
                assignee: Some("Alice".into()),
            }],
            technical_terms: vec!["WASM".into()],
            participants: vec!["Alice".into(), "Bob".into()],
            citations,
        }
    }

    #[test]
    fn resolves_citation_indices_to_message_ids() {
        let out = finalize(
            raw(
                "we shipped",
                vec![RawCitation {
                    message_index: 2,
                    quote: Some("ship it".into()),
                }],
            ),
            FinishReason::Stop,
            &ids(5),
        )
        .unwrap();
        assert_eq!(out.citations.len(), 1);
        assert_eq!(out.citations[0].message_id.as_str(), "m2");
        assert_eq!(out.citations[0].quote.as_deref(), Some("ship it"));
    }

    #[test]
    fn truncated_output_is_rejected() {
        assert_eq!(
            finalize(raw("partial", vec![]), FinishReason::Length, &ids(3)),
            Err(QualityError::Truncated)
        );
    }

    #[test]
    fn content_filter_is_rejected() {
        assert_eq!(
            finalize(raw("x", vec![]), FinishReason::ContentFilter, &ids(3)),
            Err(QualityError::Filtered)
        );
    }

    #[test]
    fn empty_extraction_is_rejected() {
        let empty = RawExtraction {
            text: "   ".into(),
            key_points: vec![],
            action_items: vec![],
            technical_terms: vec![],
            participants: vec![],
            citations: vec![],
        };
        assert_eq!(
            finalize(empty, FinishReason::Stop, &ids(3)),
            Err(QualityError::Empty)
        );
    }

    #[test]
    fn out_of_range_citation_is_rejected() {
        let out = finalize(
            raw(
                "x",
                vec![RawCitation {
                    message_index: 9,
                    quote: None,
                }],
            ),
            FinishReason::Stop,
            &ids(3),
        );
        assert_eq!(
            out,
            Err(QualityError::CitationOutOfRange {
                index: 9,
                message_count: 3
            })
        );
    }

    #[test]
    fn text_only_summary_without_key_points_is_fine() {
        let r = RawExtraction {
            text: "a short summary".into(),
            key_points: vec![],
            action_items: vec![],
            technical_terms: vec![],
            participants: vec![],
            citations: vec![],
        };
        assert!(finalize(r, FinishReason::Stop, &ids(0)).is_ok());
    }
}
