//! Live-source ingestion helper (ADR-128) — fetch a platform's messages and
//! persist them into the message store.
//!
//! Shared by the on-demand connection sync (api) and the scheduled live sync
//! (the schedule runner), so both go through one tested path. Generic over the
//! [`PlatformFetcher`] (injectable, so it's unit-testable with a fake) and the
//! [`WhatsAppRepository`] message store. Persistence is idempotent on the native
//! message id, so overlapping windows converge rather than duplicate.

use crate::platform::{FetchScope, PlatformFetcher};
use domain::WorkspaceId;
use repository::WhatsAppRepository;

/// Outcome of a sync: which channels were read, how many messages were fetched
/// in the window, how many were newly stored, and any per-channel failures.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub channel_ids: Vec<String>,
    pub fetched: usize,
    pub stored: usize,
    pub errors: Vec<(String, String)>,
}

/// Resolve `scope` to channels, fetch `[start, end]`, and persist each message.
/// Returns a [`SyncReport`]. A resolve failure is the only hard error; per-channel
/// fetch failures are recorded in the report (one bad channel doesn't sink the
/// rest).
pub fn sync_into_store<R: WhatsAppRepository>(
    fetcher: &dyn PlatformFetcher,
    repo: &R,
    workspace: &WorkspaceId,
    scope: &FetchScope,
    start: i64,
    end: i64,
) -> anyhow::Result<SyncReport> {
    let channels = fetcher
        .resolve_channels(scope)
        .map_err(|e| anyhow::anyhow!("resolve channels: {}", e.message))?;
    let result = fetcher.fetch_messages(&channels, start, end);

    let mut stored = 0usize;
    for m in &result.messages {
        if repo.save_message(workspace, m)? {
            stored += 1;
        }
    }
    Ok(SyncReport {
        channel_ids: channels.iter().map(|c| c.as_str().to_string()).collect(),
        fetched: result.messages.len(),
        stored,
        errors: result
            .errors
            .into_iter()
            .map(|e| (e.channel.as_str().to_string(), e.message))
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{FetchError, FetchResult, PlatformContext};
    use domain::{ChannelId, MessageId, NormalizedMessage, Platform};
    use repository::SqliteRepository;

    /// An in-memory fetcher with a fixed store, for testing the helper.
    struct FakeFetcher {
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
            Platform::Discord
        }
        fn resolve_channels(&self, scope: &FetchScope) -> Result<Vec<ChannelId>, FetchError> {
            match scope {
                FetchScope::Channels(c) => Ok(c.clone()),
                FetchScope::Workspace => {
                    let mut v: Vec<ChannelId> = self.store.iter().map(|(c, _)| c.clone()).collect();
                    v.dedup();
                    Ok(v)
                }
                FetchScope::Category(_) => Err(FetchError {
                    channel: ChannelId::parse("cat").unwrap(),
                    message: "unsupported".into(),
                }),
            }
        }
        fn fetch_messages(&self, channels: &[ChannelId], start: i64, end: i64) -> FetchResult {
            FetchResult {
                messages: self
                    .store
                    .iter()
                    .filter(|(c, m)| {
                        channels.contains(c) && m.timestamp >= start && m.timestamp <= end
                    })
                    .map(|(_, m)| m.clone())
                    .collect(),
                errors: vec![],
            }
        }
        fn context(&self, _channels: &[ChannelId]) -> PlatformContext {
            PlatformContext {
                platform: Platform::Discord,
                server_name: "S".into(),
                primary_channel_name: "general".into(),
            }
        }
        fn channel_directory(&self) -> Result<Vec<crate::platform::ChannelInfo>, String> {
            Ok(self
                .store
                .iter()
                .map(|(c, _)| crate::platform::ChannelInfo {
                    id: c.clone(),
                    name: c.as_str().to_string(),
                    category: None,
                    category_id: None,
                })
                .collect())
        }
    }

    fn ws() -> WorkspaceId {
        WorkspaceId::parse("ws-1").unwrap()
    }

    #[test]
    fn fetches_in_window_and_persists_idempotently() {
        let repo = SqliteRepository::in_memory().unwrap();
        let fetcher = FakeFetcher {
            store: vec![
                (
                    ChannelId::parse("c1").unwrap(),
                    msg("m1", "c1", 100, "plan the launch"),
                ),
                (
                    ChannelId::parse("c1").unwrap(),
                    msg("m2", "c1", 500, "after the window"),
                ),
            ],
        };
        let scope = FetchScope::Channels(vec![ChannelId::parse("c1").unwrap()]);

        let r = sync_into_store(&fetcher, &repo, &ws(), &scope, 0, 200).unwrap();
        assert_eq!(r.fetched, 1); // m2 is outside [0,200]
        assert_eq!(r.stored, 1);
        assert_eq!(r.channel_ids, vec!["c1".to_string()]);

        // Re-syncing the same window stores nothing new (idempotent).
        let r2 = sync_into_store(&fetcher, &repo, &ws(), &scope, 0, 200).unwrap();
        assert_eq!(r2.fetched, 1);
        assert_eq!(r2.stored, 0);

        // The message is queryable from the store.
        let stored = repo
            .list_messages(&ws(), &ChannelId::parse("c1").unwrap(), 0, 200)
            .unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].content, "plan the launch");
    }

    #[test]
    fn workspace_scope_resolves_all_channels() {
        let repo = SqliteRepository::in_memory().unwrap();
        let fetcher = FakeFetcher {
            store: vec![
                (
                    ChannelId::parse("c1").unwrap(),
                    msg("m1", "c1", 100, "channel one talk"),
                ),
                (
                    ChannelId::parse("c2").unwrap(),
                    msg("m2", "c2", 150, "channel two talk"),
                ),
            ],
        };
        let r = sync_into_store(&fetcher, &repo, &ws(), &FetchScope::Workspace, 0, 1000).unwrap();
        assert_eq!(r.fetched, 2);
        assert_eq!(r.stored, 2);
        assert_eq!(r.channel_ids.len(), 2);
    }
}
