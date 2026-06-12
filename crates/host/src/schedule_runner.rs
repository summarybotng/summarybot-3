//! The concrete [`ScheduleRunner`] (PRD §3, §12.7) — what a due schedule
//! actually *does*: resolve its channel scope (ADR-011), read the messages in
//! the lookback window, summarize them through the resilient pipeline, and
//! deliver the result (always-on dashboard store + any configured destinations).
//!
//! This is the connective tissue between Phase 6 (scheduler) and Phases 3/4
//! (summarize/deliver). Synchronous + deterministic given the injected services;
//! the periodic async driver that calls the scheduler's `tick` is the server's.

use crate::delivery::{load_workspace_delivery, Deliverer, DeliveryService};
use crate::knowledge::{Embedder, KnowledgeService};
use crate::llm::{LlmClient, LlmProvider, RequestPriority, ResilientLlm};
use crate::scheduler::ScheduleRunner;
use crate::summarize::{SummarizationService, SummarizeRequest, SummaryOutcome};
use domain::summarize::{ExtractedSummary, ModelLadder, SummaryLength};
use domain::{decide_rolling, end_weekday, format_day, RollingAction, RollingPeriod, RollingState};
use repository::{
    DestinationRepository, KnowledgeRepository, PlatformCredentialRepository, RollingConfig,
    RollingRepository, RollingSummaryRow, ScheduleSourceRepository, StoredSchedule,
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
    /// Embedder for feeding rolling-delta facts into the knowledge base (ADR-129
    /// Layer 3); `None` disables knowledge ingestion for scheduled runs.
    embedder: Option<&'a dyn Embedder>,
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
            embedder: None,
        }
    }

    /// Feed rolling-delta knowledge units into the vector store as each period
    /// accumulates (ADR-129 Layer 3). Best-effort; dedup (Layers 1–2) handles
    /// overlap. `None` (the default) leaves scheduled runs out of the knowledge base.
    pub fn with_knowledge(mut self, embedder: &'a dyn Embedder) -> Self {
        self.embedder = Some(embedder);
        self
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
        + ScheduleSourceRepository
        + RollingRepository
        + KnowledgeRepository,
    C: LlmClient,
{
    fn run(&self, stored: &StoredSchedule, now: i64) -> Result<(), String> {
        let ws = &stored.schedule.workspace_id;
        // No scope set → nothing to do (not a failure).
        let Some(channel) = stored.schedule.channel.clone() else {
            return Ok(());
        };

        // Rolling-period schedule (ADR-101)? Drive the accumulation state machine
        // instead of producing an independent summary each run.
        if let Ok(Some(cfg)) = self.repo.get_rolling_config(&stored.id) {
            return self.run_rolling(stored, &channel, &cfg, now);
        }

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
        self.deliver_record(ws, &record)
    }
}

/// Render one summary as a markdown section body (text + key points + action
/// items) and append it under a dated heading to the running rolling document.
fn append_section(content: &str, label: &str, s: &ExtractedSummary) -> String {
    let mut out = content.to_string();
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(&format!("## {label}\n"));
    let text = s.text.trim();
    if !text.is_empty() {
        out.push_str(text);
        out.push('\n');
    }
    for kp in &s.key_points {
        out.push_str("- ");
        out.push_str(kp);
        out.push('\n');
    }
    for ai in &s.action_items {
        out.push_str("- [ ] ");
        out.push_str(&ai.text);
        if let Some(a) = &ai.assignee {
            out.push_str(" (");
            out.push_str(a);
            out.push(')');
        }
        out.push('\n');
    }
    out
}

impl<R, C> SummarizingScheduleRunner<'_, R, C>
where
    R: WhatsAppRepository
        + StructuredSummaryRepository
        + DestinationRepository
        + WorkspaceSettingsRepository
        + PlatformCredentialRepository
        + ScheduleSourceRepository
        + RollingRepository
        + KnowledgeRepository,
    C: LlmClient,
{
    /// Feed one rolling delta's facts into the knowledge base (ADR-129 Layer 3) —
    /// incrementally, so dedup (Layers 1–2) handles overlap and finalize needn't
    /// re-ingest the whole digest. Best-effort: never fails the run.
    fn ingest_delta(
        &self,
        workspace: &domain::WorkspaceId,
        schedule_id: &str,
        summary: &ExtractedSummary,
        until: i64,
        now: i64,
    ) {
        let Some(embedder) = self.embedder else {
            return;
        };
        let sid = format!("roll_{schedule_id}_{until}");
        if let Err(e) =
            KnowledgeService::new(self.repo, embedder).ingest(workspace, summary, &sid, now)
        {
            eprintln!("rolling knowledge ingest failed for {schedule_id}: {e}");
        }
    }

    /// Deliver a produced record: always-on dashboard store + any configured
    /// destinations (DSH-010/011), gated by the workspace's capabilities. Shared
    /// by the one-shot and rolling-finalize paths.
    fn deliver_record(
        &self,
        ws: &domain::WorkspaceId,
        record: &SummaryRecord,
    ) -> Result<(), String> {
        let (destinations, caps) = load_workspace_delivery(self.repo, ws, self.master.as_ref())
            .map_err(|e| e.to_string())?;
        DeliveryService::new(self.repo)
            .with_deliverers(self.deliverers)
            .deliver(ws, record, &destinations, &caps)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Summarize a channel's messages in `(since, until]` (live-syncing first, as
    /// the one-shot path does). Returns `None` when there's nothing substantial —
    /// so an empty window contributes no section to the rolling document.
    fn summarize_window(
        &self,
        stored: &StoredSchedule,
        channel: &domain::ChannelId,
        since: i64,
        until: i64,
    ) -> Result<Option<SummaryOutcome>, String> {
        let ws = &stored.schedule.workspace_id;
        self.live_sync(stored, channel, since, until);
        let messages = self
            .repo
            .list_messages(ws, channel, since, until)
            .map_err(|e| e.to_string())?;
        if !messages.iter().any(|m| m.is_substantial()) {
            return Ok(None);
        }
        let instructions = self
            .repo
            .get_settings(ws)
            .map_err(|e| e.to_string())?
            .summary_instructions;
        // Daily deltas are brief; the period's document is the cumulative view.
        let outcome = SummarizationService::new(self.engine, self.ladder)
            .summarize(&SummarizeRequest {
                messages: &messages,
                length: SummaryLength::Brief,
                provider: self.provider,
                priority: RequestPriority::Low,
                cap_micros: self.cap_micros,
                instructions: instructions.as_deref(),
            })
            .map_err(|e| format!("{e:?}"))?;
        Ok(Some(outcome))
    }

    /// Drive the rolling-period state machine (ADR-101) for a due run: open a new
    /// period, accumulate the days since the last run, or finalize a period that
    /// has ended (publishing it as a normal summary and clearing the accumulator).
    /// v1 merge is **Append** (dated sections); `resummarize`/`hybrid` are accepted
    /// but currently behave as append (a documented refinement, ADR-129).
    fn run_rolling(
        &self,
        stored: &StoredSchedule,
        channel: &domain::ChannelId,
        cfg: &RollingConfig,
        now: i64,
    ) -> Result<(), String> {
        let ws = &stored.schedule.workspace_id;
        let tz = stored.schedule.timezone;
        let period = RollingPeriod::parse(&cfg.period)
            .ok_or_else(|| format!("unknown rolling period: {}", cfg.period))?;
        let window = period
            .window(now, tz, end_weekday(cfg.end_day))
            .ok_or("could not resolve rolling window")?;

        let active = self
            .repo
            .get_active_rolling(&stored.id)
            .map_err(|e| e.to_string())?;
        let state = active.as_ref().map(|r| RollingState {
            period_start: r.period_start,
            period_end: r.period_end,
            accumulated_through: r.accumulated_through,
            finalized: false,
            accumulation_count: r.accumulation_count,
        });

        match decide_rolling(state.as_ref(), window, now) {
            RollingAction::StartNew { window, until } => {
                let outcome = self.summarize_window(stored, channel, window.start, until)?;
                let (content_md, cost_micros, model, count) = match outcome {
                    Some(o) => {
                        self.ingest_delta(ws, &stored.id, &o.summary, until, now);
                        (
                            append_section("", &format_day(until, tz), &o.summary),
                            o.cost_micros,
                            o.model,
                            1,
                        )
                    }
                    None => (String::new(), 0, String::new(), 0),
                };
                self.repo
                    .upsert_active_rolling(&RollingSummaryRow {
                        schedule_id: stored.id.clone(),
                        workspace_id: ws.as_str().to_string(),
                        channel: channel.as_str().to_string(),
                        period_start: window.start,
                        period_end: window.end,
                        accumulated_through: until,
                        accumulation_count: count,
                        content_md,
                        cost_micros,
                        model,
                        created_at: now,
                        updated_at: now,
                    })
                    .map_err(|e| e.to_string())?;
            }
            RollingAction::Accumulate { since, until } => {
                let Some(mut row) = active else { return Ok(()) };
                if let Some(o) = self.summarize_window(stored, channel, since, until)? {
                    self.ingest_delta(ws, &stored.id, &o.summary, until, now);
                    row.content_md =
                        append_section(&row.content_md, &format_day(until, tz), &o.summary);
                    row.cost_micros += o.cost_micros;
                    row.model = o.model;
                    row.accumulation_count += 1;
                }
                row.accumulated_through = until;
                row.updated_at = now;
                self.repo
                    .upsert_active_rolling(&row)
                    .map_err(|e| e.to_string())?;
            }
            RollingAction::Finalize => {
                let Some(mut row) = active else { return Ok(()) };
                // Fold the tail (last run → period end) before publishing.
                if let Some(o) =
                    self.summarize_window(stored, channel, row.accumulated_through, row.period_end)?
                {
                    self.ingest_delta(ws, &stored.id, &o.summary, row.period_end, now);
                    row.content_md = append_section(
                        &row.content_md,
                        &format_day(row.period_end.saturating_sub(1), tz),
                        &o.summary,
                    );
                    row.cost_micros += o.cost_micros;
                    if !o.model.is_empty() {
                        row.model = o.model;
                    }
                }
                if !row.content_md.trim().is_empty() {
                    let header = format!(
                        "# {} digest · {} → {}\n\n",
                        cfg.period,
                        format_day(row.period_start, tz),
                        format_day(row.period_end.saturating_sub(1), tz),
                    );
                    let record = SummaryRecord {
                        id: format!("sum_{}_{}", stored.id, row.period_end),
                        channel_id: Some(channel.clone()),
                        model: if row.model.is_empty() {
                            "rolling".to_string()
                        } else {
                            row.model.clone()
                        },
                        cost_micros: row.cost_micros,
                        degraded: false,
                        created_at: now,
                        pinned: false,
                        archived: false,
                        tags: vec![format!("rolling-{}", cfg.period)],
                        coherence_score: None,
                        summary: ExtractedSummary {
                            text: format!("{header}{}", row.content_md),
                            key_points: vec![],
                            action_items: vec![],
                            technical_terms: vec![],
                            participants: vec![],
                            citations: vec![],
                        },
                    };
                    self.deliver_record(ws, &record)?;
                }
                self.repo
                    .delete_active_rolling(&stored.id)
                    .map_err(|e| e.to_string())?;
            }
        }
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
    fn rolling_weekly_accumulates_then_finalizes_one_digest() {
        use repository::{RollingConfig, RollingRepository};
        let repo = SqliteRepository::in_memory().unwrap();
        let ws = WorkspaceId::parse("ws-1").unwrap();
        repo.create_schedule(&schedule_with_channel(&ws, 0))
            .unwrap();
        // Make the schedule rolling-weekly (ends Sunday=6, append strategy).
        repo.set_rolling_config(
            "sch_1",
            &RollingConfig {
                period: "weekly".into(),
                strategy: "append".into(),
                end_day: 6,
            },
        )
        .unwrap();

        // Resolve a concrete week so timestamps land inside it.
        let tz = chrono_tz::UTC;
        let window = RollingPeriod::Weekly
            .window(1_767_700_000, tz, end_weekday(6))
            .unwrap();
        let now1 = window.start + 1_000;
        let now2 = window.start + 100_000;
        let now3 = window.end + 10;

        repo.save_message(
            &ws,
            &msg("m0", window.start + 500, "we shipped the release today"),
        )
        .unwrap();

        let limiter = Arc::new(GlobalRateLimiter::new(RateLimitConfig::default()));
        let engine = ResilientLlm::new(FakeLlm, limiter);
        let l = ladder();
        let runner = SummarizingScheduleRunner::new(&repo, &engine, &l);
        let sched = repo.get_schedule(&ws, "sch_1").unwrap().unwrap();

        // Run 1 (in-period, no active) → StartNew + accumulate the week so far.
        runner.run(&sched, now1).unwrap();
        let active = repo.get_active_rolling("sch_1").unwrap().unwrap();
        assert_eq!(active.period_start, window.start);
        assert_eq!(active.period_end, window.end);
        assert_eq!(active.accumulated_through, now1);
        assert_eq!(active.accumulation_count, 1);
        assert!(active.content_md.contains("shipped"));
        assert!(repo.list_records(&ws, false, 10).unwrap().is_empty()); // not finalized yet

        // A new message, then Run 2 (still in-period) → Accumulate the delta.
        repo.save_message(
            &ws,
            &msg("m1", now1 + 500, "deployed the hotfix successfully"),
        )
        .unwrap();
        runner.run(&sched, now2).unwrap();
        let active = repo.get_active_rolling("sch_1").unwrap().unwrap();
        assert_eq!(active.accumulated_through, now2);
        assert_eq!(active.accumulation_count, 2);

        // Run 3 (period ended) → Finalize: publish one digest, clear the accumulator.
        runner.run(&sched, now3).unwrap();
        assert!(repo.get_active_rolling("sch_1").unwrap().is_none());
        let records = repo.list_records(&ws, false, 10).unwrap();
        assert_eq!(records.len(), 1, "exactly one finalized weekly digest");
        assert!(records[0].summary.text.contains("digest"));
        assert!(records[0].tags.contains(&"rolling-weekly".to_string()));
        assert_eq!(records[0].channel_id.as_ref().unwrap().as_str(), "c1");
    }

    #[test]
    fn rolling_delta_feeds_the_knowledge_base() {
        use crate::knowledge::DemoEmbedder;
        use repository::{KnowledgeRepository, RollingConfig, RollingRepository};
        let repo = SqliteRepository::in_memory().unwrap();
        let ws = WorkspaceId::parse("ws-1").unwrap();
        repo.create_schedule(&schedule_with_channel(&ws, 0))
            .unwrap();
        repo.set_rolling_config(
            "sch_1",
            &RollingConfig {
                period: "weekly".into(),
                strategy: "append".into(),
                end_day: 6,
            },
        )
        .unwrap();
        let tz = chrono_tz::UTC;
        let window = RollingPeriod::Weekly
            .window(1_767_700_000, tz, end_weekday(6))
            .unwrap();
        repo.save_message(
            &ws,
            &msg(
                "m0",
                window.start + 500,
                "we shipped the release and ran the migration",
            ),
        )
        .unwrap();

        let limiter = Arc::new(GlobalRateLimiter::new(RateLimitConfig::default()));
        let engine = ResilientLlm::new(FakeLlm, limiter);
        let l = ladder();
        let emb = DemoEmbedder::default();
        // Knowledge ingestion is opt-in: off → no units; on → the rolling delta
        // feeds the knowledge base (ADR-129 Layer 3).
        SummarizingScheduleRunner::new(&repo, &engine, &l)
            .run(
                &repo.get_schedule(&ws, "sch_1").unwrap().unwrap(),
                window.start + 1_000,
            )
            .unwrap();
        assert_eq!(
            repo.count_units(&ws).unwrap(),
            0,
            "no ingest without with_knowledge"
        );

        // Clear the accumulator so the next run re-opens (StartNew) and ingests.
        repo.delete_active_rolling("sch_1").unwrap();
        SummarizingScheduleRunner::new(&repo, &engine, &l)
            .with_knowledge(&emb)
            .run(
                &repo.get_schedule(&ws, "sch_1").unwrap().unwrap(),
                window.start + 1_000,
            )
            .unwrap();
        assert!(
            repo.count_units(&ws).unwrap() > 0,
            "rolling delta fed the knowledge base"
        );
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
