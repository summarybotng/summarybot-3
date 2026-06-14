//! Platform message-fetcher abstraction (ADR-051; PRD §12.3, WSP-006).
//!
//! The contract every **live** platform (Discord, Slack) implements so the rest
//! of the system is platform-blind: resolve a scope to channels, fetch a time
//! range as [`NormalizedMessage`]s, and describe the source. Concrete adapters
//! do real network I/O and land with the Phase 5 async runtime; this defines the
//! shape and a fake that lets the surrounding pipeline be tested now. The trait
//! is intentionally synchronous to match the current codebase — it will move to
//! `async` when the server runtime arrives.
//!
//! WhatsApp is **not** a `PlatformFetcher`: it is push-only/upload-only (§13.2),
//! so its path is the ADR-121 parser + ingestor, not a live fetch.

use domain::{ChannelId, NormalizedMessage, Platform};

/// Which channels a request targets (ADR-011 / SCP-*).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchScope {
    /// Specific channels.
    Channels(Vec<ChannelId>),
    /// All channels under a platform category (e.g. Discord category id).
    Category(String),
    /// All channels the bot can access in the workspace's connection.
    Workspace,
}

/// A browsable channel in a source's directory (WSP-006): its id, display name,
/// and the category it sits under (Discord), so the dashboard can present the
/// server's channels grouped by category for point-and-click selection. Slack and
/// uncategorized channels carry `category: None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelInfo {
    pub id: ChannelId,
    pub name: String,
    pub category: Option<String>,
}

/// Display context for summary headers (resolved names, never ids).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformContext {
    pub platform: Platform,
    pub server_name: String,
    pub primary_channel_name: String,
}

/// A per-channel failure. One unreachable channel degrades to a recorded error
/// rather than failing the whole fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchError {
    pub channel: ChannelId,
    pub message: String,
}

/// Outcome of a fetch: normalized messages plus any per-channel errors.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchResult {
    pub messages: Vec<NormalizedMessage>,
    pub errors: Vec<FetchError>,
}

impl FetchResult {
    /// The substantial messages only (MSG-008) — what the summarizer should see.
    pub fn substantial(&self) -> impl Iterator<Item = &NormalizedMessage> {
        self.messages.iter().filter(|m| m.is_substantial())
    }
}

/// A live-platform message source (ADR-051). Object-safe, so adapters compose in
/// a registry (`Box<dyn PlatformFetcher>`), exactly as identity providers do.
pub trait PlatformFetcher {
    fn platform(&self) -> Platform;

    /// Resolve a scope to concrete channel ids (ADR-011). `Category` is
    /// platform-specific (Discord); platforms without it return an error.
    fn resolve_channels(&self, scope: &FetchScope) -> Result<Vec<ChannelId>, FetchError>;

    /// Fetch messages in the inclusive UTC range `[start, end]`, already
    /// normalized to this platform's `NormalizedMessage`s.
    fn fetch_messages(&self, channels: &[ChannelId], start: i64, end: i64) -> FetchResult;

    /// Resolved display context for a set of channels.
    fn context(&self, channels: &[ChannelId]) -> PlatformContext;

    /// List the source's summarizable channels for browsing/selection (WSP-006),
    /// grouped by category where the platform has them (Discord). A network call;
    /// returns a directory the dashboard renders for point-and-click sync.
    fn channel_directory(&self) -> Result<Vec<ChannelInfo>, String>;
}

