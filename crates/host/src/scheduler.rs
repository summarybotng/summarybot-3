//! Scheduler executor (PRD §3.1 SCH-005/006) — host orchestration over the pure
//! recurrence + tick policy and the schedule store.
//!
//! [`SchedulerService::tick`] is one pass of the scheduler: restore enabled
//! schedules, evaluate each against the clock, fire the due ones via a
//! [`ScheduleRunner`], and persist the new run state (advance `next_run`, bump
//! failures, auto-disable). It is synchronous and deterministic (clock injected),
//! so the whole policy is testable; the periodic driver that calls `tick` on an
//! interval is the server's async loop. A `Once` that has fired (no next run) is
//! auto-disabled.

use domain::{evaluate_tick, TickAction};
use repository::{ScheduleRepository, StoredSchedule};

/// Runs the actual summary for a due schedule (resolve scope → summarize →
/// deliver). Abstracted so the scheduler is testable without an LLM/network; the
/// real impl wires `SummarizationService` + `DeliveryService`.
pub trait ScheduleRunner {
    /// Returns `Ok` on success, `Err(reason)` on failure (drives the
    /// consecutive-failure count and eventual auto-disable).
    fn run(&self, stored: &StoredSchedule, now: i64) -> Result<(), String>;
}

/// Counts of what a tick did (for telemetry/tests).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TickReport {
    pub fired: u32,
    pub skipped: u32,
    pub failed: u32,
    pub disabled: u32,
}

/// Drives schedules over the store + recurrence policy.
pub struct SchedulerService<'a, R> {
    repo: &'a R,
    grace_secs: i64,
    max_failures: u32,
}

impl<'a, R: ScheduleRepository> SchedulerService<'a, R> {
    /// Defaults: 5-minute grace, auto-disable after 5 consecutive failures.
    pub fn new(repo: &'a R) -> Self {
        Self {
            repo,
            grace_secs: 300,
            max_failures: 5,
        }
    }

    pub fn with_policy(mut self, grace_secs: i64, max_failures: u32) -> Self {
        self.grace_secs = grace_secs;
        self.max_failures = max_failures;
        self
    }

    /// One scheduler pass at `now`.
    pub fn tick(&self, runner: &dyn ScheduleRunner, now: i64) -> anyhow::Result<TickReport> {
        let mut report = TickReport::default();
        for stored in self.repo.list_enabled()? {
            match evaluate_tick(
                stored.next_run,
                now,
                self.grace_secs,
                stored.consecutive_failures,
                self.max_failures,
            ) {
                TickAction::NotYet => {}
                TickAction::Disable => {
                    self.repo.update_runtime(
                        &stored.id,
                        stored.next_run,
                        stored.consecutive_failures,
                        false,
                    )?;
                    report.disabled += 1;
                }
                TickAction::Skip => {
                    // Missed run beyond grace: advance without firing (catch-up).
                    self.reschedule(&stored, now, stored.consecutive_failures)?;
                    report.skipped += 1;
                }
                TickAction::Fire => {
                    let failures = match runner.run(&stored, now) {
                        Ok(()) => {
                            report.fired += 1;
                            0
                        }
                        Err(_) => {
                            report.failed += 1;
                            stored.consecutive_failures + 1
                        }
                    };
                    self.reschedule(&stored, now, failures)?;
                }
            }
        }
        Ok(report)
    }

