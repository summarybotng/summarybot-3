//! Coherence gate (PRD §8.3 COH-001, HIGH) — pure grounding check.
//!
//! Validates a summary's claims against its source messages to catch
//! hallucination: a claim whose significant words barely appear in the sources
//! is **flagged**, not blocked — so an ungrounded claim is *visible* rather than
//! silently published. v1 is lexical (token overlap); an LLM-judge is a stronger
//! later pass. Provenance (every claim → source segment, COH-005) is carried
//! separately by knowledge units and citations.

use crate::summarize::ExtractedSummary;
use std::collections::HashSet;

/// Result of grounding a summary's claims against its sources.
#[derive(Debug, Clone, PartialEq)]
pub struct CoherenceReport {
    /// Overall grounded fraction in `[0, 1]` — share of claim words found in the
    /// sources. 1.0 when there's nothing to ground; 0.0 when sources are absent.
    pub score: f32,
    /// Claims whose grounding fell below the floor (likely hallucinated/unsupported).
    pub ungrounded: Vec<String>,
}

/// A claim with < this share of its significant words in the sources is flagged.
const GROUND_FLOOR: f32 = 0.5;
/// Don't flag very short claims (too few words to assess reliably).
const MIN_CLAIM_TOKENS: usize = 3;

fn is_stopword(w: &str) -> bool {
    matches!(
        w,
        "the"
            | "and"
            | "for"
            | "that"
            | "this"
            | "with"
            | "from"
            | "they"
            | "have"
            | "will"
            | "into"
            | "your"
            | "you"
            | "are"
            | "was"
            | "were"
            | "their"
            | "them"
            | "then"
            | "than"
            | "about"
            | "would"
            | "could"
            | "should"
            | "which"
            | "what"
            | "when"
    )
}

/// Significant tokens: lowercased alphanumeric words of length ≥ 4 that aren't
/// stopwords. (Short words carry little grounding signal and inflate noise.)
fn significant_tokens(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 4 && !is_stopword(w))
        .map(str::to_string)
        .collect()
}

/// Ground `summary`'s claims (headline + key points + action items) against the
/// `sources` (source message texts). See [`CoherenceReport`].
pub fn check_coherence(summary: &ExtractedSummary, sources: &[&str]) -> CoherenceReport {
    let source_set: HashSet<String> = sources.iter().flat_map(|s| significant_tokens(s)).collect();
    if source_set.is_empty() {
        // Nothing to ground against — unassessed, don't raise false alarms.
        return CoherenceReport {
            score: 0.0,
            ungrounded: vec![],
        };
    }

    let mut claims: Vec<String> = Vec::new();
    if !summary.text.trim().is_empty() {
        claims.push(summary.text.clone());
    }
    claims.extend(summary.key_points.iter().map(|k| k.text.clone()));
    claims.extend(summary.action_items.iter().map(|a| a.text.clone()));

    let mut total = 0usize;
    let mut grounded = 0usize;
    let mut ungrounded = Vec::new();
    for claim in &claims {
        let toks = significant_tokens(claim);
        if toks.is_empty() {
            continue;
        }
        let hits = toks.iter().filter(|t| source_set.contains(*t)).count();
        total += toks.len();
        grounded += hits;
        let fraction = hits as f32 / toks.len() as f32;
        if toks.len() >= MIN_CLAIM_TOKENS && fraction < GROUND_FLOOR {
            ungrounded.push(claim.trim().to_string());
        }
    }

    let score = if total == 0 {
        1.0
    } else {
        grounded as f32 / total as f32
    };
    CoherenceReport { score, ungrounded }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::summarize::{ActionItem, ResolvedCitation};
    use crate::MessageId;

    fn summary(text: &str, points: &[&str]) -> ExtractedSummary {
        ExtractedSummary {
            text: text.into(),
            key_points: points.iter().map(|p| (*p).into()).collect(),
            action_items: vec![ActionItem {
                text: "deploy the service".into(),
                assignee: None,
            }],
            technical_terms: vec![],
            participants: vec![],
            citations: vec![ResolvedCitation {
                message_id: MessageId::parse("m0").unwrap(),
                quote: None,
            }],
        }
    }

    #[test]
    fn well_grounded_summary_scores_high_with_no_flags() {
        let sources = [
            "We must run the database migration before the deploy on Friday.",
            "Alice will deploy the service after the migration.",
        ];
        let s = summary(
            "Database migration must run before the Friday deploy.",
            &["Run the migration before deploy"],
        );
        let r = check_coherence(&s, &sources);
        assert!(r.score > 0.7, "score was {}", r.score);
        assert!(r.ungrounded.is_empty(), "flagged: {:?}", r.ungrounded);
    }

    #[test]
    fn hallucinated_claim_is_flagged() {
        let sources = ["We discussed the database migration timing."];
        let mut s = summary("Database migration timing discussed.", &[]);
        // A key point with no support in the sources.
        s.key_points = vec!["The quarterly revenue forecast exceeded marketing projections".into()];
        let r = check_coherence(&s, &sources);
        assert!(r.ungrounded.iter().any(|c| c.contains("revenue forecast")));
        assert!(r.score < 1.0);
    }

    #[test]
    fn no_sources_is_unassessed_not_alarming() {
        let s = summary("anything at all here", &["some claim words"]);
        let r = check_coherence(&s, &[]);
        assert_eq!(r.score, 0.0);
        assert!(r.ungrounded.is_empty());
    }
}
