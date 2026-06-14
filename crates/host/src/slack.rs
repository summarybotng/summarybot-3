//! Slack live ingestion (ADR-128) — a [`PlatformFetcher`] over the Slack Web API.
//!
//! Mirrors [`crate::discord`]: the pure layer (Slack `ts` → unix seconds, the
//! message-JSON → [`NormalizedMessage`] mapping, the system-subtype filter) is
//! compiled and unit-tested in every build; only [`SlackFetcher`] (the blocking
//! `ureq` calls) is gated behind the `slack` feature. A fetch normalizes messages
//! and the caller persists them via the message store, so the existing summarize
//! / schedule path consumes them unchanged.
//!
//! Unlike Discord, a Slack bot token is workspace-scoped — channels are listed
//! with `conversations.list` (no team id needed), and history needs the bot to be
//! a member of the channel (`not_in_channel` otherwise, surfaced per-channel).

use domain::{Attachment, AttachmentKind, ChannelId, MessageId, NormalizedMessage, Platform};

/// Decode a Slack message `ts` ("1700000000.000100") to unix **seconds** — the
/// integer part. Slack `ts` is also the per-channel message identity. Returns
/// `None` if the integer part isn't parseable.
pub fn slack_ts_to_unix_secs(ts: &str) -> Option<i64> {
    ts.split('.').next()?.parse().ok()
}

/// Map a Slack file `mimetype` to the coarse [`AttachmentKind`] (MSG-006).
fn attachment_kind(mimetype: Option<&str>) -> AttachmentKind {
    match mimetype {
        Some(m) if m.starts_with("image/") => AttachmentKind::Image,
        Some(m) if m.starts_with("video/") => AttachmentKind::Video,
        Some(m) if m.starts_with("audio/") => AttachmentKind::Audio,
        Some(m) if m.starts_with("application/") || m.starts_with("text/") => {
            AttachmentKind::Document
        }
        _ => AttachmentKind::Other,
    }
}

/// Slack message `subtype`s that are channel events, not user content (joins,
/// topic changes, etc.) — flagged `is_system` and dropped by `is_substantial()`.
/// `bot_message`, `thread_broadcast`, `me_message`, `file_share` are real content.
fn is_system_subtype(subtype: &str) -> bool {
    matches!(
        subtype,
        "channel_join"
            | "channel_leave"
            | "channel_topic"
            | "channel_purpose"
            | "channel_name"
            | "channel_archive"
            | "channel_unarchive"
            | "group_join"
            | "group_leave"
            | "pinned_item"
            | "unpinned_item"
    )
}

/// Parse one Slack message object into a [`NormalizedMessage`], scoped to its
/// `channel` (Slack ids are per-channel `ts`, so we key the [`MessageId`] on
/// `slack-{channel}-{ts}` for global uniqueness + idempotent re-ingest). Returns
/// `None` for non-`message` events or a missing `ts`.
pub fn parse_message(v: &serde_json::Value, channel: &str) -> Option<NormalizedMessage> {
    if v.get("type").and_then(|t| t.as_str()) != Some("message") {
        return None;
    }
    let ts = v.get("ts")?.as_str()?;
    let timestamp = slack_ts_to_unix_secs(ts)?;
    let channel_id = ChannelId::parse(channel).ok()?;
    let id = MessageId::parse(format!("slack-{channel}-{ts}")).ok()?;

    let author_id = v
        .get("user")
        .or_else(|| v.get("bot_id"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    // Slack doesn't inline display names; `username` is set for bot/webhook posts,
    // otherwise fall back to the user id (cosmetic — name resolution is a refinement).
    let author_name = v
        .get("username")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            if author_id.is_empty() {
                "unknown".to_string()
            } else {
                author_id.clone()
            }
        });

    let content = v
        .get("text")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();

    let is_system = v
        .get("subtype")
        .and_then(|s| s.as_str())
        .map(is_system_subtype)
        .unwrap_or(false);

    // A threaded reply carries `thread_ts` of its parent (≠ its own `ts`).
    let reply_to = v
        .get("thread_ts")
        .and_then(|t| t.as_str())
        .filter(|t| *t != ts)
        .and_then(|t| MessageId::parse(format!("slack-{channel}-{t}")).ok());

    let attachments = v
        .get("files")
        .and_then(|f| f.as_array())
        .map(|arr| {
            arr.iter()
                .map(|file| Attachment {
                    kind: attachment_kind(file.get("mimetype").and_then(|m| m.as_str())),
                    filename: file
                        .get("name")
                        .and_then(|n| n.as_str())
                        .map(str::to_string),
                })
                .collect()
        })
        .unwrap_or_default();

    Some(NormalizedMessage {
        id,
        platform: Platform::Slack,
        channel_id,
        author_id,
        author_name,
        content,
        timestamp,
        is_system,
        reply_to,
        attachments,
    })
}

#[cfg(feature = "slack")]
pub use live::SlackFetcher;

