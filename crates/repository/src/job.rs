//! Job persistence (PRD §5.3; ADR-013).
//!
//! Jobs are recorded before async work and updated through their lifecycle, so
//! progress, cost and a classified `failure_reason` survive crashes. On startup
//! [`JobRepository::pause_running`] moves every still-`Running` job to `Paused`
//! — the process that owned them is gone (ADR-013 restart recovery).

use crate::SqliteRepository;
use anyhow::{Context, Result};
use domain::{Job, JobId, JobStatus, JobType, WorkspaceId};
use rusqlite::{params, OptionalExtension};

/// Storage boundary for jobs.
pub trait JobRepository {
    /// Insert a freshly recorded (`Pending`) job.
    fn create_job(&self, job: &Job) -> Result<()>;
    /// Persist the full current state of a job (status/progress/cost/reason).
    fn update_job(&self, job: &Job) -> Result<()>;
    fn get_job(&self, workspace: &WorkspaceId, id: &JobId) -> Result<Option<Job>>;
    /// A workspace's jobs, newest first, capped at `limit` (the Jobs view).
    fn list_jobs(&self, workspace: &WorkspaceId, limit: u32) -> Result<Vec<Job>>;
    /// Restart recovery: move all `Running` jobs to `Paused`. Returns the count.
    fn pause_running(&self, now: i64) -> Result<u64>;
}

/// All job columns in a fixed order, shared by `get_job` and `list_jobs`.
const JOB_COLS: &str = "id, workspace_id, job_type, status, progress_current, progress_total, \
     cost_micros, failure_reason, created_at, updated_at, scope, schedule_name, summary_ids, \
     date_start, date_end, started_at, completed_at, creation_source, pause_reason";

/// Map a row selecting [`JOB_COLS`] into a [`Job`].
fn row_to_job(row: &rusqlite::Row) -> Result<Job> {
    let ty: String = row.get(2)?;
    let st: String = row.get(3)?;
    let summary_ids: String = row.get(12)?;
    Ok(Job {
        id: JobId::parse(row.get::<_, String>(0)?).map_err(anyhow::Error::new)?,
        workspace_id: WorkspaceId::parse(row.get::<_, String>(1)?).map_err(anyhow::Error::new)?,
        job_type: JobType::parse(&ty).context("unknown job_type in storage")?,
        status: JobStatus::parse(&st).context("unknown job status in storage")?,
        progress_current: row.get(4)?,
        progress_total: row.get(5)?,
        cost_micros: row.get(6)?,
        failure_reason: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
        scope: row.get(10)?,
        schedule_name: row.get(11)?,
        summary_ids: if summary_ids.is_empty() {
            Vec::new()
        } else {
            summary_ids.split('\n').map(str::to_string).collect()
        },
        date_start: row.get(13)?,
        date_end: row.get(14)?,
        started_at: row.get(15)?,
        completed_at: row.get(16)?,
        creation_source: row.get(17)?,
        pause_reason: row.get(18)?,
    })
}

impl JobRepository for SqliteRepository {
    fn create_job(&self, job: &Job) -> Result<()> {
        self.conn.execute(
            "INSERT INTO jobs
               (id, workspace_id, job_type, status, progress_current, progress_total,
                cost_micros, failure_reason, created_at, updated_at, scope, schedule_name,
                summary_ids, date_start, date_end, started_at, completed_at, creation_source,
                pause_reason)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
            params![
                job.id.as_str(),
                job.workspace_id.as_str(),
                job.job_type.as_str(),
                job.status.as_str(),
                job.progress_current,
                job.progress_total,
                job.cost_micros,
                job.failure_reason,
                job.created_at,
                job.updated_at,
                job.scope,
                job.schedule_name,
                job.summary_ids.join("\n"),
                job.date_start,
                job.date_end,
                job.started_at,
                job.completed_at,
                job.creation_source,
                job.pause_reason,
            ],
        )?;
        Ok(())
    }

