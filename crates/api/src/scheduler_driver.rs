//! Background scheduler driver (PRD §12.7) — the async glue that makes a booted
//! server actually fire schedules.
//!
//! A tokio task ticks on an interval; each tick locks the repo, builds the
//! [`SummarizingScheduleRunner`] over the live services, and runs one
//! [`SchedulerService::tick`] (which restores enabled schedules, fires the due
//! ones, advances/skips/disables — SCH-005/006). The per-tick work is the
//! testable [`run_one_tick`]; only the interval loop is untested glue.
//!
//! The LLM backend and rate limiter come from the shared [`AppState`]: the
//! deterministic demo client by default, or OpenRouter when the server is
//! configured for it (`main.rs`) — the same one the on-demand endpoint uses.

use crate::auth::now_secs;
use crate::AppState;
use domain::summarize::ModelLadder;
use host::llm::{LlmClient, ResilientLlm};
use host::{SchedulerService, SummarizingScheduleRunner, TickReport};
use std::time::Duration;

/// Spawn the background scheduler loop. Returns immediately; the task runs until
/// the server exits.
pub fn spawn_scheduler(state: AppState, interval_secs: u64) {
    tokio::spawn(async move {
        // Built once over the shared backend + limiter; reused every tick.
        let engine = ResilientLlm::new(state.llm.clone(), state.limiter.clone());
        let ladder = state.model_ladder();
        let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs.max(1)));
        loop {
            ticker.tick().await;
            match run_one_tick(&state, &engine, &ladder, now_secs()) {
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
/// demo LLM does no I/O); no `await` is held across the repo lock.
pub(crate) fn run_one_tick<C: LlmClient>(
    state: &AppState,
    engine: &ResilientLlm<C>,
    ladder: &ModelLadder,
    now: i64,
) -> anyhow::Result<TickReport> {
    let repo = state.repo.lock().expect("repo mutex");
    let runner = SummarizingScheduleRunner::new(&*repo, engine, ladder);
    SchedulerService::new(&*repo).tick(&runner, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::summarize::{Model, ModelPrice};
    use domain::{ChannelId, NormalizedMessage, Platform, Schedule, WorkspaceId};
    use host::llm::{DemoLlmClient, GlobalRateLimiter, RateLimitConfig};
    use repository::{
        ScheduleRepository, SqliteRepository, StoredSchedule, StructuredSummaryRepository,
        WhatsAppRepository,
    };
    use std::sync::Arc;

    fn engine() -> ResilientLlm<DemoLlmClient> {
        ResilientLlm::new(
            DemoLlmClient,
            Arc::new(GlobalRateLimiter::new(RateLimitConfig::default())),
        )
    }

    fn ladder() -> ModelLadder {
        ModelLadder::new(vec![Model {
            name: "demo".into(),
            price: ModelPrice {
                input_micros_per_ktoken: 0,
                output_micros_per_ktoken: 0,
            },
            context_tokens: 200_000,
        }])
    }

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
        let report = run_one_tick(&state, &engine(), &ladder(), 3_600).unwrap();
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
        let report = run_one_tick(&state, &engine(), &ladder(), 3_600).unwrap();
        assert_eq!(report, TickReport::default());
    }
}