#[cfg(feature = "slack")]
mod live {
    use super::*;
    use crate::platform::{
        ChannelInfo, FetchError, FetchResult, FetchScope, PlatformContext, PlatformFetcher,
    };
    use domain::Secret;

    const API_BASE: &str = "https://slack.com/api";
    /// Page size for `conversations.history` (Slack max is 1000; keep modest).
    const PAGE_LIMIT: usize = 200;
    /// Cap pages per channel per fetch (v1 backfill bound).
    const MAX_PAGES: usize = 10;

    /// A Slack Web API fetcher authenticated with a bot token (`xoxb-…`). The
    /// token is workspace-scoped, so there's no team/guild id.
    pub struct SlackFetcher {
        token: Secret<String>,
        agent: ureq::Agent,
    }

    impl SlackFetcher {
        pub fn new(token: impl Into<String>) -> Self {
            Self {
                token: Secret::new(token.into()),
                agent: ureq::AgentBuilder::new()
                    .timeout(std::time::Duration::from_secs(30))
                    .build(),
            }
        }

        /// One authenticated GET. Slack signals failure in the JSON body
        /// (`{"ok": false, "error": "..."}`), not the HTTP status, so we check it.
        fn get(&self, url: &str) -> Result<serde_json::Value, String> {
            let resp = self
                .agent
                .get(url)
                .set(
                    "Authorization",
                    &format!("Bearer {}", self.token.expose_secret()),
                )
                .call();
            let body: serde_json::Value = match resp {
                Ok(r) => r.into_json().map_err(|e| format!("json: {e}"))?,
                Err(ureq::Error::Status(code, _)) => return Err(format!("http {code}")),
                Err(ureq::Error::Transport(t)) => return Err(format!("transport: {t}")),
            };
            if body.get("ok").and_then(|o| o.as_bool()) != Some(true) {
                let err = body
                    .get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("unknown");
                return Err(format!("slack: {err}"));
            }
            Ok(body)
        }

        /// List the workspace's public + private channels as ids.
        fn list_channels(&self) -> Result<Vec<ChannelId>, String> {
            let mut out = Vec::new();
            let mut cursor: Option<String> = None;
            for _ in 0..MAX_PAGES {
                let mut url = format!(
                    "{API_BASE}/conversations.list?types=public_channel,private_channel&limit=200"
                );
                if let Some(c) = &cursor {
                    url.push_str(&format!("&cursor={c}"));
                }
                let body = self.get(&url)?;
                if let Some(chans) = body.get("channels").and_then(|c| c.as_array()) {
                    for c in chans {
                        if let Some(id) = c.get("id").and_then(|i| i.as_str()) {
                            if let Ok(cid) = ChannelId::parse(id) {
                                out.push(cid);
                            }
                        }
                    }
                }
                cursor = body
                    .get("response_metadata")
                    .and_then(|m| m.get("next_cursor"))
                    .and_then(|c| c.as_str())
                    .filter(|c| !c.is_empty())
                    .map(str::to_string);
                if cursor.is_none() {
                    break;
                }
            }
            Ok(out)
        }

        /// Fetch one channel's messages in `[start, end]` (Slack `oldest`/`latest`
        /// are unix-second strings), paging on `next_cursor`.
        fn channel_history(
            &self,
            channel: &ChannelId,
            start: i64,
            end: i64,
        ) -> Result<Vec<NormalizedMessage>, String> {
            let mut out = Vec::new();
            let mut cursor: Option<String> = None;
            for _ in 0..MAX_PAGES {
                let mut url = format!(
                    "{API_BASE}/conversations.history?channel={}&oldest={start}&latest={end}&limit={PAGE_LIMIT}",
                    channel.as_str()
                );
                if let Some(c) = &cursor {
                    url.push_str(&format!("&cursor={c}"));
                }
                let body = self.get(&url)?;
                if let Some(msgs) = body.get("messages").and_then(|m| m.as_array()) {
                    for m in msgs {
                        if let Some(nm) = parse_message(m, channel.as_str()) {
                            out.push(nm);
                        }
                    }
                }
                cursor = body
                    .get("response_metadata")
                    .and_then(|m| m.get("next_cursor"))
                    .and_then(|c| c.as_str())
                    .filter(|c| !c.is_empty())
                    .map(str::to_string);
                if cursor.is_none() {
                    break;
                }
            }
            out.sort_by_key(|m| m.timestamp);
            Ok(out)
        }
    }

    impl PlatformFetcher for SlackFetcher {
        fn platform(&self) -> Platform {
            Platform::Slack
        }

        fn resolve_channels(&self, scope: &FetchScope) -> Result<Vec<ChannelId>, FetchError> {
            let err = |message: String| FetchError {
                channel: ChannelId::parse("workspace").unwrap(),
                message,
            };
            match scope {
                FetchScope::Channels(c) => Ok(c.clone()),
                FetchScope::Workspace => self.list_channels().map_err(err),
                FetchScope::Category(_) => Err(err("category scope unsupported on Slack".into())),
            }
        }

