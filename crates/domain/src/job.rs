//! Job lifecycle (PRD §5.3, §12.4; ADR-013).
//!
//! A long-running unit of work (a summarization, a backfill) is recorded
//! **before** the async work starts, so a crash never loses the fact that it was
//! requested. The status is a small state machine; on restart, anything left
//! `Running` is moved to `Paused` (the process that owned it is gone). Failures
//! persist a classified `failure_reason` (LEG-002), not a generic "failed".

use crate::{string_id, FailureClass, WorkspaceId};

string_id!(
    /// Server-issued job id (e.g. `job_<hex>`).
    JobId,
    "job id",
    128
);

/// What kind of work a job represents (ADR-013/040 — the Jobs view tracks every
/// long-running producer, not just summarization).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobType {
    /// Generate a summary on demand (the manual "Summarize now" path).
    Summarization,
    /// A scheduled summary run fired by the scheduler (or a manual trigger).
    Scheduled,
    /// (Re)ingest historical messages (DAT-003..006) — retrospective by-week.
    Backfill,
    /// Fetch + persist messages from a connected platform (ADR-128).
    Sync,
    /// Synthesize the knowledge-base wiki page from stored units (ADR-067).
    WikiSynthesis,
    /// Re-run a stored summary over the same source window with changed
    /// parameters (perspective/length) — ADR-133.
    Regenerate,
}

impl JobType {
    pub fn as_str(self) -> &'static str {
        match self {
            JobType::Summarization => "summarization",
            JobType::Scheduled => "scheduled",
            JobType::Backfill => "backfill",
            JobType::Sync => "sync",
            JobType::WikiSynthesis => "wiki_synthesis",
            JobType::Regenerate => "regenerate",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "summarization" => Some(JobType::Summarization),
            "scheduled" => Some(JobType::Scheduled),
            "backfill" => Some(JobType::Backfill),
            "sync" => Some(JobType::Sync),
            "wiki_synthesis" => Some(JobType::WikiSynthesis),
            "regenerate" => Some(JobType::Regenerate),
            _ => None,
        }
    }
}

/// Lifecycle states (ADR-013).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    /// Recorded, not yet started (record-before-async).
    Pending,
    /// Actively running.
    Running,
    /// Was running when the process restarted; awaiting resume/cleanup.
    Paused,
    /// Finished successfully.
    Completed,
    /// Finished with a classified failure.
    Failed,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Pending => "pending",
            JobStatus::Running => "running",
            JobStatus::Paused => "paused",
            JobStatus::Completed => "completed",
            JobStatus::Failed => "failed",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "pending" => Some(JobStatus::Pending),
            "running" => Some(JobStatus::Running),
            "paused" => Some(JobStatus::Paused),
            "completed" => Some(JobStatus::Completed),
            "failed" => Some(JobStatus::Failed),
            _ => None,
        }
    }

    /// Terminal states never transition further.
    pub fn is_terminal(self) -> bool {
        matches!(self, JobStatus::Completed | JobStatus::Failed)
    }
}

/// An illegal lifecycle transition was attempted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidTransition {
    pub from: JobStatus,
    pub action: &'static str,
}

/// A tracked job. Beyond the lifecycle core, jobs carry v2-parity context
/// (ADR-133 §B) so the Jobs view can show *what* ran: scope, the originating
/// schedule, the covered window, the produced summaries, and timing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: JobId,
    pub workspace_id: WorkspaceId,
    pub job_type: JobType,
    pub status: JobStatus,
    pub progress_current: u32,
    pub progress_total: u32,
    pub cost_micros: i64,
    /// Classified failure reason (LEG-002 `as_str`), set when `Failed`.
    pub failure_reason: Option<String>,
    /// Unix seconds; supplied by the host.
    pub created_at: i64,
    pub updated_at: i64,
    // --- ADR-133 §B context ---
    /// Human scope label, e.g. `channel #c1` / `workspace-wide` / `on-demand`.
    pub scope: Option<String>,
    /// Originating schedule's display name (scheduled/rolling runs).
    pub schedule_name: Option<String>,
    /// Ids of the summaries this job produced (links from the Jobs view).
    pub summary_ids: Vec<String>,
    /// Covered message-time window, `0` when not applicable.
    pub date_start: i64,
    pub date_end: i64,
    /// When the job moved to `Running` (for duration), and to a terminal state.
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    /// What triggered it, e.g. `manual` / `scheduler` / `api`.
    pub creation_source: Option<String>,
    /// Why it was paused (restart recovery — ADR-013).
    pub pause_reason: Option<String>,
}

impl Job {
    /// Record a new job in `Pending` (before any async work — ADR-013).
    pub fn record(id: JobId, workspace_id: WorkspaceId, job_type: JobType, now: i64) -> Self {
        Self {
            id,
            workspace_id,
            job_type,
            status: JobStatus::Pending,
            progress_current: 0,
            progress_total: 0,
            cost_micros: 0,
            failure_reason: None,
            created_at: now,
            updated_at: now,
            scope: None,
            schedule_name: None,
            summary_ids: Vec::new(),
            date_start: 0,
            date_end: 0,
            started_at: None,
            completed_at: None,
            creation_source: None,
            pause_reason: None,
        }
    }

