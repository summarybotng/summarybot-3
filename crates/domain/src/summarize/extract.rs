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

/// Slim source-message metadata the resolver needs to build per-claim references
/// (ADR-004 §2.1) — the author, instant, and text behind a cited message. The
/// host builds these from its `NormalizedMessage`s (or synthetic partials).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitationSource {
    pub id: MessageId,
    pub author_name: String,
    pub timestamp: i64,
    pub content: String,
}

/// A resolved reference anchoring a claim to one source message (ADR-004 §2.1):
/// the message id plus the human-locating fields (author, time, 1-based position
/// in the window, and a short snippet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageReference {
    pub message_id: MessageId,
    pub author_name: String,
    pub timestamp: i64,
    /// 1-based ordinal position in the summarized window (ADR-004's key locator).
    pub position: usize,
    /// A short excerpt (<= 200 chars) of the source message.
    pub snippet: String,
}

/// Longest snippet a reference carries (ADR-004 §2.1).
const SNIPPET_MAX: usize = 200;

/// A claim the model produced, with the 0-based indices of the messages that
/// ground it and a self-assessed confidence (ADR-004 `ReferencedClaim`).
#[derive(Debug, Clone, PartialEq)]
pub struct RawClaim {
    pub text: String,
    pub citations: Vec<usize>,
    pub confidence: f32,
}

impl From<&str> for RawClaim {
    fn from(text: &str) -> Self {
        Self { text: text.to_string(), citations: vec![], confidence: 1.0 }
    }
}
impl From<String> for RawClaim {
    fn from(text: String) -> Self {
        Self { text, citations: vec![], confidence: 1.0 }
    }
}

/// A summary claim with its supporting source references resolved (ADR-004).
/// A claim with no references is still valid — it's then surfaced as ungrounded
/// by the coherence gate rather than dropped.
#[derive(Debug, Clone, PartialEq)]
pub struct ReferencedClaim {
    pub text: String,
    pub references: Vec<MessageReference>,
    /// Model's self-assessed confidence the claim is supported by its references.
    pub confidence: f32,
}

impl From<&str> for ReferencedClaim {
    fn from(text: &str) -> Self {
        Self { text: text.to_string(), references: vec![], confidence: 1.0 }
    }
}
impl From<String> for ReferencedClaim {
    fn from(text: String) -> Self {
        Self { text, references: vec![], confidence: 1.0 }
    }
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
#[derive(Debug, Clone, PartialEq)]
pub struct RawExtraction {
    pub text: String,
    pub key_points: Vec<RawClaim>,
    pub action_items: Vec<ActionItem>,
    pub technical_terms: Vec<String>,
    pub participants: Vec<String>,
    pub citations: Vec<RawCitation>,
}

/// A validated, citation-resolved summary.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedSummary {
    pub text: String,
    pub key_points: Vec<ReferencedClaim>,
    pub action_items: Vec<ActionItem>,
    pub technical_terms: Vec<String>,
    pub participants: Vec<String>,
    /// Summary-level union of every claim's references (deduped by message id),
    /// kept for provenance consumers (knowledge units, the citations store) that
    /// don't need the per-claim breakdown. The per-claim refs live on `key_points`.
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

/// Char-safe truncation to at most `max` characters (snippet bound, ADR-004).
fn truncate(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        t.to_string()
    } else {
        t.chars().take(max).collect()
    }
}

