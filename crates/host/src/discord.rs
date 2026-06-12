//! Discord live ingestion (ADR-128) — a [`PlatformFetcher`] over the Discord
//! REST API.
//!
//! The pure layer (snowflake → timestamp, the message-JSON → [`NormalizedMessage`]
//! mapping, the text-channel filter) is compiled in every build and unit-tested
//! offline. Only [`DiscordFetcher`] (the blocking `ureq` calls) is gated behind
//! the `discord` feature, so the default build stays network-free — exactly like
//! the HTTP LLM client and the sink plugins (ADR-125/126).
//!
//! A fetch normalizes messages and the caller persists them via the message store
//! (`save_message`, idempotent on the native id), so the existing summarize /
//! schedule path consumes them unchanged — ingestion, not a second pipeline.

use domain::{Attachment, AttachmentKind, ChannelId, MessageId, NormalizedMessage, Platform};

/// Discord's epoch (2015-01-01T00:00:00Z) in milliseconds — snowflake ids encode
/// `(unix_ms - DISCORD_EPOCH_MS) << 22`.
const DISCORD_EPOCH_MS: i64 = 1_420_070_400_000;

/// Decode the creation time (unix **seconds**, UTC) from a Discord snowflake id.
/// Returns `None` if `id` isn't a valid u64. This is how we timestamp messages
/// and order pagination without parsing ISO-8601.
pub fn snowflake_to_unix_secs(id: &str) -> Option<i64> {
    let raw: u64 = id.parse().ok()?;
    let ms = ((raw >> 22) as i64) + DISCORD_EPOCH_MS;
    Some(ms / 1000)
}

/// Map a Discord attachment `content_type` to the coarse [`AttachmentKind`]
/// (MSG-006). Unknown/absent types are `Other`.
fn attachment_kind(content_type: Option<&str>) -> AttachmentKind {
    match content_type {
        Some(ct) if ct.starts_with("image/") => AttachmentKind::Image,
        Some(ct) if ct.starts_with("video/") => AttachmentKind::Video,
        Some(ct) if ct.starts_with("audio/") => AttachmentKind::Audio,
        Some(ct) if ct.starts_with("application/") || ct.starts_with("text/") => {
            AttachmentKind::Document
        }
        _ => AttachmentKind::Other,
    }
}

/// Discord message types that are ordinary user content (default + reply). Every
/// other type (member joins, pins, boosts, …) is treated as a system message and
/// filtered by `is_substantial()`.
fn is_user_message_type(t: i64) -> bool {
    t == 0 || t == 19
}

/// Whether a Discord channel object is a text channel we can read: `GUILD_TEXT`
/// (0) or `GUILD_ANNOUNCEMENT` (5).
pub fn is_text_channel(channel: &serde_json::Value) -> bool {
    matches!(
        channel.get("type").and_then(|t| t.as_i64()),
        Some(0) | Some(5)
    )
}

