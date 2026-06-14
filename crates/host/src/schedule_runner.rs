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
use domain::{
    decide_rolling, end_weekday, format_day, AccumulationStrategy, RollingAction, RollingPeriod,
    RollingState,
};
use repository::{
    DestinationRepository, JobRepository, KnowledgeRepository, PlatformCredentialRepository,
    RollingConfig, RollingRepository, RollingSummaryRow, ScheduleDestinationRepository,
    ScheduleSourceRepository, StoredSchedule, StructuredSummaryRepository, SummaryRecord,
    TenantPluginRepository, WhatsAppRepository, WorkspaceRepository, WorkspaceSettingsRepository,
};

/// Sentinel channel id meaning "all of the workspace's channels" (ADR-011
/// workspace scope). A schedule with this scope reads the message store across
/// every channel rather than one, and its summary is workspace-wide (no single
/// channel). Chosen as a value no real platform channel id uses.
pub const ALL_CHANNELS: &str = "*";

/// Sentinel prefix for a category-scoped schedule (ADR-011 category scope):
/// `category:<platform-category-id>`. At run time the bound platform source
/// (Discord) resolves the category to its current channel set, so a summary
/// follows channels added to / removed from the category over time. Chosen so it
/// can't collide with a real channel id (which never contains this prefix).
pub const CATEGORY_PREFIX: &str = "category:";

