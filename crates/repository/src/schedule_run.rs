//! Schedule execution history (PRD §5.2 SCM-005).
//!
//! One row is recorded each time a schedule is acted on — fired (with the
//! produced summary id), failed (with the reason), or skipped (a missed run
//! advanced past the grace window). Both the periodic scheduler and a manual
//! trigger (SCM-007) write here, distinguished by `manual`. Tenant isolation is
//! the workspace scope on every read, as elsewhere.

use crate::SqliteRepository;
use anyhow::{anyhow, Result};
use domain::WorkspaceId;
use rusqlite::params;

/// Outcome of one schedule run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// Ran and produced a summary (or a scoped run with nothing substantial).
    Fired,
    /// The run errored (drives the consecutive-failure count).
    Failed,
    /// A missed run advanced past the grace window without firing.
    Skipped,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Fired => "fired",
            RunStatus::Failed => "failed",
            RunStatus::Skipped => "skipped",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "fired" => RunStatus::Fired,
            "failed" => RunStatus::Failed,
            "skipped" => RunStatus::Skipped,
            _ => return None,
        })
    }
}

/// A recorded run (newest-first when listed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleRun {
    pub id: i64,
    pub schedule_id: String,
    pub ran_at: i64,
    pub status: RunStatus,
    /// Produced summary id on success, failure reason on error, else `None`.
    pub detail: Option<String>,
    /// Whether this was a manual trigger (SCM-007) vs the periodic scheduler.
    pub manual: bool,
}

/// Storage boundary for schedule execution history (SCM-005).
pub trait ScheduleRunRepository {
    /// Append a run record.
    fn record_run(
        &self,
        workspace: &WorkspaceId,
        schedule_id: &str,
        ran_at: i64,
        status: RunStatus,
        detail: Option<&str>,
        manual: bool,
    ) -> Result<()>;
    /// List a schedule's run history, newest first, capped at `limit`.
    fn list_runs(
        &self,
        workspace: &WorkspaceId,
        schedule_id: &str,
        limit: u32,
    ) -> Result<Vec<ScheduleRun>>;
}

impl ScheduleRunRepository for SqliteRepository {
    fn record_run(
        &self,
        workspace: &WorkspaceId,
        schedule_id: &str,
        ran_at: i64,
        status: RunStatus,
        detail: Option<&str>,
        manual: bool,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO schedule_runs (schedule_id, workspace_id, ran_at, status, detail, manual)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                schedule_id,
                workspace.as_str(),
                ran_at,
                status.as_str(),
                detail,
                manual,
            ],
        )?;
        Ok(())
    }

    fn list_runs(
        &self,
        workspace: &WorkspaceId,
        schedule_id: &str,
        limit: u32,
    ) -> Result<Vec<ScheduleRun>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, schedule_id, ran_at, status, detail, manual
             FROM schedule_runs
             WHERE workspace_id = ?1 AND schedule_id = ?2
             ORDER BY ran_at DESC, id DESC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![workspace.as_str(), schedule_id, limit], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, bool>(5)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, schedule_id, ran_at, status_raw, detail, manual) = row?;
            let status = RunStatus::parse(&status_raw)
                .ok_or_else(|| anyhow!("invalid run status in storage: {status_raw}"))?;
            out.push(ScheduleRun {
                id,
                schedule_id,
                ran_at,
                status,
                detail,
                manual,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> SqliteRepository {
        SqliteRepository::in_memory().unwrap()
    }
    fn ws() -> WorkspaceId {
        WorkspaceId::parse("ws-1").unwrap()
    }

    #[test]
    fn records_and_lists_newest_first() {
        let repo = repo();
        repo.record_run(&ws(), "sch_1", 100, RunStatus::Fired, Some("sum_a"), false)
            .unwrap();
        repo.record_run(&ws(), "sch_1", 200, RunStatus::Failed, Some("boom"), false)
            .unwrap();
        repo.record_run(&ws(), "sch_1", 300, RunStatus::Fired, Some("sum_b"), true)
            .unwrap();

        let runs = repo.list_runs(&ws(), "sch_1", 10).unwrap();
        assert_eq!(runs.len(), 3);
        // Newest first.
        assert_eq!(runs[0].ran_at, 300);
        assert_eq!(runs[0].status, RunStatus::Fired);
        assert!(runs[0].manual);
        assert_eq!(runs[0].detail.as_deref(), Some("sum_b"));
        assert_eq!(runs[1].status, RunStatus::Failed);
        assert_eq!(runs[1].detail.as_deref(), Some("boom"));
    }

    #[test]
    fn list_is_scoped_by_workspace_and_schedule() {
        let repo = repo();
        repo.record_run(&ws(), "sch_1", 100, RunStatus::Fired, None, false)
            .unwrap();
        repo.record_run(&ws(), "sch_2", 100, RunStatus::Fired, None, false)
            .unwrap();
        let other = WorkspaceId::parse("ws-other").unwrap();
        repo.record_run(&other, "sch_1", 100, RunStatus::Fired, None, false)
            .unwrap();

        // Only sch_1 in ws-1.
        assert_eq!(repo.list_runs(&ws(), "sch_1", 10).unwrap().len(), 1);
        // Another workspace's sch_1 is invisible.
        assert_eq!(repo.list_runs(&other, "sch_1", 10).unwrap().len(), 1);
        // Limit caps results.
        assert_eq!(repo.list_runs(&ws(), "sch_1", 0).unwrap().len(), 0);
    }
}