/// Parse one Discord message object into a [`NormalizedMessage`]. Returns `None`
/// if it lacks the identifiers we require (`id`, `channel_id`). Timestamp comes
/// from the snowflake; the author's display name prefers `global_name`, then
/// `username`. Webhook/bot authorship is preserved (the summarizer can still use
/// it); only Discord *system* message types are flagged `is_system`.
pub fn parse_message(v: &serde_json::Value) -> Option<NormalizedMessage> {
    let id = MessageId::parse(v.get("id")?.as_str()?).ok()?;
    let channel_id = ChannelId::parse(v.get("channel_id")?.as_str()?).ok()?;
    let timestamp = snowflake_to_unix_secs(id.as_str())?;

    let author = v.get("author");
    let author_id = author
        .and_then(|a| a.get("id"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let author_name = author
        .and_then(|a| {
            a.get("global_name")
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .or_else(|| a.get("username").and_then(|x| x.as_str()))
        })
        .unwrap_or("unknown")
        .to_string();

    let content = v
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();

    let msg_type = v.get("type").and_then(|t| t.as_i64()).unwrap_or(0);
    let is_system = !is_user_message_type(msg_type);

    let reply_to = v
        .get("referenced_message")
        .and_then(|r| r.get("id"))
        .or_else(|| v.get("message_reference").and_then(|r| r.get("message_id")))
        .and_then(|x| x.as_str())
        .and_then(|s| MessageId::parse(s).ok());

    let attachments = v
        .get("attachments")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .map(|att| Attachment {
                    kind: attachment_kind(att.get("content_type").and_then(|c| c.as_str())),
                    filename: att
                        .get("filename")
                        .and_then(|f| f.as_str())
                        .map(str::to_string),
                })
                .collect()
        })
        .unwrap_or_default();

    Some(NormalizedMessage {
        id,
        platform: Platform::Discord,
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

#[cfg(feature = "discord")]
pub use live::DiscordFetcher;

#[cfg(feature = "discord")]
mod live {
    use super::*;
    use crate::platform::{FetchError, FetchResult, FetchScope, PlatformContext, PlatformFetcher};
    use domain::Secret;

    const API_BASE: &str = "https://discord.com/api/v10";
    /// Page size for the messages endpoint (Discord max is 100).
    const PAGE_LIMIT: usize = 100;
    /// Cap pages per channel per fetch, so backfilling a huge channel can't run
    /// away (v1; 10 pages = up to 1000 messages).
    const MAX_PAGES: usize = 10;

    /// A Discord REST fetcher scoped to one guild, authenticated with a bot token.
    pub struct DiscordFetcher {
        token: Secret<String>,
        guild_id: String,
        agent: ureq::Agent,
    }

    impl DiscordFetcher {
        pub fn new(token: impl Into<String>, guild_id: impl Into<String>) -> Self {
            Self {
                token: Secret::new(token.into()),
                guild_id: guild_id.into(),
                agent: ureq::AgentBuilder::new()
                    .timeout(std::time::Duration::from_secs(30))
                    .build(),
            }
        }

        /// One authenticated GET returning parsed JSON. Honors a single `429`
        /// `retry-after` before giving up (v1 bounded backoff).
        fn get(&self, url: &str) -> Result<serde_json::Value, String> {
            for attempt in 0..2 {
                let req = self.agent.get(url).set(
                    "Authorization",
                    &format!("Bot {}", self.token.expose_secret()),
                );
                match req.call() {
                    Ok(resp) => {
                        return resp.into_json().map_err(|e| format!("json: {e}"));
                    }
                    Err(ureq::Error::Status(429, resp)) if attempt == 0 => {
                        let wait = resp
                            .header("retry-after")
                            .and_then(|h| h.parse::<f64>().ok())
                            .unwrap_or(1.0)
                            .min(5.0);
                        std::thread::sleep(std::time::Duration::from_millis(
                            (wait * 1000.0) as u64,
                        ));
                    }
                    Err(ureq::Error::Status(code, _)) => return Err(format!("http {code}")),
                    Err(ureq::Error::Transport(t)) => return Err(format!("transport: {t}")),
                }
            }
            Err("rate limited".to_string())
        }

        /// List the guild's text channels as `(id, name)`.
        fn guild_text_channels(&self) -> Result<Vec<(ChannelId, String)>, String> {
            let url = format!("{API_BASE}/guilds/{}/channels", self.guild_id);
            let arr = self.get(&url)?;
            let chans = arr.as_array().ok_or("channels: expected array")?;
            Ok(chans
                .iter()
                .filter(|c| is_text_channel(c))
                .filter_map(|c| {
                    let id = ChannelId::parse(c.get("id")?.as_str()?).ok()?;
                    let name = c
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some((id, name))
                })
                .collect())
        }

        /// Fetch one channel's messages in `[start, end]`, paging backwards from
        /// newest until older than `start` (or the page cap).
        fn channel_messages(
            &self,
            channel: &ChannelId,
            start: i64,
            end: i64,
        ) -> Result<Vec<NormalizedMessage>, String> {
            let mut out = Vec::new();
            let mut before: Option<String> = None;
            for _ in 0..MAX_PAGES {
                let mut url = format!(
                    "{API_BASE}/channels/{}/messages?limit={PAGE_LIMIT}",
                    channel.as_str()
                );
                if let Some(b) = &before {
                    url.push_str(&format!("&before={b}"));
                }
                let page = self.get(&url)?;
                let msgs = page.as_array().ok_or("messages: expected array")?;
                if msgs.is_empty() {
                    break;
                }
                // Discord returns newest-first; remember the oldest id for the
                // next page.
                let oldest_id = msgs
                    .last()
                    .and_then(|m| m.get("id"))
                    .and_then(|x| x.as_str())
                    .map(str::to_string);
                let mut reached_older = false;
                for m in msgs {
                    if let Some(nm) = parse_message(m) {
                        if nm.timestamp < start {
                            reached_older = true;
                        } else if nm.timestamp <= end {
                            out.push(nm);
                        }
                    }
                }
                if reached_older || msgs.len() < PAGE_LIMIT {
                    break;
                }
                before = oldest_id;
                if before.is_none() {
                    break;
                }
            }
            // Oldest-first, matching the message store's contract.
            out.sort_by_key(|m| m.timestamp);
            Ok(out)
        }
    }

    impl PlatformFetcher for DiscordFetcher {
        fn platform(&self) -> Platform {
            Platform::Discord
        }

        fn resolve_channels(&self, scope: &FetchScope) -> Result<Vec<ChannelId>, FetchError> {
            let err = |message: String| FetchError {
                channel: ChannelId::parse("guild").unwrap(),
                message,
            };
            match scope {
                FetchScope::Channels(c) => Ok(c.clone()),
                FetchScope::Workspace => Ok(self
                    .guild_text_channels()
                    .map_err(err)?
                    .into_iter()
                    .map(|(id, _)| id)
                    .collect()),
                FetchScope::Category(cat) => {
                    let url = format!("{API_BASE}/guilds/{}/channels", self.guild_id);
                    let arr = self.get(&url).map_err(err)?;
                    let chans = arr.as_array().ok_or_else(|| err("expected array".into()))?;
                    Ok(chans
                        .iter()
                        .filter(|c| is_text_channel(c))
                        .filter(|c| c.get("parent_id").and_then(|p| p.as_str()) == Some(cat))
                        .filter_map(|c| ChannelId::parse(c.get("id")?.as_str()?).ok())
                        .collect())
                }
            }
        }

        fn fetch_messages(&self, channels: &[ChannelId], start: i64, end: i64) -> FetchResult {
            let mut result = FetchResult::default();
            for channel in channels {
                match self.channel_messages(channel, start, end) {
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
            let server_name = self
                .get(&format!("{API_BASE}/guilds/{}", self.guild_id))
                .ok()
                .and_then(|g| g.get("name").and_then(|n| n.as_str()).map(str::to_string))
                .unwrap_or_else(|| self.guild_id.clone());
            let primary_channel_name = channels
                .first()
                .and_then(|c| {
                    self.get(&format!("{API_BASE}/channels/{}", c.as_str()))
                        .ok()
                        .and_then(|ch| ch.get("name").and_then(|n| n.as_str()).map(str::to_string))
                })
                .unwrap_or_default();
            PlatformContext {
                platform: Platform::Discord,
                server_name,
                primary_channel_name,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn snowflake_decodes_to_known_time() {
        // Snowflake 175928847299117063 → 2016-04-30T11:18:25.796Z ≈ 1462015105s.
        let ts = snowflake_to_unix_secs("175928847299117063").unwrap();
        assert_eq!(ts, 1_462_015_105);
        // The Discord epoch itself (snowflake 0) is 2015-01-01.
        assert_eq!(snowflake_to_unix_secs("0").unwrap(), 1_420_070_400);
        assert!(snowflake_to_unix_secs("not-a-number").is_none());
    }

    #[test]
    fn text_channel_filter() {
        assert!(is_text_channel(&json!({"type": 0})));
        assert!(is_text_channel(&json!({"type": 5}))); // announcement
        assert!(!is_text_channel(&json!({"type": 2}))); // voice
        assert!(!is_text_channel(&json!({"type": 4}))); // category
    }

    #[test]
    fn parses_a_normal_message() {
        let v = json!({
            "id": "1110000000000000000",
            "channel_id": "c-42",
            "type": 0,
            "content": "Let's plan the launch for Friday",
            "author": { "id": "u1", "global_name": "Alice", "username": "alice123" },
            "attachments": []
        });
        let m = parse_message(&v).unwrap();
        assert_eq!(m.platform, Platform::Discord);
        assert_eq!(m.channel_id.as_str(), "c-42");
        assert_eq!(m.author_name, "Alice"); // prefers global_name
        assert_eq!(m.author_id, "u1");
        assert!(!m.is_system);
        assert!(m.is_substantial());
        assert!(m.timestamp > 1_420_070_400);
    }

    #[test]
    fn falls_back_to_username_and_flags_system_types() {
        let v = json!({
            "id": "1110000000000000001",
            "channel_id": "c-42",
            "type": 7, // GUILD_MEMBER_JOIN → system
            "content": "",
            "author": { "id": "u2", "username": "bob" }
        });
        let m = parse_message(&v).unwrap();
        assert_eq!(m.author_name, "bob");
        assert!(m.is_system);
        assert!(!m.is_substantial()); // system messages aren't summarizable
    }

    #[test]
    fn captures_reply_and_attachments() {
        let v = json!({
            "id": "1110000000000000002",
            "channel_id": "c-42",
            "type": 19, // reply
            "content": "see the mockup",
            "author": { "id": "u3", "username": "carol" },
            "referenced_message": { "id": "1110000000000000000" },
            "attachments": [
                { "filename": "hero.png", "content_type": "image/png" },
                { "filename": "spec.pdf", "content_type": "application/pdf" }
            ]
        });
        let m = parse_message(&v).unwrap();
        assert_eq!(m.reply_to.as_ref().unwrap().as_str(), "1110000000000000000");
        assert_eq!(m.attachments.len(), 2);
        assert_eq!(m.attachments[0].kind, AttachmentKind::Image);
        assert_eq!(m.attachments[1].kind, AttachmentKind::Document);
        assert_eq!(m.attachments[0].filename.as_deref(), Some("hero.png"));
    }

    #[test]
    fn missing_identifiers_yield_none() {
        assert!(parse_message(&json!({ "content": "no id" })).is_none());
        assert!(parse_message(&json!({ "id": "123", "content": "no channel" })).is_none());
    }
}
