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

/// What kind of work a job represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobType {
    /// Generate a summary.
    Summarization,
    /// (Re)ingest historical messages (DAT-003..006).
    Backfill,
}

impl JobType {
    pub fn as_str(self) -> &'static str {
        match self {
            JobType::Summarization => "summarization",
            JobType::Backfill => "backfill",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "summarization" => Some(JobType::Summarization),
            "backfill" => Some(JobType::Backfill),
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

/// A tracked job.
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
        }
    }

    /// Pending → Running.
    pub fn start(&mut self, now: i64) -> Result<(), InvalidTransition> {
        self.expect(JobStatus::Pending, "start")?;
        self.set(JobStatus::Running, now);
        Ok(())
    }

    /// Update progress while Running (no-op guard elsewhere).
    pub fn set_progress(&mut self, current: u32, total: u32, now: i64) {
        self.progress_current = current;
        self.progress_total = total;
        self.updated_at = now;
    }

    /// Running → Completed, recording final cost.
    pub fn complete(&mut self, cost_micros: i64, now: i64) -> Result<(), InvalidTransition> {
        self.expect(JobStatus::Running, "complete")?;
        self.cost_micros = cost_micros.max(0);
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
        self.set(JobStatus::Failed, now);
        Ok(())
    }

    /// Running → Paused (used by restart recovery — ADR-013).
    pub fn pause(&mut self, now: i64) -> Result<(), InvalidTransition> {
        self.expect(JobStatus::Running, "pause")?;
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
        for t in [JobType::Summarization, JobType::Backfill] {
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