    /// Advance to the next run after `now`; a schedule with no future run (a
    /// fired `Once`) is disabled.
    fn reschedule(&self, stored: &StoredSchedule, now: i64, failures: u32) -> anyhow::Result<()> {
        match stored.schedule.next_run(now) {
            Some(next) => self.repo.update_runtime(&stored.id, next, failures, true)?,
            None => self
                .repo
                .update_runtime(&stored.id, stored.next_run, failures, false)?,
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{Schedule, ScheduleType, TimeOfDay, WorkspaceId};
    use repository::SqliteRepository;
    use std::cell::RefCell;

    fn repo() -> SqliteRepository {
        SqliteRepository::in_memory().unwrap()
    }

    fn ws() -> WorkspaceId {
        WorkspaceId::parse("ws-1").unwrap()
    }

    fn hourly(id: &str, next_run: i64, failures: u32) -> StoredSchedule {
        StoredSchedule {
            id: id.into(),
            schedule: Schedule {
                workspace_id: ws(),
                schedule_type: ScheduleType::Hourly,
                at: TimeOfDay { hour: 0, minute: 0 },
                days: vec![],
                day_of_month: 1,
                timezone: chrono_tz::UTC,
                once_at: None,
                custom_interval_secs: 0,
                enabled: true,
                channel: None,
                lookback_secs: 86_400,
            },
            next_run,
            consecutive_failures: failures,
        }
    }

    struct OkRunner {
        ran: RefCell<u32>,
    }
    impl ScheduleRunner for OkRunner {
        fn run(&self, _s: &StoredSchedule, _now: i64) -> Result<(), String> {
            *self.ran.borrow_mut() += 1;
            Ok(())
        }
    }

    struct FailRunner;
    impl ScheduleRunner for FailRunner {
        fn run(&self, _s: &StoredSchedule, _now: i64) -> Result<(), String> {
            Err("boom".into())
        }
    }

    #[test]
    fn fires_due_schedule_and_advances_next_run() {
        let repo = repo();
        repo.create_schedule(&hourly("h", 3_600, 0)).unwrap();
        let runner = OkRunner {
            ran: RefCell::new(0),
        };
        let report = SchedulerService::new(&repo).tick(&runner, 3_600).unwrap();
        assert_eq!(report.fired, 1);
        assert_eq!(*runner.ran.borrow(), 1);
        // next_run advanced past now to the next hour boundary.
        let got = repo.get_schedule(&ws(), "h").unwrap().unwrap();
        assert_eq!(got.next_run, 7_200);
        assert_eq!(got.consecutive_failures, 0);
    }

    #[test]
    fn not_yet_schedule_does_not_fire() {
        let repo = repo();
        repo.create_schedule(&hourly("h", 10_000, 0)).unwrap();
        let runner = OkRunner {
            ran: RefCell::new(0),
        };
        let report = SchedulerService::new(&repo).tick(&runner, 5_000).unwrap();
        assert_eq!(report, TickReport::default());
        assert_eq!(*runner.ran.borrow(), 0);
    }

    #[test]
    fn stale_missed_run_is_skipped_and_rescheduled() {
        let repo = repo();
        // Due at 3600 but now is far past the grace window.
        repo.create_schedule(&hourly("h", 3_600, 0)).unwrap();
        let runner = OkRunner {
            ran: RefCell::new(0),
        };
        let report = SchedulerService::new(&repo).tick(&runner, 100_000).unwrap();
        assert_eq!(report.skipped, 1);
        assert_eq!(*runner.ran.borrow(), 0); // not fired (stale)
        let got = repo.get_schedule(&ws(), "h").unwrap().unwrap();
        assert!(got.next_run > 100_000); // advanced to the future
    }

    #[test]
    fn failures_accumulate_then_auto_disable() {
        let repo = repo();
        // Already at the failure threshold (5) → this tick disables before firing.
        repo.create_schedule(&hourly("h", 3_600, 5)).unwrap();
        let report = SchedulerService::new(&repo)
            .tick(&FailRunner, 3_600)
            .unwrap();
        assert_eq!(report.disabled, 1);
        assert!(repo.list_enabled().unwrap().is_empty());
    }

    #[test]
    fn a_failing_run_bumps_the_failure_count() {
        let repo = repo();
        repo.create_schedule(&hourly("h", 3_600, 0)).unwrap();
        SchedulerService::new(&repo)
            .tick(&FailRunner, 3_600)
            .unwrap();
        let got = repo.get_schedule(&ws(), "h").unwrap().unwrap();
        assert_eq!(got.consecutive_failures, 1);
    }

    #[test]
    fn once_disables_after_firing() {
        let repo = repo();
        let mut once = hourly("o", 3_600, 0);
        once.schedule.schedule_type = ScheduleType::Once;
        once.schedule.once_at = Some(3_600);
        repo.create_schedule(&once).unwrap();
        let runner = OkRunner {
            ran: RefCell::new(0),
        };
        SchedulerService::new(&repo).tick(&runner, 3_600).unwrap();
        assert_eq!(*runner.ran.borrow(), 1);
        // No next run for a fired Once → disabled.
        assert!(repo.list_enabled().unwrap().is_empty());
    }
}