    fn update_job(&self, job: &Job) -> Result<()> {
        self.conn.execute(
            "UPDATE jobs SET status = ?3, progress_current = ?4, progress_total = ?5,
                 cost_micros = ?6, failure_reason = ?7, updated_at = ?8, summary_ids = ?9,
                 started_at = ?10, completed_at = ?11, pause_reason = ?12
             WHERE id = ?1 AND workspace_id = ?2",
            params![
                job.id.as_str(),
                job.workspace_id.as_str(),
                job.status.as_str(),
                job.progress_current,
                job.progress_total,
                job.cost_micros,
                job.failure_reason,
                job.updated_at,
                job.summary_ids.join("\n"),
                job.started_at,
                job.completed_at,
                job.pause_reason,
            ],
        )?;
        Ok(())
    }

    fn get_job(&self, workspace: &WorkspaceId, id: &JobId) -> Result<Option<Job>> {
        self.conn
            .query_row(
                &format!("SELECT {JOB_COLS} FROM jobs WHERE id = ?1 AND workspace_id = ?2"),
                params![id.as_str(), workspace.as_str()],
                |row| Ok(row_to_job(row)),
            )
            .optional()?
            .transpose()
    }

    fn list_jobs(&self, workspace: &WorkspaceId, limit: u32) -> Result<Vec<Job>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {JOB_COLS} FROM jobs WHERE workspace_id = ?1 \
             ORDER BY created_at DESC, id LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![workspace.as_str(), limit], |row| Ok(row_to_job(row)))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    fn pause_running(&self, now: i64) -> Result<u64> {
        let changed = self.conn.execute(
            "UPDATE jobs SET status = 'paused', updated_at = ?1,
                 pause_reason = 'process restarted while running'
             WHERE status = 'running'",
            params![now],
        )?;
        Ok(changed as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::FailureClass;

    fn repo() -> SqliteRepository {
        SqliteRepository::in_memory().unwrap()
    }
    fn ws() -> WorkspaceId {
        WorkspaceId::parse("ws-1").unwrap()
    }
    fn new_job() -> Job {
        Job::record(
            JobId::parse("job_1").unwrap(),
            ws(),
            JobType::Summarization,
            1_000,
        )
    }

    #[test]
    fn create_update_get_round_trips() {
        let repo = repo();
        let mut job = new_job();
        repo.create_job(&job).unwrap();
        job.start(1_001).unwrap();
        job.set_progress(2, 5, 1_002);
        job.complete(42_000, 1_003).unwrap();
        repo.update_job(&job).unwrap();

        let got = repo.get_job(&ws(), &job.id).unwrap().unwrap();
        assert_eq!(got, job);
        assert_eq!(got.status, JobStatus::Completed);
        assert_eq!(got.cost_micros, 42_000);
    }

    #[test]
    fn failure_reason_persists() {
        let repo = repo();
        let mut job = new_job();
        repo.create_job(&job).unwrap();
        job.start(1).unwrap();
        job.fail(FailureClass::ServiceUnavailable, 3_000, 2)
            .unwrap();
        repo.update_job(&job).unwrap();
        let got = repo.get_job(&ws(), &job.id).unwrap().unwrap();
        assert_eq!(got.failure_reason.as_deref(), Some("service_unavailable"));
    }

    #[test]
    fn get_is_workspace_scoped() {
        let repo = repo();
        let job = new_job();
        repo.create_job(&job).unwrap();
        let other = WorkspaceId::parse("ws-other").unwrap();
        assert!(repo.get_job(&other, &job.id).unwrap().is_none());
    }

    #[test]
    fn restart_pauses_running_jobs_only() {
        let repo = repo();
        // One running, one completed.
        let mut running = new_job();
        repo.create_job(&running).unwrap();
        running.start(1).unwrap();
        repo.update_job(&running).unwrap();

        let mut done = Job::record(JobId::parse("job_2").unwrap(), ws(), JobType::Backfill, 1);
        repo.create_job(&done).unwrap();
        done.start(1).unwrap();
        done.complete(0, 2).unwrap();
        repo.update_job(&done).unwrap();

        assert_eq!(repo.pause_running(9_000).unwrap(), 1);
        assert_eq!(
            repo.get_job(&ws(), &running.id).unwrap().unwrap().status,
            JobStatus::Paused
        );
        assert_eq!(
            repo.get_job(&ws(), &done.id).unwrap().unwrap().status,
            JobStatus::Completed
        );
    }
}