        fn fetch_messages(&self, channels: &[ChannelId], start: i64, end: i64) -> FetchResult {
            let mut result = FetchResult::default();
            for channel in channels {
                match self.channel_history(channel, start, end) {
                    Ok(mut msgs) => result.messages.append(&mut msgs),
                    Err(message) => result.errors.push(FetchError {
                        channel: channel.clone(),
                        message,
                    }),
                }
            }
            result
        }

        fn context(&self, channels: &[ChannelId]) -> PlatformContext {
            let primary_channel_name = channels
                .first()
                .and_then(|c| {
                    self.get(&format!(
                        "{API_BASE}/conversations.info?channel={}",
                        c.as_str()
                    ))
                    .ok()
                    .and_then(|b| {
                        b.get("channel")
                            .and_then(|ch| ch.get("name"))
                            .and_then(|n| n.as_str())
                            .map(str::to_string)
                    })
                })
                .unwrap_or_default();
            PlatformContext {
                platform: Platform::Slack,
                server_name: "Slack".to_string(),
                primary_channel_name,
            }
        }

        fn channel_directory(&self) -> Result<Vec<ChannelInfo>, String> {
            // Slack has no categories — a flat list of channels with their names.
            let mut out = Vec::new();
            let mut cursor: Option<String> = None;
            for _ in 0..MAX_PAGES {
                let mut url = format!(
                    "{API_BASE}/conversations.list?types=public_channel,private_channel&limit=200"
                );
                if let Some(c) = &cursor {
                    url.push_str(&format!("&cursor={c}"));
                }
                let body = self.get(&url)?;
                if let Some(chans) = body.get("channels").and_then(|c| c.as_array()) {
                    for c in chans {
                        if let Some(id) = c.get("id").and_then(|i| i.as_str()) {
                            if let Ok(cid) = ChannelId::parse(id) {
                                out.push(ChannelInfo {
                                    id: cid,
                                    name: c
                                        .get("name")
                                        .and_then(|n| n.as_str())
                                        .unwrap_or("")
                                        .to_string(),
                                    category: None,
                                });
                            }
                        }
                    }
                }
                cursor = body
                    .get("response_metadata")
                    .and_then(|m| m.get("next_cursor"))
                    .and_then(|c| c.as_str())
                    .filter(|c| !c.is_empty())
                    .map(str::to_string);
                if cursor.is_none() {
                    break;
                }
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ts_decodes_to_integer_seconds() {
        assert_eq!(
            slack_ts_to_unix_secs("1700000000.000100"),
            Some(1_700_000_000)
        );
        assert_eq!(slack_ts_to_unix_secs("1234567890"), Some(1_234_567_890));
        assert!(slack_ts_to_unix_secs("not-a-ts").is_none());
    }

    #[test]
    fn parses_a_normal_message() {
        let v = json!({
            "type": "message",
            "user": "U123",
            "text": "let's ship the release on Friday",
            "ts": "1700000000.000100",
        });
        let m = parse_message(&v, "C42").unwrap();
        assert_eq!(m.platform, Platform::Slack);
        assert_eq!(m.channel_id.as_str(), "C42");
        assert_eq!(m.id.as_str(), "slack-C42-1700000000.000100");
        assert_eq!(m.author_id, "U123");
        assert_eq!(m.author_name, "U123"); // no inline name; falls back to id
        assert!(!m.is_system);
        assert!(m.is_substantial());
        assert_eq!(m.timestamp, 1_700_000_000);
    }

    #[test]
    fn flags_system_subtype_and_skips_non_messages() {
        let join = json!({
            "type": "message",
            "subtype": "channel_join",
            "user": "U1",
            "text": "has joined the channel",
            "ts": "1700000001.000000",
        });
        let m = parse_message(&join, "C42").unwrap();
        assert!(m.is_system);
        assert!(!m.is_substantial());

        // Non-message events are dropped entirely.
        assert!(parse_message(&json!({ "type": "reaction_added", "ts": "1.0" }), "C42").is_none());
    }

    #[test]
    fn captures_thread_reply_and_files() {
        let v = json!({
            "type": "message",
            "user": "U9",
            "text": "see attached",
            "ts": "1700000050.000200",
            "thread_ts": "1700000000.000100",
            "files": [{ "name": "spec.pdf", "mimetype": "application/pdf" }],
        });
        let m = parse_message(&v, "C42").unwrap();
        assert_eq!(
            m.reply_to.as_ref().unwrap().as_str(),
            "slack-C42-1700000000.000100"
        );
        assert_eq!(m.attachments.len(), 1);
        assert_eq!(m.attachments[0].kind, AttachmentKind::Document);
    }

    #[test]
    fn bot_message_uses_username() {
        let v = json!({
            "type": "message",
            "subtype": "bot_message",
            "username": "CI Bot",
            "bot_id": "B1",
            "text": "build passed",
            "ts": "1700000099.000000",
        });
        let m = parse_message(&v, "C42").unwrap();
        assert_eq!(m.author_name, "CI Bot");
        assert!(!m.is_system); // bot_message is real content
    }
}
