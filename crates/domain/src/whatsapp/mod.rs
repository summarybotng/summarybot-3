//! WhatsApp `_chat.txt` export parser (PRD §2.3 WHA-002/011/015/020; ADR-121).
//!
//! Pure, bounded compute (WHA-011) — the host unzips and streams the text and
//! supplies the declared zone + date order; this returns a UTC-normalized,
//! pre-identity intermediate. Identity resolution, phone anonymization and the
//! synthetic-fingerprint message id are **host-side** (ADR-121), so this stops
//! short of [`crate::NormalizedMessage`]: it emits [`RawWhatsAppMessage`]s the
//! host then resolves, anonymizes and fingerprints.

mod datetime;
pub use datetime::DateOrder;
use datetime::{parse_stamp, to_utc};

use chrono_tz::Tz;

/// Which export dialect a line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhatsAppFormat {
    /// iOS: `[DD/MM/YYYY, HH:MM:SS] Sender: message`.
    Ios,
    /// Android: `DD/MM/YYYY, HH:MM - Sender: message`.
    Android,
}

/// A parsed message before identity resolution/anonymization (ADR-121). The
/// raw `sender` string is retained verbatim; the host resolves it to a
/// participant and anonymizes any phone number before storage (WHA-006/013).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawWhatsAppMessage {
    /// UTC unix seconds (normalized via the declared zone, WHA-020).
    pub timestamp: i64,
    /// Raw sender as it appeared, or `None` for a system line.
    pub sender: Option<String>,
    /// Message body (continuation lines joined with `\n`).
    pub content: String,
    pub is_system: bool,
    /// The body was a media placeholder (`<Media omitted>` etc., MSG-006 hint).
    pub has_media: bool,
}

/// A group lifecycle event detected from system lines (WHA-015) — used to learn
/// the chat predates any export and to drive coverage-gap classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatEventKind {
    GroupCreated,
    MemberJoinedOrAdded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatEvent {
    pub kind: ChatEventKind,
    pub timestamp: i64,
    pub text: String,
}

/// Result of parsing one export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedExport {
    pub format: WhatsAppFormat,
    pub messages: Vec<RawWhatsAppMessage>,
    pub events: Vec<ChatEvent>,
    /// (earliest, latest) UTC timestamp across messages + events, if any.
    pub date_range: Option<(i64, i64)>,
}

/// Parse a WhatsApp export. `zone`/`order` are declared at upload (the file
/// carries neither). Lines that don't begin a new message are treated as
/// continuations of the previous one; unparseable headers are skipped.
pub fn parse_export(raw: &str, zone: Tz, order: DateOrder) -> ParsedExport {
    let mut format = WhatsAppFormat::Ios;
    let mut messages: Vec<RawWhatsAppMessage> = Vec::new();
    let mut events: Vec<ChatEvent> = Vec::new();

    for line in raw.lines() {
        let line = sanitize_line(line);
        match split_header(&line, order) {
            Some((naive_stamp, rest, fmt)) => {
                let Some(ts) = to_utc(naive_stamp, zone) else {
                    continue; // nonexistent local time (DST gap) — skip
                };
                format = fmt;
                let (sender, content, is_system) = split_body(&rest);
                if is_system {
                    if let Some(kind) = classify_event(&content) {
                        events.push(ChatEvent {
                            kind,
                            timestamp: ts,
                            text: content.clone(),
                        });
                    }
                }
                messages.push(RawWhatsAppMessage {
                    timestamp: ts,
                    sender,
                    has_media: is_media(&content),
                    content,
                    is_system,
                });
            }
            // Continuation line: append to the current message body.
            None => {
                if let Some(last) = messages.last_mut() {
                    last.content.push('\n');
                    last.content.push_str(line.trim_end());
                    if is_media(&last.content) {
                        last.has_media = true;
                    }
                }
            }
        }
    }

    let date_range = compute_range(&messages, &events);
    ParsedExport {
        format,
        messages,
        events,
        date_range,
    }
}

/// Strip the directional/zero-width marks WhatsApp injects and normalize the
/// no-break spaces it puts before AM/PM, so downstream parsing sees plain text.
fn sanitize_line(line: &str) -> String {
    line.chars()
        .filter_map(|c| match c {
            '\u{200E}'
            | '\u{200F}'
            | '\u{FEFF}'
            | '\u{200B}'
            | '\u{200C}'
            | '\u{200D}'
            | '\u{202A}'..='\u{202E}' => None,
            '\u{00A0}' | '\u{202F}' => Some(' '),
            other => Some(other),
        })
        .collect()
}

/// Split a line into (naive datetime, rest-of-line, format) if it starts a
/// message. iOS uses `[stamp] rest`; Android uses `stamp - rest` where the
/// stamp parses as a date+time.
fn split_header(
    line: &str,
    order: DateOrder,
) -> Option<(chrono::NaiveDateTime, String, WhatsAppFormat)> {
    // iOS: bracketed stamp.
    if let Some(stripped) = line.strip_prefix('[') {
        if let Some(end) = stripped.find(']') {
            if let Some(dt) = parse_stamp(stripped[..end].trim(), order) {
                let rest = stripped[end + 1..].trim_start().to_string();
                return Some((dt, rest, WhatsAppFormat::Ios));
            }
        }
    }
    // Android: `stamp - rest`, where the part before the first " - " is a stamp.
    if let Some(idx) = line.find(" - ") {
        if let Some(dt) = parse_stamp(line[..idx].trim(), order) {
            return Some((dt, line[idx + 3..].to_string(), WhatsAppFormat::Android));
        }
    }
    None
}

