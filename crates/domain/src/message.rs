//! Platform-agnostic message model + processing (PRD §2 MSG-*; ADR-051).
//!
//! Every platform adapter (Discord/Slack fetch, WhatsApp parse) converts its
//! native payload into a [`NormalizedMessage`], after which the rest of the
//! system is platform-blind (WSP-006). The *fetching* is host I/O; the model and
//! the processing here are pure — cleaning, code-block extraction, and the
//! substantial-vs-trivial judgement (MSG-004/005/008) that decides what's worth
//! summarizing.

use crate::{string_id, Platform};

string_id!(
    /// Stable message identity. For live platforms this is the native message
    /// id; for WhatsApp it is the synthetic fingerprint (WHA-012/021). Either
    /// way, downstream treats it uniformly (grounded citations, dedup, coverage).
    MessageId,
    "message id",
    512
);

string_id!(
    /// A platform channel/chat identifier (Discord channel, Slack channel,
    /// WhatsApp chat). Platform-opaque; the workspace mapping lives elsewhere.
    ChannelId,
    "channel id",
    256
);

/// Coarse attachment kind (MSG-006). The bytes themselves are the host's
/// concern; the model only records that an attachment of some kind was present,
/// which is enough for triviality and for the summarizer to note "shared a file".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentKind {
    Image,
    Video,
    Audio,
    Document,
    Other,
}

/// A single attachment on a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub kind: AttachmentKind,
    /// Original filename, if the platform provides one (never a URL/secret).
    pub filename: Option<String>,
}

/// A fenced code block extracted from message content (MSG-005).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeBlock {
    /// Language tag on the opening fence, if any (```rust → `Some("rust")`).
    pub language: Option<String>,
    pub body: String,
}

/// The single, platform-agnostic message shape (ADR-051; PRD ProcessedMessage).
/// `author_id` is the *resolved* author identity (a platform user id, or a
/// WhatsApp participant id); `author_name` is a display name/pseudonym and must
/// never carry a phone number (WHA-006). `timestamp` is a UTC unix-seconds
/// instant (WhatsApp is normalized at parse, WHA-020).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedMessage {
    pub id: MessageId,
    pub platform: Platform,
    pub channel_id: ChannelId,
    pub author_id: String,
    pub author_name: String,
    pub content: String,
    pub timestamp: i64,
    /// A join/leave/created-group or other platform system event, not human
    /// content. Retained for context/coverage but never counted as substantial.
    pub is_system: bool,
    /// The message this one replies to, if any (MSG-007 thread/reply context).
    pub reply_to: Option<MessageId>,
    pub attachments: Vec<Attachment>,
}

/// Minimum cleaned word count for a text-only message to count as substantial.
const MIN_SUBSTANTIAL_WORDS: usize = 2;

/// Short acknowledgements that carry no summarizable information (MSG-008).
const TRIVIAL_ACKS: &[&str] = &[
    "ok",
    "okay",
    "k",
    "kk",
    "thanks",
    "thank you",
    "thx",
    "ty",
    "yes",
    "no",
    "yep",
    "nope",
    "yeah",
    "sure",
    "lol",
    "lmao",
    "nice",
    "cool",
    "+1",
    "same",
    "agreed",
    "done",
    "got it",
];

impl NormalizedMessage {
    /// Whether this message carries summarizable signal (MSG-008). System
    /// events, empty/whitespace, bare reactions/acks and single stray words are
    /// trivial; attachments, code blocks, and real sentences are substantial.
    pub fn is_substantial(&self) -> bool {
        if self.is_system {
            return false;
        }
        if !self.attachments.is_empty() {
            return true;
        }
        if !extract_code_blocks(&self.content).is_empty() {
            return true;
        }
        let cleaned = clean_content(&self.content);
        if cleaned.is_empty() {
            return false;
        }
        if TRIVIAL_ACKS.contains(&cleaned.to_lowercase().as_str()) {
            return false;
        }
        let has_alpha = cleaned.chars().any(|c| c.is_alphabetic());
        has_alpha && cleaned.split_whitespace().count() >= MIN_SUBSTANTIAL_WORDS
    }
}

