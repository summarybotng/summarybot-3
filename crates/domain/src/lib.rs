//! Core domain logic, shared by the native host and the WASM guest.
//!
//! This crate is the single source of truth for summarization logic so the
//! same code runs natively (tests, host fallback) and inside the sandboxed
//! WASM component. It has no I/O and no platform dependencies (PRD §12.0:
//! "WASM does bounded/streamed pure compute"; logic never lives in handlers).

/// Validation failures surfaced at system boundaries (PRD §12.0: fail-fast,
/// newtypes, no silent fallbacks).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    EmptyWorkspaceId,
    WorkspaceIdTooLong { len: usize, max: usize },
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::EmptyWorkspaceId => write!(f, "workspace id must not be empty"),
            ValidationError::WorkspaceIdTooLong { len, max } => {
                write!(f, "workspace id length {len} exceeds max {max}")
            }
        }
    }
}

impl std::error::Error for ValidationError {}

/// A validated workspace identifier. Construction is the only way in, so an
/// existing `WorkspaceId` is guaranteed well-formed (parse, don't validate).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WorkspaceId(String);

impl WorkspaceId {
    /// Maximum accepted length; generous but bounded to reject abuse early.
    pub const MAX_LEN: usize = 256;

    pub fn parse(raw: impl Into<String>) -> Result<Self, ValidationError> {
        let raw = raw.into();
        if raw.trim().is_empty() {
            return Err(ValidationError::EmptyWorkspaceId);
        }
        if raw.len() > Self::MAX_LEN {
            return Err(ValidationError::WorkspaceIdTooLong {
                len: raw.len(),
                max: Self::MAX_LEN,
            });
        }
        Ok(Self(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Bounded input to the pure-compute summarizer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryInput {
    pub workspace_id: WorkspaceId,
    pub messages: Vec<String>,
}

/// Result of summarization. `message_count`/`word_count` are derived invariants
/// the property tests pin down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub text: String,
    pub message_count: u32,
    pub word_count: u32,
}

/// Number of leading words kept in the preview line.
const PREVIEW_WORDS: usize = 12;

/// Produce a trivial extractive summary. Deterministic and side-effect free —
/// a placeholder for the real Phase 4 pipeline, sufficient to prove the
/// host -> WASM -> repository -> DB walking skeleton.
pub fn summarize(input: &SummaryInput) -> Summary {
    let message_count = input.messages.len() as u32;
    let word_count = input
        .messages
        .iter()
        .flat_map(|m| m.split_whitespace())
        .count() as u32;
    let preview: String = input
        .messages
        .first()
        .map(|m| {
            m.split_whitespace()
                .take(PREVIEW_WORDS)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    let text = format!("[{message_count} msgs / {word_count} words] {preview}");
    Summary {
        text,
        message_count,
        word_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn workspace_id_rejects_empty_and_blank() {
        assert_eq!(
            WorkspaceId::parse(""),
            Err(ValidationError::EmptyWorkspaceId)
        );
        assert_eq!(
            WorkspaceId::parse("   "),
            Err(ValidationError::EmptyWorkspaceId)
        );
    }

    #[test]
    fn workspace_id_rejects_overlong() {
        let raw = "x".repeat(WorkspaceId::MAX_LEN + 1);
        assert!(matches!(
            WorkspaceId::parse(raw),
            Err(ValidationError::WorkspaceIdTooLong { .. })
        ));
    }

    #[test]
    fn summarize_empty_is_zeroed() {
        let input = SummaryInput {
            workspace_id: WorkspaceId::parse("ws").unwrap(),
            messages: vec![],
        };
        let s = summarize(&input);
        assert_eq!(s.message_count, 0);
        assert_eq!(s.word_count, 0);
    }

    proptest! {
        // message_count always equals the number of input messages.
        #[test]
        fn message_count_matches_input(msgs in proptest::collection::vec(".*", 0..50)) {
            let input = SummaryInput {
                workspace_id: WorkspaceId::parse("ws").unwrap(),
                messages: msgs.clone(),
            };
            prop_assert_eq!(summarize(&input).message_count as usize, msgs.len());
        }

        // word_count equals the total whitespace-split token count.
        #[test]
        fn word_count_matches_tokens(msgs in proptest::collection::vec("[a-z ]{0,40}", 0..50)) {
            let expected: usize = msgs.iter().flat_map(|m| m.split_whitespace()).count();
            let input = SummaryInput {
                workspace_id: WorkspaceId::parse("ws").unwrap(),
                messages: msgs,
            };
            prop_assert_eq!(summarize(&input).word_count as usize, expected);
        }

        // summarize is pure: identical input yields identical output.
        #[test]
        fn summarize_is_deterministic(msgs in proptest::collection::vec(".*", 0..30)) {
            let input = SummaryInput {
                workspace_id: WorkspaceId::parse("ws").unwrap(),
                messages: msgs,
            };
            prop_assert_eq!(summarize(&input), summarize(&input));
        }
    }
}
