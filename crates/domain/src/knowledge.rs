//! Knowledge units + vector-search math (PRD §8.1; ADR-127) — pure core.
//!
//! A produced summary yields **knowledge units** (its headline, key points, and
//! action items), each carrying provenance back to the summary and its grounded
//! source messages (KNO-001, COH-005). Embedding + storage + ranking-at-scale are
//! host/repository concerns; the *extraction* and the *similarity math* are pure
//! and live here so they're exhaustively testable without a model or a database.

use crate::summarize::ExtractedSummary;
use crate::MessageId;

/// What part of a summary a unit came from (drives display + later weighting).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitKind {
    /// The summary's prose headline.
    Headline,
    /// One key point.
    KeyPoint,
    /// One action item.
    ActionItem,
}

impl UnitKind {
    pub fn as_str(self) -> &'static str {
        match self {
            UnitKind::Headline => "headline",
            UnitKind::KeyPoint => "key_point",
            UnitKind::ActionItem => "action_item",
        }
    }
}

/// A single extracted knowledge unit, with provenance + grounding metadata
/// (ADR-127/129; ADR-117 carries these into RVF export).
#[derive(Debug, Clone, PartialEq)]
pub struct KnowledgeUnit {
    /// The summary this unit was extracted from.
    pub summary_id: String,
    pub kind: UnitKind,
    pub text: String,
    /// Source messages grounding this unit — for a key point, its own per-claim
    /// references (ADR-004); otherwise the summary's citation union (COH-005).
    pub source_message_ids: Vec<MessageId>,
    /// The channel/scope the summary covered (`None` for a workspace-wide or
    /// unscoped summary).
    pub source_channel: Option<String>,
    /// When the cited activity happened (unix seconds): the earliest reference
    /// timestamp for a grounded claim, else the summary's creation time.
    pub source_date: i64,
    /// Confidence the claim is supported by its sources (ADR-004). 1.0 for the
    /// headline and action items (not individually scored by the model).
    pub confidence: f32,
}

/// Max units kept per summary (PRD §12.8: ≤20/summary) — keeps the index lean.
pub const MAX_UNITS_PER_SUMMARY: usize = 20;

/// Extract knowledge units from a produced summary (KNO-001). Units inherit the
/// summary's grounded source messages as provenance. Capped at
/// [`MAX_UNITS_PER_SUMMARY`]; trivial/empty texts are skipped.
pub fn extract_units(
    summary: &ExtractedSummary,
    summary_id: &str,
    source_channel: Option<&str>,
    created_at: i64,
) -> Vec<KnowledgeUnit> {
    let union: Vec<MessageId> = summary
        .citations
        .iter()
        .map(|c| c.message_id.clone())
        .collect();
    let channel = source_channel.map(str::to_string);
    let mut units = Vec::new();
    let mut push = |kind: UnitKind, text: String, sources: Vec<MessageId>, date: i64, confidence: f32| {
        if !text.trim().is_empty() {
            units.push(KnowledgeUnit {
                summary_id: summary_id.to_string(),
                kind,
                text: text.trim().to_string(),
                source_message_ids: sources,
                source_channel: channel.clone(),
                source_date: date,
                confidence,
            });
        }
    };
    push(UnitKind::Headline, summary.text.clone(), union.clone(), created_at, 1.0);
    for kp in &summary.key_points {
        // A key point carries its own grounding (ADR-004): cite its references
        // and date it to the earliest cited message; fall back to the summary.
        let (sources, date) = if kp.references.is_empty() {
            (union.clone(), created_at)
        } else {
            let ids = kp.references.iter().map(|r| r.message_id.clone()).collect();
            let earliest = kp.references.iter().map(|r| r.timestamp).min().unwrap_or(created_at);
            (ids, earliest)
        };
        push(UnitKind::KeyPoint, kp.text.clone(), sources, date, kp.confidence);
    }
    for ai in &summary.action_items {
        let text = match &ai.assignee {
            Some(who) => format!("{} (owner: {who})", ai.text),
            None => ai.text.clone(),
        };
        push(UnitKind::ActionItem, text, union.clone(), created_at, 1.0);
    }
    units.truncate(MAX_UNITS_PER_SUMMARY);
    units
}

/// Cosine similarity of two embedding vectors in `[-1, 1]`. Returns 0 for a
/// length mismatch or a zero-norm vector (no meaningful direction).
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Rank `candidates` (id + embedding) by cosine similarity to `query`, returning
/// the top `k` as `(id, score)` descending. Ties keep input order (stable sort).
pub fn rank_by_cosine(
    query: &[f32],
    candidates: &[(String, Vec<f32>)],
    k: usize,
) -> Vec<(String, f32)> {
    let mut scored: Vec<(String, f32)> = candidates
        .iter()
        .map(|(id, v)| (id.clone(), cosine_similarity(query, v)))
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(k);
    scored
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::summarize::{ActionItem, ResolvedCitation};

    fn summary() -> ExtractedSummary {
        ExtractedSummary {
            text: "The team agreed to launch Friday.".into(),
            key_points: vec!["Launch on Friday".into(), "  ".into()],
            action_items: vec![
                ActionItem {
                    text: "write changelog".into(),
                    assignee: Some("Alice".into()),
                },
                ActionItem {
                    text: "notify users".into(),
                    assignee: None,
                },
            ],
            technical_terms: vec![],
            participants: vec![],
            citations: vec![ResolvedCitation {
                message_id: MessageId::parse("m0").unwrap(),
                quote: None,
            }],
        }
    }

    #[test]
    fn extracts_units_with_provenance_and_skips_blanks() {
        let units = extract_units(&summary(), "sum_1", Some("c1"), 5000);
        // headline + 1 real key point (blank skipped) + 2 action items = 4
        assert_eq!(units.len(), 4);
        assert_eq!(units[0].kind, UnitKind::Headline);
        assert_eq!(units[0].text, "The team agreed to launch Friday.");
        assert!(units
            .iter()
            .any(|u| u.text == "write changelog (owner: Alice)"));
        assert!(units.iter().any(|u| u.text == "notify users"));
        // Every unit carries the summary's source message as provenance.
        assert!(units
            .iter()
            .all(|u| u.summary_id == "sum_1" && u.source_message_ids[0].as_str() == "m0"));
    }

    #[test]
    fn units_are_capped() {
        let mut s = summary();
        s.key_points = (0..50).map(|i| format!("point {i}").into()).collect();
        assert_eq!(extract_units(&s, "x", None, 0).len(), MAX_UNITS_PER_SUMMARY);
    }

    #[test]
    fn cosine_basics() {
        assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert!((cosine_similarity(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-6);
        // Degenerate inputs → 0, never NaN.
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
        assert_eq!(cosine_similarity(&[1.0], &[1.0, 2.0]), 0.0);
    }

    #[test]
    fn ranks_top_k_by_similarity() {
        let candidates = vec![
            ("a".to_string(), vec![1.0, 0.0]),
            ("b".to_string(), vec![0.0, 1.0]),
            ("c".to_string(), vec![0.9, 0.1]),
        ];
        let top = rank_by_cosine(&[1.0, 0.0], &candidates, 2);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].0, "a");
        assert_eq!(top[1].0, "c");
    }
}