/// Returns true for zero-width/invisible code points worth stripping.
fn is_zero_width(c: char) -> bool {
    matches!(c, '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}')
}

/// Normalize text for comparison/triviality (MSG-004): drop zero-width
/// characters and collapse all runs of whitespace (including newlines) to a
/// single space. Platform-specific stripping (mention/emoji markup) is the
/// adapter's job before this; this is the shared, platform-agnostic pass.
pub fn clean_content(raw: &str) -> String {
    raw.chars()
        .filter(|c| !is_zero_width(*c))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Extract fenced code blocks (MSG-005). Recognizes triple-backtick fences with
/// an optional language tag on the opening fence. Unterminated fences are
/// ignored rather than guessed.
pub fn extract_code_blocks(content: &str) -> Vec<CodeBlock> {
    let mut blocks = Vec::new();
    let mut in_block = false;
    let mut language: Option<String> = None;
    let mut body = String::new();
    for line in content.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("```") {
            if in_block {
                blocks.push(CodeBlock {
                    language: language.take(),
                    body: body.trim_end_matches('\n').to_string(),
                });
                body.clear();
                in_block = false;
            } else {
                in_block = true;
                let tag = rest.trim();
                language = (!tag.is_empty()).then(|| tag.to_string());
            }
        } else if in_block {
            body.push_str(line);
            body.push('\n');
        }
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(content: &str) -> NormalizedMessage {
        NormalizedMessage {
            id: MessageId::parse("m1").unwrap(),
            platform: Platform::Discord,
            channel_id: ChannelId::parse("c1").unwrap(),
            author_id: "u1".to_string(),
            author_name: "Alice".to_string(),
            content: content.to_string(),
            timestamp: 1_700_000_000,
            is_system: false,
            reply_to: None,
            attachments: vec![],
        }
    }

    #[test]
    fn clean_collapses_whitespace_and_strips_zero_width() {
        assert_eq!(clean_content("  hello\n\tworld  "), "hello world");
        assert_eq!(clean_content("a\u{200B}b"), "ab");
        assert_eq!(clean_content("   \n  "), "");
    }

    #[test]
    fn substantial_real_sentence() {
        assert!(msg("Let's ship the release tomorrow").is_substantial());
    }

    #[test]
    fn trivial_acks_and_reactions_are_not_substantial() {
        for t in ["ok", "Thanks", "👍", "  ", "lol", "+1", "thank you"] {
            assert!(!msg(t).is_substantial(), "{t:?} should be trivial");
        }
    }

    #[test]
    fn single_word_is_trivial_but_attachments_and_code_are_not() {
        assert!(!msg("deploy").is_substantial());

        let mut with_file = msg("");
        with_file.attachments.push(Attachment {
            kind: AttachmentKind::Document,
            filename: Some("spec.pdf".to_string()),
        });
        assert!(with_file.is_substantial());

        assert!(msg("```rust\nfn main() {}\n```").is_substantial());
    }

    #[test]
    fn system_messages_are_never_substantial() {
        let mut m = msg("Alice created group \"Team\"");
        m.is_system = true;
        assert!(!m.is_substantial());
    }

    #[test]
    fn extract_code_block_with_language() {
        let blocks = extract_code_blocks("intro\n```rust\nfn main() {}\nlet x = 1;\n```\noutro");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].language.as_deref(), Some("rust"));
        assert_eq!(blocks[0].body, "fn main() {}\nlet x = 1;");
    }

    #[test]
    fn extract_code_block_without_language_and_ignores_unterminated() {
        let blocks = extract_code_blocks("```\nplain\n```");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].language, None);
        assert_eq!(blocks[0].body, "plain");

        // Unterminated fence yields nothing rather than a partial block.
        assert!(extract_code_blocks("```rust\nfn main() {}").is_empty());
    }

    #[test]
    fn multiple_code_blocks() {
        let blocks = extract_code_blocks("```py\nx=1\n```\ntext\n```js\ny=2\n```");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].language.as_deref(), Some("py"));
        assert_eq!(blocks[1].language.as_deref(), Some("js"));
    }
}
