//! The concrete [`ScheduleRunner`] (PRD §3, §12.7) — what a due schedule
//! actually *does*: resolve its channel scope (ADR-011), read the messages in
//! the lookback window, summarize them through the resilient pipeline, and
//! deliver the result (always-on dashboard store + any configured destinations).
//!
//! This is the connective tissue between Phase 6 (scheduler) and Phases 3/4
//! (summarize/deliver). Synchronous + deterministic given the injected services;
//! the periodic async driver that calls the scheduler's `tick` is the server's.

use crate::delivery::{load_workspace_delivery, Deliverer, DeliveryService};
use crate::llm::{LlmClient, LlmProvider, RequestPriority, ResilientLlm};
use crate::scheduler::ScheduleRunner;
use crate::summarize::{SummarizationService, SummarizeRequest};
use domain::summarize::{ModelLadder, SummaryLength};
use repository::{
    DestinationRepository, PlatformCredentialRepository, ScheduleSourceRepository, StoredSchedule,
    StructuredSummaryRepository, SummaryRecord, WhatsAppRepository, WorkspaceSettingsRepository,
};

/// Runs a scheduled summary end-to-end. Generic over the storage backend and the
/// LLM client so it's testable with fakes.
pub struct SummarizingScheduleRunner<'a, R, C: LlmClient> {
    repo: &'a R,
    engine: &'a ResilientLlm<C>,
    ladder: &'a ModelLadder,
    length: SummaryLength,
    provider: LlmProvider,
    cap_micros: i64,
    /// Deliverers for external destinations (DSH-010); empty = dashboard-only.
    deliverers: &'a [Box<dyn Deliverer + 'a>],
    /// Master key to decrypt stored destination addresses (`None` = none stored).
    master: Option<[u8; 32]>,
}

impl<'a, R, C: LlmClient> SummarizingScheduleRunner<'a, R, C> {
    pub fn new(repo: &'a R, engine: &'a ResilientLlm<C>, ladder: &'a ModelLadder) -> Self {
        Self {
            repo,
            engine,
            ladder,
            length: SummaryLength::Detailed,
            provider: LlmProvider::OpenRouter,
            cap_micros: i64::MAX,
            deliverers: &[],
            master: None,
        }
    }

    pub fn with_options(
        mut self,
        length: SummaryLength,
        provider: LlmProvider,
        cap_micros: i64,
    ) -> Self {
        self.length = length;
        self.provider = provider;
        self.cap_micros = cap_micros;
        self
    }

    /// Configure external delivery (DSH-010): the deliverer set and the master
    /// key used to decrypt stored destination addresses.
    pub fn with_delivery(
        mut self,
        deliverers: &'a [Box<dyn Deliverer + 'a>],
        master: Option<[u8; 32]>,
    ) -> Self {
        self.deliverers = deliverers;
        self.master = master;
        self
    }
}

impl<R, C: LlmClient> SummarizingScheduleRunner<'_, R, C>
where
    R: WhatsAppRepository + PlatformCredentialRepository + ScheduleSourceRepository,
{
    /// Best-effort live fetch into the store before a scheduled summary (ADR-128).
    /// Every failure is swallowed (logged) so the summary still runs over whatever
    /// is already stored — a missing token, an uncompiled platform feature, or a
    /// transient network error must not fail the schedule.
    fn live_sync(
        &self,
        stored: &StoredSchedule,
        channel: &domain::ChannelId,
        start: i64,
        now: i64,
    ) {
        let ws = &stored.schedule.workspace_id;
        let Some(master) = self.master else { return };
        let Ok(Some(src)) = self.repo.get_schedule_source(&stored.id) else {
            return;
        };
        let Ok(platform) = domain::Platform::parse(&src.platform) else {
            return;
        };
        let Ok(Some(enc)) = self.repo.get_platform_token(ws, &src.platform) else {
            return;
        };
        let Ok(token) = crate::decrypt_secret(&master, &enc) else {
            eprintln!(
                "scheduled live sync: cannot decrypt token for {}",
                stored.id
            );
            return;
        };
        let fetcher = match crate::make_platform_fetcher(platform, token, src.source_id) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("scheduled live sync skipped for {}: {e}", stored.id);
                return;
            }
        };
        let scope = crate::FetchScope::Channels(vec![channel.clone()]);
        if let Err(e) = crate::sync_into_store(&*fetcher, self.repo, ws, &scope, start, now) {
            eprintln!("scheduled live sync failed for {}: {e}", stored.id);
        }
    }
}