/// Validate an extraction structurally and resolve each claim's citation indices
/// to per-claim [`MessageReference`]s against the **ordered** input `sources`
/// (ADR-004). Returns the clean [`ExtractedSummary`] or the first structural
/// problem. `key_points` carry their own references; `citations` is the
/// deduped summary-level union for provenance consumers.
pub fn finalize(
    raw: RawExtraction,
    finish_reason: FinishReason,
    sources: &[CitationSource],
) -> Result<ExtractedSummary, QualityError> {
    match finish_reason {
        FinishReason::Length => return Err(QualityError::Truncated),
        FinishReason::ContentFilter => return Err(QualityError::Filtered),
        FinishReason::Stop | FinishReason::Other => {}
    }
    if raw.text.trim().is_empty() && raw.key_points.is_empty() {
        return Err(QualityError::Empty);
    }
    let reference = |idx: usize| -> Result<MessageReference, QualityError> {
        let s = sources.get(idx).ok_or(QualityError::CitationOutOfRange {
            index: idx,
            message_count: sources.len(),
        })?;
        Ok(MessageReference {
            message_id: s.id.clone(),
            author_name: s.author_name.clone(),
            timestamp: s.timestamp,
            position: idx + 1, // ADR-004: 1-based position in the window
            snippet: truncate(&s.content, SNIPPET_MAX),
        })
    };

    // Resolve each claim's references (ADR-004 per-claim grounding).
    let mut key_points = Vec::with_capacity(raw.key_points.len());
    for c in raw.key_points {
        let mut references = Vec::with_capacity(c.citations.len());
        for idx in c.citations {
            references.push(reference(idx)?);
        }
        key_points.push(ReferencedClaim {
            text: c.text,
            references,
            confidence: c.confidence.clamp(0.0, 1.0),
        });
    }

    // Summary-level union: every claim's cited message + any flat model-level
    // citations, deduped by message id (provenance for knowledge/storage).
    let mut citations: Vec<ResolvedCitation> = Vec::new();
    let add = |id: MessageId, quote: Option<String>, v: &mut Vec<ResolvedCitation>| {
        if !v.iter().any(|c| c.message_id == id) {
            v.push(ResolvedCitation { message_id: id, quote });
        }
    };
    for kp in &key_points {
        for r in &kp.references {
            add(r.message_id.clone(), None, &mut citations);
        }
    }
    for c in raw.citations {
        let s = sources.get(c.message_index).ok_or(QualityError::CitationOutOfRange {
            index: c.message_index,
            message_count: sources.len(),
        })?;
        add(s.id.clone(), c.quote, &mut citations);
    }

    Ok(ExtractedSummary {
        text: raw.text,
        key_points,
        action_items: raw.action_items,
        technical_terms: raw.technical_terms,
        participants: raw.participants,
        citations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources(n: usize) -> Vec<CitationSource> {
        (0..n)
            .map(|i| CitationSource {
                id: MessageId::parse(format!("m{i}")).unwrap(),
                author_name: format!("author{i}"),
                timestamp: 1_000 + i as i64,
                content: format!("message {i} content"),
            })
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
    fn resolves_flat_citation_indices_into_the_union() {
        let out = finalize(
            raw(
                "we shipped",
                vec![RawCitation {
                    message_index: 2,
                    quote: Some("ship it".into()),
                }],
            ),
            FinishReason::Stop,
            &sources(5),
        )
        .unwrap();
        assert_eq!(out.citations.len(), 1);
        assert_eq!(out.citations[0].message_id.as_str(), "m2");
        assert_eq!(out.citations[0].quote.as_deref(), Some("ship it"));
    }

    #[test]
    fn resolves_per_claim_references_with_position_author_and_snippet() {
        // ADR-004: each key point carries its own references (message id, author,
        // 1-based position, snippet) and a confidence.
        let mut r = raw("we shipped", vec![]);
        r.key_points = vec![RawClaim {
            text: "shipped the release".into(),
            citations: vec![0, 3],
            confidence: 0.9,
        }];
        let out = finalize(r, FinishReason::Stop, &sources(5)).unwrap();
        let kp = &out.key_points[0];
        assert_eq!(kp.text, "shipped the release");
        assert_eq!(kp.confidence, 0.9);
        assert_eq!(kp.references.len(), 2);
        assert_eq!(kp.references[0].message_id.as_str(), "m0");
        assert_eq!(kp.references[0].position, 1); // 1-based
        assert_eq!(kp.references[0].author_name, "author0");
        assert_eq!(kp.references[0].snippet, "message 0 content");
        assert_eq!(kp.references[1].position, 4);
        // The union reflects both cited messages.
        let ids: Vec<&str> = out.citations.iter().map(|c| c.message_id.as_str()).collect();
        assert_eq!(ids, vec!["m0", "m3"]);
    }

    #[test]
    fn a_claims_out_of_range_reference_is_rejected() {
        let mut r = raw("x", vec![]);
        r.key_points = vec![RawClaim { text: "bad".into(), citations: vec![9], confidence: 1.0 }];
        assert_eq!(
            finalize(r, FinishReason::Stop, &sources(3)),
            Err(QualityError::CitationOutOfRange { index: 9, message_count: 3 })
        );
    }

    #[test]
    fn truncated_output_is_rejected() {
        assert_eq!(
            finalize(raw("partial", vec![]), FinishReason::Length, &sources(3)),
            Err(QualityError::Truncated)
        );
    }

    #[test]
    fn content_filter_is_rejected() {
        assert_eq!(
            finalize(raw("x", vec![]), FinishReason::ContentFilter, &sources(3)),
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
            finalize(empty, FinishReason::Stop, &sources(3)),
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
            &sources(3),
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
        assert!(finalize(r, FinishReason::Stop, &sources(0)).is_ok());
    }
}
