//! Background scheduler driver (PRD §12.7) — the async glue that makes a booted
//! server actually fire schedules.
//!
//! A tokio task ticks on an interval; each tick locks the repo and runs one
//! [`SchedulerService::tick`] (which restores enabled schedules, fires the due
//! ones, advances/skips/disables — SCH-005/006). The per-tick work is the
//! testable [`run_one_tick`]; only the interval loop is untested glue.
//!
//! Firing is delegated to [`TenantAwareRunner`], which — per schedule — resolves
//! the owning tenant's LLM backend and budget from the shared [`AppState`]
//! (ADR-125), exactly as a manual trigger does. So a tenant's scheduled digest
//! honors its BYO endpoint/model and is metered against (and gated by) its
//! budget, rather than silently using the process default.

use crate::auth::now_secs;
use crate::AppState;
use host::llm::ResilientLlm;
use host::{ScheduleRunner, SchedulerService, SummarizingScheduleRunner, TickReport};
use repository::{SqliteRepository, StoredSchedule, StructuredSummaryRepository};
use std::time::Duration;

/// Spawn the background scheduler loop. Returns immediately; the task runs until
/// the server exits.
pub fn spawn_scheduler(state: AppState, interval_secs: u64) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs.max(1)));
        loop {
            ticker.tick().await;
            match run_one_tick(&state, now_secs()) {
                Ok(report) if report.fired + report.skipped + report.disabled > 0 => {
                    eprintln!("scheduler: {report:?}");
                }
                Ok(_) => {}
                Err(e) => eprintln!("scheduler tick error: {e}"),
            }
        }
    });
}

/// One scheduler pass against the shared state. Sync (the repo is sync, and the
/// demo LLM does no I/O); no `await` is held across the repo lock. Each due
/// schedule resolves its own tenant's LLM + budget (ADR-125), exactly like a
/// manual trigger — so a tenant's scheduled digest uses its BYO endpoint/model
/// and is metered against its budget, not the process default.
pub(crate) fn run_one_tick(state: &AppState, now: i64) -> anyhow::Result<TickReport> {
    let repo = state.repo.lock().expect("repo mutex");
    let deliverers = state.deliverers();
    let runner = TenantAwareRunner {
        state,
        repo: &repo,
        deliverers: &deliverers,
    };
    SchedulerService::new(&*repo).tick(&runner, now)
}

/// A [`ScheduleRunner`] that resolves the LLM backend + budget per schedule (by
/// its workspace's tenant) before summarizing — the scheduler-side equivalent of
/// `schedules::trigger_schedule`. Without this, scheduled runs silently used the
/// process-default model and skipped tenant budgets.
struct TenantAwareRunner<'a> {
    state: &'a AppState,
    repo: &'a SqliteRepository,
    deliverers: &'a [Box<dyn host::Deliverer>],
}