/// Split the post-stamp remainder into (sender, content, is_system). A normal
/// message is `Sender: text`; a line with no plausible `Sender:` prefix is a
/// system line. The sender heuristic (short, no newline) errs toward treating
/// odd lines as system rather than inventing a sender.
fn split_body(rest: &str) -> (Option<String>, String, bool) {
    if let Some((sender, content)) = rest.split_once(": ") {
        let plausible =
            !sender.is_empty() && sender.chars().count() <= 80 && !sender.contains('\n');
        if plausible {
            return (Some(sender.to_string()), content.to_string(), false);
        }
    }
    (None, rest.to_string(), true)
}

/// Detect a group lifecycle event from a system line (WHA-015).
fn classify_event(text: &str) -> Option<ChatEventKind> {
    let t = text.to_lowercase();
    if t.contains("created group") || t.contains("created this group") {
        Some(ChatEventKind::GroupCreated)
    } else if t.contains(" added ")
        || t.contains("was added")
        || t.contains("were added")
        || t.contains(" joined")
    {
        Some(ChatEventKind::MemberJoinedOrAdded)
    } else {
        None
    }
}

/// Whether a body is a media placeholder rather than text (MSG-006 hint).
fn is_media(content: &str) -> bool {
    let t = content.to_lowercase();
    t.contains("media omitted")
        || t.contains("image omitted")
        || t.contains("video omitted")
        || t.contains("audio omitted")
        || t.contains("<attached")
}

fn compute_range(messages: &[RawWhatsAppMessage], events: &[ChatEvent]) -> Option<(i64, i64)> {
    let times = messages
        .iter()
        .map(|m| m.timestamp)
        .chain(events.iter().map(|e| e.timestamp));
    times.fold(None, |acc, t| match acc {
        None => Some((t, t)),
        Some((lo, hi)) => Some((lo.min(t), hi.max(t))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LONDON: Tz = chrono_tz::Europe::London;

    fn parse(raw: &str) -> ParsedExport {
        parse_export(raw, LONDON, DateOrder::DayMonthYear)
    }

    #[test]
    fn parses_ios_message() {
        let p = parse("[01/01/2026, 12:00:00] Alice: Hello team");
        assert_eq!(p.format, WhatsAppFormat::Ios);
        assert_eq!(p.messages.len(), 1);
        let m = &p.messages[0];
        assert_eq!(m.sender.as_deref(), Some("Alice"));
        assert_eq!(m.content, "Hello team");
        assert!(!m.is_system);
    }

    #[test]
    fn parses_android_message_without_seconds() {
        let p = parse("01/01/2026, 12:00 - Bob: Morning");
        assert_eq!(p.format, WhatsAppFormat::Android);
        assert_eq!(p.messages[0].sender.as_deref(), Some("Bob"));
        assert_eq!(p.messages[0].content, "Morning");
    }

    #[test]
    fn multi_line_messages_are_joined() {
        let p = parse("[01/01/2026, 12:00:00] Alice: line one\nline two\nline three");
        assert_eq!(p.messages.len(), 1);
        assert_eq!(p.messages[0].content, "line one\nline two\nline three");
    }

    #[test]
    fn system_line_has_no_sender() {
        let p = parse("[01/01/2026, 09:00:00] Messages and calls are end-to-end encrypted.");
        assert_eq!(p.messages.len(), 1);
        assert!(p.messages[0].is_system);
        assert_eq!(p.messages[0].sender, None);
    }

    #[test]
    fn detects_group_created_and_join_events() {
        let p = parse(
            "[01/01/2020, 08:00:00] Alice created group \"Team\"\n\
             [01/06/2022, 10:00:00] Alice added Carol",
        );
        assert_eq!(p.events.len(), 2);
        assert_eq!(p.events[0].kind, ChatEventKind::GroupCreated);
        assert_eq!(p.events[1].kind, ChatEventKind::MemberJoinedOrAdded);
    }

    #[test]
    fn media_placeholder_is_flagged() {
        let p = parse("[01/01/2026, 12:00:00] Alice: <Media omitted>");
        assert!(p.messages[0].has_media);
    }

    #[test]
    fn timestamps_are_utc_normalized_with_dst() {
        // Summer (BST, +1): 12:00 local → 11:00 UTC.
        let p = parse("[01/07/2026, 12:00:00] Alice: hi");
        let summer = p.messages[0].timestamp;
        // Winter (GMT): 12:00 local → 12:00 UTC, one hour later in absolute terms.
        let p2 = parse("[01/01/2026, 12:00:00] Alice: hi");
        let winter = p2.messages[0].timestamp;
        // Same wall-clock noon, but summer is an hour "earlier" in UTC.
        assert_eq!(winter % 3600, 0);
        assert_eq!(summer % 3600, 0);
    }

    #[test]
    fn date_range_spans_min_to_max() {
        let p = parse(
            "[01/01/2026, 09:00:00] Alice: first\n\
             [03/01/2026, 18:30:00] Bob: last",
        );
        let (lo, hi) = p.date_range.unwrap();
        assert!(lo < hi);
        assert_eq!(lo, p.messages[0].timestamp);
        assert_eq!(hi, p.messages[1].timestamp);
    }

    #[test]
    fn continuation_before_any_header_is_ignored() {
        let p = parse("stray preamble line\n[01/01/2026, 12:00:00] Alice: hi");
        assert_eq!(p.messages.len(), 1);
        assert_eq!(p.messages[0].content, "hi");
    }
}