    /// Builder: set the scope label (e.g. `channel #c1`).
    pub fn with_scope(mut self, scope: impl Into<String>) -> Self {
        self.scope = Some(scope.into());
        self
    }

    /// Builder: set the originating schedule's name.
    pub fn with_schedule_name(mut self, name: impl Into<String>) -> Self {
        self.schedule_name = Some(name.into());
        self
    }

    /// Builder: set the covered message-time window.
    pub fn with_date_range(mut self, start: i64, end: i64) -> Self {
        self.date_start = start;
        self.date_end = end;
        self
    }

    /// Builder: set what triggered the job.
    pub fn with_creation_source(mut self, source: impl Into<String>) -> Self {
        self.creation_source = Some(source.into());
        self
    }

    /// Record a summary id this job produced.
    pub fn add_summary_id(&mut self, id: impl Into<String>) {
        self.summary_ids.push(id.into());
    }

    /// Pending → Running (stamps `started_at`).
    pub fn start(&mut self, now: i64) -> Result<(), InvalidTransition> {
        self.expect(JobStatus::Pending, "start")?;
        self.started_at = Some(now);
        self.set(JobStatus::Running, now);
        Ok(())
    }

    /// Update progress while Running (no-op guard elsewhere).
    pub fn set_progress(&mut self, current: u32, total: u32, now: i64) {
        self.progress_current = current;
        self.progress_total = total;
        self.updated_at = now;
    }

    /// Running → Completed, recording final cost (stamps `completed_at`).
    pub fn complete(&mut self, cost_micros: i64, now: i64) -> Result<(), InvalidTransition> {
        self.expect(JobStatus::Running, "complete")?;
        self.cost_micros = cost_micros.max(0);
        self.completed_at = Some(now);
        self.set(JobStatus::Completed, now);
        Ok(())
    }

    /// Running → Failed with a classified reason (LEG-002) and the cost so far.
    pub fn fail(
        &mut self,
        reason: FailureClass,
        cost_micros: i64,
        now: i64,
    ) -> Result<(), InvalidTransition> {
        self.expect(JobStatus::Running, "fail")?;
        self.failure_reason = Some(reason.as_str().to_string());
        self.cost_micros = cost_micros.max(0);
        self.completed_at = Some(now);
        self.set(JobStatus::Failed, now);
        Ok(())
    }

    /// Running → Paused with a reason (used by restart recovery — ADR-013).
    pub fn pause(&mut self, now: i64) -> Result<(), InvalidTransition> {
        self.expect(JobStatus::Running, "pause")?;
        self.pause_reason = Some("process restarted while running".to_string());
        self.set(JobStatus::Paused, now);
        Ok(())
    }

    fn expect(&self, want: JobStatus, action: &'static str) -> Result<(), InvalidTransition> {
        if self.status == want {
            Ok(())
        } else {
            Err(InvalidTransition {
                from: self.status,
                action,
            })
        }
    }

    fn set(&mut self, status: JobStatus, now: i64) {
        self.status = status;
        self.updated_at = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job() -> Job {
        Job::record(
            JobId::parse("job_1").unwrap(),
            WorkspaceId::parse("ws-1").unwrap(),
            JobType::Summarization,
            1_000,
        )
    }

    #[test]
    fn happy_path_pending_running_completed() {
        let mut j = job();
        assert_eq!(j.status, JobStatus::Pending);
        j.start(1_001).unwrap();
        assert_eq!(j.status, JobStatus::Running);
        j.set_progress(3, 10, 1_002);
        j.complete(42_000, 1_003).unwrap();
        assert_eq!(j.status, JobStatus::Completed);
        assert_eq!(j.cost_micros, 42_000);
        assert_eq!(j.updated_at, 1_003);
    }

    #[test]
    fn failure_records_classified_reason() {
        let mut j = job();
        j.start(1).unwrap();
        j.fail(FailureClass::RateLimited, 5_000, 2).unwrap();
        assert_eq!(j.status, JobStatus::Failed);
        assert_eq!(j.failure_reason.as_deref(), Some("rate_limited"));
        assert_eq!(j.cost_micros, 5_000);
    }

    #[test]
    fn running_pauses_on_restart() {
        let mut j = job();
        j.start(1).unwrap();
        j.pause(2).unwrap();
        assert_eq!(j.status, JobStatus::Paused);
    }

    #[test]
    fn illegal_transitions_are_rejected() {
        let mut j = job();
        // Can't complete before starting.
        assert_eq!(
            j.complete(0, 1),
            Err(InvalidTransition {
                from: JobStatus::Pending,
                action: "complete"
            })
        );
        // Can't start twice.
        j.start(1).unwrap();
        j.complete(0, 2).unwrap();
        assert!(j.start(3).is_err());
        assert!(j.status.is_terminal());
    }

    #[test]
    fn type_and_status_round_trip() {
        for t in [
            JobType::Summarization,
            JobType::Scheduled,
            JobType::Backfill,
            JobType::Sync,
            JobType::WikiSynthesis,
            JobType::Regenerate,
        ] {
            assert_eq!(JobType::parse(t.as_str()), Some(t));
        }
        for s in [
            JobStatus::Pending,
            JobStatus::Running,
            JobStatus::Paused,
            JobStatus::Completed,
            JobStatus::Failed,
        ] {
            assert_eq!(JobStatus::parse(s.as_str()), Some(s));
        }
    }
}