/// Build the live fetcher for a platform from a bot token (ADR-128). `scope_id`
/// is the platform's source scope: Discord needs a guild id; Slack ignores it
/// (the token is workspace-scoped). Returns an error if the platform isn't a
/// live source, or its feature isn't compiled into this build.
#[allow(unused_variables)]
pub fn make_platform_fetcher(
    platform: Platform,
    token: String,
    scope_id: Option<String>,
) -> Result<Box<dyn PlatformFetcher>, String> {
    match platform {
        Platform::Discord => {
            #[cfg(feature = "discord")]
            {
                let guild = scope_id.ok_or("Discord needs a guild_id")?;
                Ok(Box::new(crate::discord::DiscordFetcher::new(token, guild)))
            }
            #[cfg(not(feature = "discord"))]
            Err("Discord ingestion not compiled in (build with --features discord)".to_string())
        }
        Platform::Slack => {
            #[cfg(feature = "slack")]
            {
                Ok(Box::new(crate::slack::SlackFetcher::new(token)))
            }
            #[cfg(not(feature = "slack"))]
            Err("Slack ingestion not compiled in (build with --features slack)".to_string())
        }
        Platform::WhatsApp => Err("WhatsApp is upload-only, not a live fetch source".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{Attachment, AttachmentKind, MessageId};

    /// An in-memory fetcher that proves the abstraction and exercises the
    /// pipeline without a network. A real Discord/Slack adapter slots in here.
    struct FakeFetcher {
        platform: Platform,
        server: String,
        // (channel, message)
        store: Vec<(ChannelId, NormalizedMessage)>,
    }

    fn msg(id: &str, channel: &str, ts: i64, content: &str) -> NormalizedMessage {
        NormalizedMessage {
            id: MessageId::parse(id).unwrap(),
            platform: Platform::Discord,
            channel_id: ChannelId::parse(channel).unwrap(),
            author_id: "u1".into(),
            author_name: "Alice".into(),
            content: content.into(),
            timestamp: ts,
            is_system: false,
            reply_to: None,
            attachments: vec![],
        }
    }

    impl PlatformFetcher for FakeFetcher {
        fn platform(&self) -> Platform {
            self.platform
        }

        fn resolve_channels(&self, scope: &FetchScope) -> Result<Vec<ChannelId>, FetchError> {
            match scope {
                FetchScope::Channels(c) => Ok(c.clone()),
                FetchScope::Workspace => {
                    let mut chans: Vec<ChannelId> =
                        self.store.iter().map(|(c, _)| c.clone()).collect();
                    chans.dedup();
                    Ok(chans)
                }
                FetchScope::Category(_) => Err(FetchError {
                    channel: ChannelId::parse("category").unwrap(),
                    message: "category scope unsupported on this platform".into(),
                }),
            }
        }

        fn fetch_messages(&self, channels: &[ChannelId], start: i64, end: i64) -> FetchResult {
            let messages = self
                .store
                .iter()
                .filter(|(c, m)| channels.contains(c) && m.timestamp >= start && m.timestamp <= end)
                .map(|(_, m)| m.clone())
                .collect();
            FetchResult {
                messages,
                errors: vec![],
            }
        }

        fn context(&self, _channels: &[ChannelId]) -> PlatformContext {
            PlatformContext {
                platform: self.platform,
                server_name: self.server.clone(),
                primary_channel_name: "general".into(),
            }
        }

        fn channel_directory(&self) -> Result<Vec<ChannelInfo>, String> {
            let mut chans: Vec<ChannelInfo> = self
                .store
                .iter()
                .map(|(c, _)| ChannelInfo {
                    id: c.clone(),
                    name: c.as_str().to_string(),
                    category: None,
                })
                .collect();
            chans.dedup_by(|a, b| a.id == b.id);
            Ok(chans)
        }
    }

    fn fake() -> FakeFetcher {
        FakeFetcher {
            platform: Platform::Discord,
            server: "Acme".into(),
            store: vec![
                (
                    ChannelId::parse("c1").unwrap(),
                    msg("m1", "c1", 100, "Let's plan the launch"),
                ),
                (ChannelId::parse("c1").unwrap(), msg("m2", "c1", 200, "ok")), // trivial
                (
                    ChannelId::parse("c2").unwrap(),
                    msg("m3", "c2", 500, "Different channel topic"),
                ),
            ],
        }
    }

    #[test]
    fn fetches_within_range_and_channel() {
        let f = fake();
        let c1 = vec![ChannelId::parse("c1").unwrap()];
        let r = f.fetch_messages(&c1, 0, 300);
        assert_eq!(r.messages.len(), 2); // m1, m2 in c1 within range; m3 excluded
        assert!(r.errors.is_empty());
    }

    #[test]
    fn substantial_filters_trivial_messages() {
        let f = fake();
        let all = vec![
            ChannelId::parse("c1").unwrap(),
            ChannelId::parse("c2").unwrap(),
        ];
        let r = f.fetch_messages(&all, 0, 1000);
        assert_eq!(r.messages.len(), 3);
        // "ok" is trivial (MSG-008) → only 2 substantial.
        assert_eq!(r.substantial().count(), 2);
    }

    #[test]
    fn workspace_scope_resolves_all_channels_category_unsupported() {
        let f = fake();
        let chans = f.resolve_channels(&FetchScope::Workspace).unwrap();
        assert!(chans.contains(&ChannelId::parse("c1").unwrap()));
        assert!(chans.contains(&ChannelId::parse("c2").unwrap()));
        assert!(f
            .resolve_channels(&FetchScope::Category("cat".into()))
            .is_err());
    }

    #[test]
    fn composes_behind_dyn_trait() {
        // Proves the registry shape: heterogeneous fetchers, one interface.
        let fetchers: Vec<Box<dyn PlatformFetcher>> = vec![Box::new(fake())];
        assert_eq!(fetchers[0].platform(), Platform::Discord);
        assert_eq!(fetchers[0].context(&[]).server_name, "Acme");
    }

    #[test]
    fn attachments_survive_normalization() {
        // A message with an attachment is substantial even with empty text.
        let f = FakeFetcher {
            platform: Platform::Slack,
            server: "S".into(),
            store: vec![(ChannelId::parse("c1").unwrap(), {
                let mut m = msg("a1", "c1", 10, "");
                m.attachments.push(Attachment {
                    kind: AttachmentKind::Image,
                    filename: Some("p.png".into()),
                });
                m
            })],
        };
        let r = f.fetch_messages(&[ChannelId::parse("c1").unwrap()], 0, 100);
        assert_eq!(r.substantial().count(), 1);
    }
}