impl<R, C> ScheduleRunner for SummarizingScheduleRunner<'_, R, C>
where
    R: WhatsAppRepository
        + StructuredSummaryRepository
        + DestinationRepository
        + WorkspaceSettingsRepository
        + PlatformCredentialRepository
        + ScheduleSourceRepository,
    C: LlmClient,
{
    fn run(&self, stored: &StoredSchedule, now: i64) -> Result<(), String> {
        let ws = &stored.schedule.workspace_id;
        // No scope set → nothing to do (not a failure).
        let Some(channel) = stored.schedule.channel.clone() else {
            return Ok(());
        };
        let start = now - stored.schedule.lookback_secs;

        // If this schedule has a live source (ADR-128), pull fresh messages into
        // the store before reading the window. Best-effort: missing creds, an
        // uncompiled platform feature, or a network failure logs and falls back to
        // whatever was already stored — a scheduled summary never fails on sync.
        self.live_sync(stored, &channel, start, now);

        let messages = self
            .repo
            .list_messages(ws, &channel, start, now)
            .map_err(|e| e.to_string())?;
        // Nothing substantial in the window → skip quietly (no empty summaries).
        if !messages.iter().any(|m| m.is_substantial()) {
            return Ok(());
        }

        // Per-workspace prompt guidance (SUM-007).
        let instructions = self
            .repo
            .get_settings(ws)
            .map_err(|e| e.to_string())?
            .summary_instructions;

        let outcome = SummarizationService::new(self.engine, self.ladder)
            .summarize(&SummarizeRequest {
                messages: &messages,
                length: self.length,
                provider: self.provider,
                priority: RequestPriority::Low, // scheduled work yields to manual
                cap_micros: self.cap_micros,
                instructions: instructions.as_deref(),
            })
            .map_err(|e| format!("{e:?}"))?;

        let record = SummaryRecord {
            id: format!("sum_{}_{}", stored.id, now),
            channel_id: Some(channel),
            model: outcome.model,
            cost_micros: outcome.cost_micros,
            degraded: outcome.degraded,
            created_at: now,
            pinned: false,
            archived: false,
            tags: vec![],
            coherence_score: Some(outcome.coherence.score),
            summary: outcome.summary,
        };
        // Fan out: always-on dashboard store + any configured destinations
        // (DSH-010/011), gated by the workspace's capabilities.
        let (destinations, caps) = load_workspace_delivery(self.repo, ws, self.master.as_ref())
            .map_err(|e| e.to_string())?;
        DeliveryService::new(self.repo)
            .with_deliverers(self.deliverers)
            .deliver(ws, &record, &destinations, &caps)
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{GlobalRateLimiter, LlmError, LlmRequest, LlmResponse, RateLimitConfig};
    use crate::scheduler::SchedulerService;
    use domain::summarize::{FinishReason, Model, ModelPrice};
    use domain::{ChannelId, NormalizedMessage, Platform, Schedule, WorkspaceId};
    use repository::{ScheduleRepository, SqliteRepository};
    use std::sync::Arc;

    struct FakeLlm;
    impl LlmClient for FakeLlm {
        fn complete(&self, _req: &LlmRequest) -> Result<LlmResponse, LlmError> {
            Ok(LlmResponse {
                model: "fake".into(),
                text: r#"{"text":"Daily recap.","key_points":["shipped"],"action_items":[],"technical_terms":[],"participants":["Alice"],"citations":[]}"#.into(),
                finish_reason: FinishReason::Stop,
            })
        }
    }

    fn ladder() -> ModelLadder {
        ModelLadder::new(vec![Model {
            name: "fake".into(),
            price: ModelPrice {
                input_micros_per_ktoken: 1,
                output_micros_per_ktoken: 1,
            },
            context_tokens: 200_000,
        }])
    }

    fn msg(id: &str, ts: i64, content: &str) -> NormalizedMessage {
        NormalizedMessage {
            id: domain::MessageId::parse(id).unwrap(),
            platform: Platform::WhatsApp,
            channel_id: ChannelId::parse("c1").unwrap(),
            author_id: "p1".into(),
            author_name: "Alice".into(),
            content: content.into(),
            timestamp: ts,
            is_system: false,
            reply_to: None,
            attachments: vec![],
        }
    }

    fn schedule_with_channel(ws: &WorkspaceId, next_run: i64) -> StoredSchedule {
        let schedule = Schedule::build(
            ws.clone(),
            "hourly",
            0,
            0,
            &[],
            1,
            "UTC",
            None,
            0,
            true,
            Some("c1"),
            100_000,
        )
        .unwrap();
        StoredSchedule {
            id: "sch_1".into(),
            schedule,
            next_run,
            consecutive_failures: 0,
        }
    }

    #[test]
    fn scheduled_run_summarizes_stored_messages_and_stores_result() {
        let repo = SqliteRepository::in_memory().unwrap();
        let ws = WorkspaceId::parse("ws-1").unwrap();
        repo.save_message(&ws, &msg("m0", 3_500, "we shipped the release today"))
            .unwrap();
        repo.save_message(&ws, &msg("m1", 3_550, "great work everyone"))
            .unwrap();
        repo.create_schedule(&schedule_with_channel(&ws, 3_600))
            .unwrap();

        let limiter = Arc::new(GlobalRateLimiter::new(RateLimitConfig::default()));
        let engine = ResilientLlm::new(FakeLlm, limiter);
        let l = ladder();
        let runner = SummarizingScheduleRunner::new(&repo, &engine, &l);

        // Tick at the due time → fires → produces + stores a summary.
        let report = SchedulerService::new(&repo).tick(&runner, 3_600).unwrap();
        assert_eq!(report.fired, 1);
        let stored = repo.list_records(&ws, false, 10).unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].summary.key_points, vec!["shipped".to_string()]);
        assert_eq!(stored[0].channel_id.as_ref().unwrap().as_str(), "c1");
    }

    #[test]
    fn live_source_without_platform_feature_falls_back_to_stored() {
        // A schedule bound to a Discord source, but the default test build has no
        // `discord` feature → the live sync is skipped gracefully and the run
        // still summarizes the already-stored messages (ADR-128 best-effort).
        use repository::{ScheduleSource, ScheduleSourceRepository};
        let repo = SqliteRepository::in_memory().unwrap();
        let ws = WorkspaceId::parse("ws-1").unwrap();
        repo.save_message(&ws, &msg("m0", 3_500, "we shipped the release today"))
            .unwrap();
        repo.create_schedule(&schedule_with_channel(&ws, 3_600))
            .unwrap();
        repo.set_schedule_source(
            "sch_1",
            &ScheduleSource {
                platform: "discord".into(),
                source_id: Some("guild-1".into()),
            },
        )
        .unwrap();

        let limiter = Arc::new(GlobalRateLimiter::new(RateLimitConfig::default()));
        let engine = ResilientLlm::new(FakeLlm, limiter);
        let l = ladder();
        // A master key is set so live_sync gets past its guard and hits the
        // (uncompiled) platform factory, which errors and is swallowed.
        let runner =
            SummarizingScheduleRunner::new(&repo, &engine, &l).with_delivery(&[], Some([7u8; 32]));

        let report = SchedulerService::new(&repo).tick(&runner, 3_600).unwrap();
        assert_eq!(report.fired, 1);
        let stored = repo.list_records(&ws, false, 10).unwrap();
        assert_eq!(
            stored.len(),
            1,
            "summarized the stored messages despite no live feature"
        );
    }

    #[test]
    fn no_channel_scope_is_a_quiet_noop() {
        let repo = SqliteRepository::in_memory().unwrap();
        let ws = WorkspaceId::parse("ws-1").unwrap();
        let mut s = schedule_with_channel(&ws, 3_600);
        s.schedule.channel = None; // unscoped
        repo.create_schedule(&s).unwrap();

        let limiter = Arc::new(GlobalRateLimiter::new(RateLimitConfig::default()));
        let engine = ResilientLlm::new(FakeLlm, limiter);
        let l = ladder();
        let runner = SummarizingScheduleRunner::new(&repo, &engine, &l);

        let report = SchedulerService::new(&repo).tick(&runner, 3_600).unwrap();
        assert_eq!(report.fired, 1); // ran, but...
        assert!(repo.list_records(&ws, false, 10).unwrap().is_empty()); // produced nothing
    }
}