impl ScheduleRunner for TenantAwareRunner<'_> {
    fn run(&self, stored: &StoredSchedule, now: i64) -> Result<(), String> {
        let ws = stored.schedule.workspace_id.clone();
        let resolution =
            crate::resolve_llm(self.repo, &ws, &self.state.model, self.state.master_key());

        // Budget gate (ADR-125 Phase 3): if the tenant is over budget, skip this
        // tick without firing — a transient condition that rolls with the window,
        // not a failure that should accumulate toward auto-disable.
        let charge = match crate::budget_gate(self.repo, &resolution, now) {
            Ok(c) => c,
            Err(_) => {
                eprintln!(
                    "scheduler: skipping {} — tenant budget exhausted",
                    stored.id
                );
                return Ok(());
            }
        };

        let client = self
            .state
            .client_for_base(resolution.base_url.clone(), resolution.api_key.clone());
        let engine = ResilientLlm::new(client, self.state.limiter.clone());
        let ladder = self.state.ladder_for(&resolution.model);
        let inner = SummarizingScheduleRunner::new(self.repo, &engine, &ladder)
            .with_delivery(self.deliverers, self.state.master_key().copied());
        inner.run(stored, now)?;

        // Charge the tenant's budget by what the produced summary cost (if one was
        // produced — the runner stores it under this deterministic id).
        if let Some((tenant, window)) = charge {
            if let Ok(Some(rec)) = self
                .repo
                .get_record(&ws, &format!("sum_{}_{}", stored.id, now))
            {
                let _ = crate::budget_charge(self.repo, &tenant, window, rec.cost_micros);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{ChannelId, NormalizedMessage, Platform, Schedule, WorkspaceId};
    use repository::{ScheduleRepository, WhatsAppRepository};

    #[test]
    fn one_tick_fires_a_due_schedule_and_stores_a_summary() {
        let repo = SqliteRepository::in_memory().unwrap();
        let ws = WorkspaceId::parse("ws-1").unwrap();

        // A substantial message in channel c1.
        repo.save_message(
            &ws,
            &NormalizedMessage {
                id: domain::MessageId::parse("m0").unwrap(),
                platform: Platform::WhatsApp,
                channel_id: ChannelId::parse("c1").unwrap(),
                author_id: "alice".into(),
                author_name: "Alice".into(),
                content: "we shipped the release today".into(),
                timestamp: 3_500,
                is_system: false,
                reply_to: None,
                attachments: vec![],
            },
        )
        .unwrap();

        // An hourly schedule scoped to c1, due at 3600.
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
        repo.create_schedule(&StoredSchedule {
            id: "sch_1".into(),
            schedule,
            next_run: 3_600,
            consecutive_failures: 0,
        })
        .unwrap();

        let state = AppState::new(repo, domain::Secret::new(b"k".to_vec()));
        let report = run_one_tick(&state, 3_600).unwrap();
        assert_eq!(report.fired, 1);

        // The produced summary is in the dashboard store.
        let repo = state.repo.lock().unwrap();
        let summaries = repo.list_records(&ws, false, 10).unwrap();
        assert_eq!(summaries.len(), 1);
        assert!(!summaries[0].summary.participants.is_empty());
    }

    #[test]
    fn one_tick_with_no_schedules_is_a_noop() {
        let repo = SqliteRepository::in_memory().unwrap();
        let state = AppState::new(repo, domain::Secret::new(b"k".to_vec()));
        let report = run_one_tick(&state, 3_600).unwrap();
        assert_eq!(report, TickReport::default());
    }

    /// Seed one substantial message in `c1` and an hourly schedule scoped to it,
    /// both for `ws`, due at 3600.
    fn seed_due_schedule(repo: &SqliteRepository, ws: &WorkspaceId) {
        repo.save_message(
            ws,
            &NormalizedMessage {
                id: domain::MessageId::parse("m0").unwrap(),
                platform: Platform::WhatsApp,
                channel_id: ChannelId::parse("c1").unwrap(),
                author_id: "alice".into(),
                author_name: "Alice".into(),
                content: "we shipped the release today".into(),
                timestamp: 3_500,
                is_system: false,
                reply_to: None,
                attachments: vec![],
            },
        )
        .unwrap();
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
        repo.create_schedule(&StoredSchedule {
            id: "sch_1".into(),
            schedule,
            next_run: 3_600,
            consecutive_failures: 0,
        })
        .unwrap();
    }

    #[test]
    fn over_budget_tenant_schedule_is_skipped_not_fired() {
        use repository::{BudgetRepository, BudgetRow, WorkspaceRepository};
        let repo = SqliteRepository::in_memory().unwrap();
        let ws = WorkspaceId::parse("ws-1").unwrap();
        let tenant = domain::TenantId::parse("acme").unwrap();
        // ws-1 belongs to acme, and acme is out of budget for this window.
        repo.create_workspace(
            &domain::Workspace::create(
                ws.clone(),
                tenant.clone(),
                "Eng",
                domain::UserId::parse("u1").unwrap(),
                10,
            )
            .unwrap(),
        )
        .unwrap();
        repo.upsert_budget(
            &tenant,
            &BudgetRow {
                limit_micros: 1_000,
                period_secs: 1_000_000,
                period_start: 3_600,
                spent_micros: 2_000, // already over the limit
            },
        )
        .unwrap();
        seed_due_schedule(&repo, &ws);

        // A non-zero price so a fired run *would* be metered (proving the skip is
        // the budget gate, not a zero-cost no-op).
        let state = AppState::new(repo, domain::Secret::new(b"k".to_vec())).with_price(1_000_000);
        let report = run_one_tick(&state, 3_600).unwrap();
        // Skipped-without-firing surfaces as Ok (no consecutive-failure bump), but
        // no summary is produced.
        assert_eq!(report.fired, 1);
        let repo = state.repo.lock().unwrap();
        assert!(
            repo.list_records(&ws, false, 10).unwrap().is_empty(),
            "an over-budget tenant must not get a scheduled summary"
        );
    }

    #[test]
    fn within_budget_tenant_run_is_metered() {
        use repository::{BudgetRepository, BudgetRow, WorkspaceRepository};
        let repo = SqliteRepository::in_memory().unwrap();
        let ws = WorkspaceId::parse("ws-1").unwrap();
        let tenant = domain::TenantId::parse("acme").unwrap();
        repo.create_workspace(
            &domain::Workspace::create(
                ws.clone(),
                tenant.clone(),
                "Eng",
                domain::UserId::parse("u1").unwrap(),
                10,
            )
            .unwrap(),
        )
        .unwrap();
        repo.upsert_budget(
            &tenant,
            &BudgetRow {
                limit_micros: 1_000_000_000,
                period_secs: 1_000_000,
                period_start: 3_600,
                spent_micros: 0,
            },
        )
        .unwrap();
        seed_due_schedule(&repo, &ws);

        let state = AppState::new(repo, domain::Secret::new(b"k".to_vec())).with_price(1_000_000);
        let report = run_one_tick(&state, 3_600).unwrap();
        assert_eq!(report.fired, 1);

        let repo = state.repo.lock().unwrap();
        // The summary was produced...
        assert_eq!(repo.list_records(&ws, false, 10).unwrap().len(), 1);
        // ...and the tenant's budget was charged for it.
        let spent = repo.get_budget(&tenant).unwrap().unwrap().spent_micros;
        assert!(spent > 0, "a fired scheduled run must draw down the budget");
    }
}