/// If `channel` is a category sentinel, return the platform category id.
pub fn parse_category(channel: &str) -> Option<&str> {
    channel.strip_prefix(CATEGORY_PREFIX).filter(|s| !s.is_empty())
}

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
        let Some(fetcher) = self.build_fetcher(stored) else {
            return;
        };
        let ws = &stored.schedule.workspace_id;
        let scope = crate::FetchScope::Channels(vec![channel.clone()]);
        if let Err(e) = crate::sync_into_store(&*fetcher, self.repo, ws, &scope, start, now) {
            eprintln!("scheduled live sync failed for {}: {e}", stored.id);
        }
    }

    /// Build the live platform fetcher for a schedule's bound source (ADR-128),
    /// or `None` if there's no master key / source / token / compiled platform.
    /// Best-effort: every miss logs (where useful) and yields `None` so the
    /// caller falls back to whatever is already stored.
    fn build_fetcher(&self, stored: &StoredSchedule) -> Option<Box<dyn crate::PlatformFetcher>> {
        let ws = &stored.schedule.workspace_id;
        let master = self.master?;
        let src = self.repo.get_schedule_source(&stored.id).ok()??;
        let platform = domain::Platform::parse(&src.platform).ok()?;
        let enc = self.repo.get_platform_token(ws, &src.platform).ok()??;
        let token = match crate::decrypt_secret(&master, &enc) {
            Ok(t) => t,
            Err(_) => {
                eprintln!("scheduled live sync: cannot decrypt token for {}", stored.id);
                return None;
            }
        };
        match crate::make_platform_fetcher(platform, token, src.source_id) {
            Ok(f) => Some(f),
            Err(e) => {
                eprintln!("scheduled live sync skipped for {}: {e}", stored.id);
                None
            }
        }
    }

    /// Category scope (ADR-011): resolve the category's current channels from the
    /// bound source, sync their recent messages into the store, and return the
    /// resolved channel set. Best-effort — any failure (no source/token/feature,
    /// or a platform that lacks categories like Slack) yields an empty set, so a
    /// category schedule with no reachable Discord source quietly produces nothing.
    fn sync_category(
        &self,
        stored: &StoredSchedule,
        category_id: &str,
        start: i64,
        now: i64,
    ) -> Vec<domain::ChannelId> {
        let Some(fetcher) = self.build_fetcher(stored) else {
            return vec![];
        };
        let ws = &stored.schedule.workspace_id;
        let scope = crate::FetchScope::Category(category_id.to_string());
        let channels = fetcher.resolve_channels(&scope).unwrap_or_default();
        if let Err(e) = crate::sync_into_store(&*fetcher, self.repo, ws, &scope, start, now) {
            eprintln!("scheduled category sync failed for {}: {e}", stored.id);
        }
        channels
    }

    /// Read every message in `[start, now]` across `channels`, merged into one
    /// chronological stream — the basis for a category-scoped summary.
    fn read_across(
        &self,
        ws: &domain::WorkspaceId,
        channels: &[domain::ChannelId],
        start: i64,
        now: i64,
    ) -> Result<Vec<domain::NormalizedMessage>, String> {
        let mut all = Vec::new();
        for ch in channels {
            let mut msgs = self
                .repo
                .list_messages(ws, ch, start, now)
                .map_err(|e| e.to_string())?;
            all.append(&mut msgs);
        }
        all.sort_by_key(|m| m.timestamp);
        Ok(all)
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
        + KnowledgeRepository
        + WorkspaceRepository
        + TenantPluginRepository
        + JobRepository
        + ScheduleDestinationRepository,
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
        let all = channel.as_str() == ALL_CHANNELS;
        let category = parse_category(channel.as_str());

        // If this schedule has a live source (ADR-128), pull fresh messages into
        // the store before reading the window. Best-effort: missing creds, an
        // uncompiled platform feature, or a network failure logs and falls back to
        // whatever was already stored — a scheduled summary never fails on sync.
        // (All-channels scope reads the store directly; there's no single channel
        // to live-sync.)
        if !all && category.is_none() {
            self.live_sync(stored, &channel, start, now);
        }

        // Scope (ADR-011): a specific channel, a category's channels (resolved
        // live from the bound Discord source), or all of the workspace's channels.
        let messages = if let Some(cat) = category {
            let channels = self.sync_category(stored, cat, start, now);
            self.read_across(ws, &channels, start, now)?
        } else if all {
            self.repo.list_messages_all(ws, start, now).map_err(|e| e.to_string())?
        } else {
            self.repo.list_messages(ws, &channel, start, now).map_err(|e| e.to_string())?
        };
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

        // Track the run as a job (ADR-013/040) so scheduled summaries — not just
        // manual ones — show in the Jobs view with status + cost. Recorded before
        // the LLM work; failures persist a classified reason. Deterministic id so a
        // re-fire at the same instant is idempotent.
        let mut job = domain::Job::record(
            domain::JobId::parse(format!("job_sch_{}_{}", stored.id, now))
                .map_err(|e| e.to_string())?,
            ws.clone(),
            domain::JobType::Scheduled,
            now,
        );
        let _ = job.start(now);
        let _ = self.repo.create_job(&job);

        let outcome = match SummarizationService::new(self.engine, self.ladder).summarize(
            &SummarizeRequest {
                messages: &messages,
                length: self.length,
                provider: self.provider,
                priority: RequestPriority::Low, // scheduled work yields to manual
                cap_micros: self.cap_micros,
                instructions: instructions.as_deref(),
            },
        ) {
            Ok(o) => o,
            Err(e) => {
                let _ = job.fail(e.failure_class(), 0, now);
                let _ = self.repo.update_job(&job);
                return Err(format!("{e:?}"));
            }
        };
        let cost = outcome.cost_micros;

        // All-channels and category scopes span multiple channels, so the record
        // has no single channel; each carries a tag describing its scope.
        let multi = all || category.is_some();
        let tags = if all {
            vec!["all-channels".to_string()]
        } else if let Some(cat) = category {
            vec![format!("category:{cat}")]
        } else {
            vec![]
        };
        let record = SummaryRecord {
            id: format!("sum_{}_{}", stored.id, now),
            channel_id: if multi { None } else { Some(channel) },
            model: outcome.model,
            cost_micros: outcome.cost_micros,
            degraded: outcome.degraded,
            created_at: now,
            pinned: false,
            archived: false,
            tags,
            coherence_score: Some(outcome.coherence.score),
            usage: outcome.usage,
            summary: outcome.summary,
        };
        let delivered = self.deliver_record(ws, &stored.id, &record);
        match &delivered {
            Ok(()) => {
                let _ = job.complete(cost, now);
            }
            Err(_) => {
                let _ = job.fail(domain::FailureClass::Unknown, cost, now);
            }
        }
        let _ = self.repo.update_job(&job);
        delivered
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
        out.push_str(&kp.text);
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
        + KnowledgeRepository
        + WorkspaceRepository
        + TenantPluginRepository
        + JobRepository
        + ScheduleDestinationRepository,
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
            KnowledgeService::new(self.repo, embedder).ingest(workspace, summary, &sid, None, now)
        {
            eprintln!("rolling knowledge ingest failed for {schedule_id}: {e}");
        }
    }

    /// Deliver a produced record: always-on dashboard store + any configured
    /// destinations (DSH-010/011), gated by the workspace's capabilities. Shared
    /// by the one-shot and rolling-finalize paths. If `schedule_id`'s schedule has
    /// a destination selection (ADR-014), delivery is restricted to it; otherwise
    /// it goes to all enabled destinations.
    fn deliver_record(
        &self,
        ws: &domain::WorkspaceId,
        schedule_id: &str,
        record: &SummaryRecord,
    ) -> Result<(), String> {
        let (mut destinations, caps) =
            load_workspace_delivery(self.repo, ws, self.master.as_ref()).map_err(|e| e.to_string())?;
        let selected = self
            .repo
            .list_schedule_destinations(schedule_id)
            .unwrap_or_default();
        if !selected.is_empty() {
            destinations.retain(|d| selected.contains(&d.id));
        }
        DeliveryService::new(self.repo)
            .with_deliverers(self.deliverers)
            .deliver(ws, record, &destinations, &caps)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Deliver an *intermediate* rolling update (ADR-108): the in-progress digest
    /// goes only to destinations flagged `rolling_deliver_intermediate` (and within
    /// the schedule's ADR-014 selection), with no dashboard store. A quiet no-op
    /// when no destination opted in — the common case.
    fn deliver_intermediate(
        &self,
        ws: &domain::WorkspaceId,
        schedule_id: &str,
        record: &SummaryRecord,
    ) -> Result<(), String> {
        let (mut destinations, caps) =
            load_workspace_delivery(self.repo, ws, self.master.as_ref()).map_err(|e| e.to_string())?;
        let selected = self
            .repo
            .list_schedule_destinations(schedule_id)
            .unwrap_or_default();
        if !selected.is_empty() {
            destinations.retain(|d| selected.contains(&d.id));
        }
        destinations.retain(|d| d.rolling_deliver_intermediate);
        if destinations.is_empty() {
            return Ok(());
        }
        DeliveryService::new(self.repo)
            .with_deliverers(self.deliverers)
            .deliver_external(record, &destinations, &caps);
        Ok(())
    }

    /// Build an intermediate rolling record (ADR-108) from the accumulated
    /// document so far — an Append-style snapshot (no synthesis cost), tagged so
    /// it's distinguishable from the finalized digest.
    fn rolling_intermediate_record(
        &self,
        stored: &StoredSchedule,
        channel: &domain::ChannelId,
        cfg: &RollingConfig,
        content_md: &str,
        now: i64,
    ) -> SummaryRecord {
        let header = format!(
            "# {} digest (in progress) · as of {}\n\n",
            cfg.period,
            format_day(now, stored.schedule.timezone),
        );
        SummaryRecord {
            id: format!("sum_{}_{}_int", stored.id, now),
            channel_id: Some(channel.clone()),
            model: "rolling".to_string(),
            cost_micros: 0,
            degraded: false,
            created_at: now,
            pinned: false,
            archived: false,
            tags: vec![format!("rolling-{}-intermediate", cfg.period)],
            coherence_score: None,
            usage: domain::summarize::SummaryUsage::default(),
            summary: ExtractedSummary {
                text: format!("{header}{content_md}"),
                key_points: vec![],
                action_items: vec![],
                technical_terms: vec![],
                participants: vec![],
                citations: vec![],
            },
        }
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
        let all = channel.as_str() == ALL_CHANNELS;
        if !all {
            self.live_sync(stored, channel, since, until);
        }
        let messages = if all {
            self.repo.list_messages_all(ws, since, until)
        } else {
            self.repo.list_messages(ws, channel, since, until)
        }
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

    /// Synthesize the accumulated rolling document into one coherent digest
    /// (ADR-101 Hybrid/Resummarize merge): the per-day dated sections are fed back
    /// through the summarizer as input so the published digest has merged
    /// highlights + deduped structured fields, instead of a raw concatenation.
    /// `None` if there's nothing to synthesize.
    fn synthesize_digest(
        &self,
        stored: &StoredSchedule,
        channel: &domain::ChannelId,
        content_md: &str,
    ) -> Result<Option<SummaryOutcome>, String> {
        let trimmed = content_md.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        // Feed each dated section back through the summarizer as one input message;
        // its map-reduce (ADR-095) handles a long period. Synthetic messages carry
        // the section text so the reduce produces a coherent, deduped digest.
        let messages: Vec<domain::NormalizedMessage> = trimmed
            .split("\n## ")
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .enumerate()
            .map(|(i, text)| domain::NormalizedMessage {
                id: domain::MessageId::parse(format!("digest-{i}")).expect("valid synthetic id"),
                platform: domain::Platform::WhatsApp,
                channel_id: channel.clone(),
                author_id: "digest".into(),
                author_name: "Digest".into(),
                content: text.to_string(),
                timestamp: i as i64,
                is_system: false,
                reply_to: None,
                attachments: vec![],
            })
            .collect();
        if messages.is_empty() {
            return Ok(None);
        }
        let instructions = self
            .repo
            .get_settings(&stored.schedule.workspace_id)
            .map_err(|e| e.to_string())?
            .summary_instructions;
        let outcome = SummarizationService::new(self.engine, self.ladder)
            .summarize(&SummarizeRequest {
                messages: &messages,
                // The period digest is the cumulative view → richer than a daily.
                length: SummaryLength::Detailed,
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
    /// has ended (publishing it as a summary and clearing the accumulator).
    /// The merge strategy applies at finalize: **Append** publishes the dated
    /// sections as-is; **Hybrid**/**Resummarize** run a synthesis pass so the
    /// digest is coherent with merged structured fields (ADR-101).
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
                        content_md: content_md.clone(),
                        cost_micros,
                        model,
                        created_at: now,
                        updated_at: now,
                    })
                    .map_err(|e| e.to_string())?;
                // ADR-108: push the in-progress digest to any opted-in destinations.
                if !content_md.trim().is_empty() {
                    let rec = self.rolling_intermediate_record(stored, channel, cfg, &content_md, now);
                    self.deliver_intermediate(ws, &stored.id, &rec)?;
                }
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
                // ADR-108: push the in-progress digest to any opted-in destinations.
                if !row.content_md.trim().is_empty() {
                    let rec =
                        self.rolling_intermediate_record(stored, channel, cfg, &row.content_md, now);
                    self.deliver_intermediate(ws, &stored.id, &rec)?;
                }
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
                    let strategy = AccumulationStrategy::parse(&cfg.strategy)
                        .unwrap_or(AccumulationStrategy::Append);
                    // Hybrid/Resummarize fold the accumulated sections into one
                    // coherent digest; Append publishes them verbatim.
                    let (summary, model) = match strategy {
                        AccumulationStrategy::Append => (
                            ExtractedSummary {
                                text: format!("{header}{}", row.content_md),
                                key_points: vec![],
                                action_items: vec![],
                                technical_terms: vec![],
                                participants: vec![],
                                citations: vec![],
                            },
                            row.model.clone(),
                        ),
                        AccumulationStrategy::Hybrid | AccumulationStrategy::Resummarize => {
                            match self.synthesize_digest(stored, channel, &row.content_md)? {
                                Some(o) => {
                                    row.cost_micros += o.cost_micros;
                                    let model = if o.model.is_empty() {
                                        row.model.clone()
                                    } else {
                                        o.model
                                    };
                                    let mut s = o.summary;
                                    s.text = format!("{header}{}", s.text.trim());
                                    (s, model)
                                }
                                // Synthesis produced nothing → fall back to the raw doc.
                                None => (
                                    ExtractedSummary {
                                        text: format!("{header}{}", row.content_md),
                                        key_points: vec![],
                                        action_items: vec![],
                                        technical_terms: vec![],
                                        participants: vec![],
                                        citations: vec![],
                                    },
                                    row.model.clone(),
                                ),
                            }
                        }
                    };
                    let record = SummaryRecord {
                        id: format!("sum_{}_{}", stored.id, row.period_end),
                        channel_id: Some(channel.clone()),
                        model: if model.is_empty() {
                            "rolling".to_string()
                        } else {
                            model
                        },
                        cost_micros: row.cost_micros,
                        degraded: false,
                        created_at: now,
                        pinned: false,
                        archived: false,
                        tags: vec![format!("rolling-{}", cfg.period)],
                        coherence_score: None,
                        usage: domain::summarize::SummaryUsage::default(),
                        summary,
                    };
                    self.deliver_record(ws, &stored.id, &record)?;
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
    fn all_channels_scope_summarizes_across_every_channel() {
        let repo = SqliteRepository::in_memory().unwrap();
        let ws = WorkspaceId::parse("ws-1").unwrap();
        // Messages in two different channels within the window.
        let m2 = |id: &str, ts: i64, content: &str| NormalizedMessage {
            id: domain::MessageId::parse(id).unwrap(),
            platform: Platform::WhatsApp,
            channel_id: ChannelId::parse("c2").unwrap(),
            author_id: "p2".into(),
            author_name: "Bob".into(),
            content: content.into(),
            timestamp: ts,
            is_system: false,
            reply_to: None,
            attachments: vec![],
        };
        repo.save_message(&ws, &msg("m0", 3_500, "we shipped the release in channel one"))
            .unwrap();
        repo.save_message(&ws, &m2("m1", 3_550, "and planned the roadmap in channel two"))
            .unwrap();
        // A schedule scoped to ALL channels (the "*" sentinel).
        let schedule = Schedule::build(
            ws.clone(), "hourly", 0, 0, &[], 1, "UTC", None, 0, true, Some(super::ALL_CHANNELS), 100_000,
        )
        .unwrap();
        repo.create_schedule(&StoredSchedule {
            id: "sch_all".into(),
            schedule,
            next_run: 3_600,
            consecutive_failures: 0,
        })
        .unwrap();

        let limiter = Arc::new(GlobalRateLimiter::new(RateLimitConfig::default()));
        let engine = ResilientLlm::new(FakeLlm, limiter);
        let l = ladder();
        let runner = SummarizingScheduleRunner::new(&repo, &engine, &l);

        SchedulerService::new(&repo).tick(&runner, 3_600).unwrap();
        let stored = repo.list_records(&ws, false, 10).unwrap();
        assert_eq!(stored.len(), 1);
        // Workspace-wide → no single channel, tagged all-channels.
        assert!(stored[0].channel_id.is_none());
        assert!(stored[0].tags.iter().any(|t| t == "all-channels"));
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
        assert_eq!(
            stored[0].summary.key_points.iter().map(|k| k.text.as_str()).collect::<Vec<_>>(),
            vec!["shipped"]
        );
        assert_eq!(stored[0].channel_id.as_ref().unwrap().as_str(), "c1");

        // The run is tracked as a completed `Scheduled` job (ADR-013/040), so it
        // shows in the Jobs view alongside manual summaries and backfills.
        let jobs = repo.list_jobs(&ws, 10).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].job_type, domain::JobType::Scheduled);
        assert_eq!(jobs[0].status, domain::JobStatus::Completed);
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
        // Append publishes the raw dated sections → no synthesized structured fields.
        assert!(records[0].summary.key_points.is_empty());
    }

    #[cfg(feature = "http-llm")]
    #[test]
    fn rolling_intermediate_delivers_only_to_opted_in_destinations() {
        // ADR-108: during a rolling period, only destinations flagged
        // `rolling_deliver_intermediate` receive the in-progress digest; the
        // others wait for finalize. The finalized digest then goes to all.
        use crate::delivery::{Deliverer, RenderedSummary};
        use repository::{
            DestinationRepository, RollingConfig, RollingRepository, StoredDestination,
        };
        use serde_json::Value;
        use std::cell::RefCell;
        use std::rc::Rc;

        struct UrlSpy {
            seen: Rc<RefCell<Vec<String>>>,
        }
        impl Deliverer for UrlSpy {
            fn id(&self) -> &str {
                "webhook"
            }
            fn deliver(&self, config: &Value, _s: &RenderedSummary) -> Result<(), String> {
                self.seen.borrow_mut().push(
                    config.get("url").and_then(Value::as_str).unwrap_or("").to_string(),
                );
                Ok(())
            }
        }

        let master = [3u8; 32];
        let repo = SqliteRepository::in_memory().unwrap();
        let ws = WorkspaceId::parse("ws-1").unwrap();
        repo.create_schedule(&schedule_with_channel(&ws, 0)).unwrap();
        repo.set_rolling_config(
            "sch_1",
            &RollingConfig { period: "weekly".into(), strategy: "append".into(), end_day: 6 },
        )
        .unwrap();

        // Two webhook destinations: "live" opted into intermediate delivery, "final" not.
        for (id, url, intermediate) in [
            ("d_live", "https://live.example/hook", true),
            ("d_final", "https://final.example/hook", false),
        ] {
            let enc = crate::encrypt_secret(&master, &format!(r#"{{"url":"{url}"}}"#)).unwrap();
            repo.upsert_destination(
                &ws,
                &StoredDestination {
                    id: id.into(),
                    kind: "webhook".into(),
                    address_enc: Some(enc),
                    enabled: true,
                    created_at: 1,
                    rolling_deliver_intermediate: intermediate,
                },
            )
            .unwrap();
        }

        let tz = chrono_tz::UTC;
        let window = RollingPeriod::Weekly.window(1_767_700_000, tz, end_weekday(6)).unwrap();
        repo.save_message(&ws, &msg("m0", window.start + 500, "we shipped the release today"))
            .unwrap();

        let limiter = Arc::new(GlobalRateLimiter::new(RateLimitConfig::default()));
        let engine = ResilientLlm::new(FakeLlm, limiter);
        let l = ladder();
        let seen: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(vec![]));
        let deliverers: Vec<Box<dyn Deliverer>> = vec![Box::new(UrlSpy { seen: Rc::clone(&seen) })];
        let runner = SummarizingScheduleRunner::new(&repo, &engine, &l)
            .with_delivery(&deliverers, Some(master));
        let sched = repo.get_schedule(&ws, "sch_1").unwrap().unwrap();

        // Mid-period run (StartNew) → only the opted-in destination is hit.
        runner.run(&sched, window.start + 1_000).unwrap();
        assert_eq!(
            seen.borrow().clone(),
            vec!["https://live.example/hook".to_string()],
            "intermediate update goes only to the opted-in destination"
        );

        // Finalize → the digest goes to both destinations.
        seen.borrow_mut().clear();
        runner.run(&sched, window.end + 10).unwrap();
        let mut got = seen.borrow().clone();
        got.sort();
        assert_eq!(
            got,
            vec!["https://final.example/hook".to_string(), "https://live.example/hook".to_string()],
            "the finalized digest goes to all destinations"
        );
    }

    #[test]
    fn rolling_hybrid_finalize_synthesizes_a_structured_digest() {
        use repository::{RollingConfig, RollingRepository};
        let repo = SqliteRepository::in_memory().unwrap();
        let ws = WorkspaceId::parse("ws-1").unwrap();
        repo.create_schedule(&schedule_with_channel(&ws, 0)).unwrap();
        // Same flow as the append test, but with the Hybrid merge strategy.
        repo.set_rolling_config(
            "sch_1",
            &RollingConfig {
                period: "weekly".into(),
                strategy: "hybrid".into(),
                end_day: 6,
            },
        )
        .unwrap();

        let tz = chrono_tz::UTC;
        let window = RollingPeriod::Weekly
            .window(1_767_700_000, tz, end_weekday(6))
            .unwrap();
        repo.save_message(&ws, &msg("m0", window.start + 500, "we shipped the release today"))
            .unwrap();

        let limiter = Arc::new(GlobalRateLimiter::new(RateLimitConfig::default()));
        let engine = ResilientLlm::new(FakeLlm, limiter);
        let l = ladder();
        let runner = SummarizingScheduleRunner::new(&repo, &engine, &l);
        let sched = repo.get_schedule(&ws, "sch_1").unwrap().unwrap();

        runner.run(&sched, window.start + 1_000).unwrap(); // StartNew
        runner.run(&sched, window.end + 10).unwrap(); // Finalize

        let records = repo.list_records(&ws, false, 10).unwrap();
        assert_eq!(records.len(), 1);
        // Hybrid ran a synthesis pass at finalize → the digest carries the
        // synthesized structured fields (FakeLlm returns a key point + participant),
        // unlike the raw-concatenation Append path.
        assert!(!records[0].summary.key_points.is_empty(), "hybrid digest has key points");
        assert!(!records[0].summary.participants.is_empty(), "hybrid digest has participants");
        assert!(records[0].summary.text.contains("digest")); // header preserved
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

    #[cfg(feature = "http-llm")]
    #[test]
    fn schedule_destination_selection_restricts_delivery() {
        // ADR-014: a schedule may pin its delivery to a subset of the workspace's
        // destinations. With two webhook destinations configured but the schedule
        // restricted to one, only that one receives the summary.
        use crate::delivery::{Deliverer, RenderedSummary};
        use repository::{
            DestinationRepository, ScheduleDestinationRepository, StoredDestination,
        };
        use serde_json::Value;
        use std::cell::RefCell;
        use std::rc::Rc;

        /// Records the `url` of every config it's asked to deliver, into a shared
        /// buffer the test can inspect once the runner releases its borrow.
        struct UrlSpy {
            seen: Rc<RefCell<Vec<String>>>,
        }
        impl Deliverer for UrlSpy {
            fn id(&self) -> &str {
                "webhook"
            }
            fn deliver(&self, config: &Value, _summary: &RenderedSummary) -> Result<(), String> {
                let url = config
                    .get("url")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                self.seen.borrow_mut().push(url);
                Ok(())
            }
        }

        let master = [9u8; 32];
        let repo = SqliteRepository::in_memory().unwrap();
        let ws = WorkspaceId::parse("ws-1").unwrap();
        repo.save_message(&ws, &msg("m0", 3_500, "we shipped the release today"))
            .unwrap();
        repo.create_schedule(&schedule_with_channel(&ws, 3_600))
            .unwrap();

        // Two enabled webhook destinations.
        for (id, url) in [("d1", "https://one.example/hook"), ("d2", "https://two.example/hook")]
        {
            let enc = crate::encrypt_secret(&master, &format!(r#"{{"url":"{url}"}}"#)).unwrap();
            repo.upsert_destination(
                &ws,
                &StoredDestination {
                    id: id.into(),
                    kind: "webhook".into(),
                    address_enc: Some(enc),
                    enabled: true,
                    created_at: 1,
                    rolling_deliver_intermediate: false,
                },
            )
            .unwrap();
        }
        // Restrict the schedule to d1 only.
        repo.set_schedule_destinations("sch_1", &["d1".into()])
            .unwrap();

        let limiter = Arc::new(GlobalRateLimiter::new(RateLimitConfig::default()));
        let engine = ResilientLlm::new(FakeLlm, limiter);
        let l = ladder();
        let seen: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(vec![]));
        let deliverers: Vec<Box<dyn Deliverer>> = vec![Box::new(UrlSpy {
            seen: Rc::clone(&seen),
        })];
        let runner = SummarizingScheduleRunner::new(&repo, &engine, &l)
            .with_delivery(&deliverers, Some(master));

        SchedulerService::new(&repo).tick(&runner, 3_600).unwrap();

        // The deliverer ran exactly once, for d1's URL — d2 was filtered out.
        assert_eq!(
            seen.borrow().clone(),
            vec!["https://one.example/hook".to_string()]
        );
    }

    #[test]
    fn parse_category_recognizes_the_sentinel() {
        assert_eq!(super::parse_category("category:c-eng"), Some("c-eng"));
        assert_eq!(super::parse_category("category:"), None); // empty id
        assert_eq!(super::parse_category("c-eng"), None);
        assert_eq!(super::parse_category(super::ALL_CHANNELS), None);
    }

    #[test]
    fn read_across_merges_selected_channels_chronologically() {
        // The deterministic core of category scope (ADR-011): once the category's
        // channels are resolved, read+merge their messages in time order, leaving
        // out channels that aren't in the set.
        let repo = SqliteRepository::in_memory().unwrap();
        let ws = WorkspaceId::parse("ws-1").unwrap();
        let m = |id: &str, ch: &str, ts: i64| NormalizedMessage {
            id: domain::MessageId::parse(id).unwrap(),
            platform: Platform::Discord,
            channel_id: ChannelId::parse(ch).unwrap(),
            author_id: "p1".into(),
            author_name: "Alice".into(),
            content: format!("msg {id}"),
            timestamp: ts,
            is_system: false,
            reply_to: None,
            attachments: vec![],
        };
        repo.save_message(&ws, &m("a2", "c1", 200)).unwrap();
        repo.save_message(&ws, &m("a1", "c1", 100)).unwrap();
        repo.save_message(&ws, &m("b1", "c2", 150)).unwrap();
        repo.save_message(&ws, &m("z1", "c3", 120)).unwrap(); // outside the category

        let limiter = Arc::new(GlobalRateLimiter::new(RateLimitConfig::default()));
        let engine = ResilientLlm::new(FakeLlm, limiter);
        let l = ladder();
        let runner = SummarizingScheduleRunner::new(&repo, &engine, &l);

        let chans = vec![ChannelId::parse("c1").unwrap(), ChannelId::parse("c2").unwrap()];
        let got = runner.read_across(&ws, &chans, 0, 1_000).unwrap();
        let ids: Vec<&str> = got.iter().map(|x| x.id.as_str()).collect();
        // c1 + c2 only, merged in timestamp order; c3 excluded.
        assert_eq!(ids, vec!["a1", "b1", "a2"]);
    }

    #[test]
    fn category_scope_without_a_source_is_a_quiet_noop() {
        // A category-scoped schedule with no bound Discord source can't resolve
        // its channels, so it produces nothing rather than failing.
        let repo = SqliteRepository::in_memory().unwrap();
        let ws = WorkspaceId::parse("ws-1").unwrap();
        repo.save_message(&ws, &msg("m0", 3_500, "we shipped the release today"))
            .unwrap();
        let schedule = Schedule::build(
            ws.clone(), "hourly", 0, 0, &[], 1, "UTC", None, 0, true, Some("category:c-eng"), 100_000,
        )
        .unwrap();
        repo.create_schedule(&StoredSchedule {
            id: "sch_cat".into(),
            schedule,
            next_run: 3_600,
            consecutive_failures: 0,
        })
        .unwrap();

        let limiter = Arc::new(GlobalRateLimiter::new(RateLimitConfig::default()));
        let engine = ResilientLlm::new(FakeLlm, limiter);
        let l = ladder();
        let runner = SummarizingScheduleRunner::new(&repo, &engine, &l);

        let report = SchedulerService::new(&repo).tick(&runner, 3_600).unwrap();
        assert_eq!(report.fired, 1);
        assert!(repo.list_records(&ws, false, 10).unwrap().is_empty());
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
