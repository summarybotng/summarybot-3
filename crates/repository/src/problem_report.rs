//! User problem reports (ADR-039 "Report a Problem") — distinct from the
//! system-generated operational error log (ADR-133 A3): these are *user-submitted*
//! reports ("this summary is wrong", "expected a digest but got none"). Captured
//! with a category, free-text description, optional resource reference, and the
//! page they came from, then triaged/resolved by an admin. New table.

use crate::SqliteRepository;
use anyhow::Result;
use domain::WorkspaceId;
use rusqlite::{params, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProblemReport {
    pub id: String,
    /// One of the ADR-039 categories (`summary_quality`, `missing_summary`, …).
    pub category: String,
    /// `low` | `medium` | `high` | `critical` (ADR-039).
    pub severity: String,
    pub description: String,
    /// Optional resource the report is about (`summary`/`feed`/`schedule`/…) + id.
    pub resource_type: Option<String>,
    pub resource_id: Option<String>,
    /// The dashboard page the user reported from (auto-captured).
    pub page_url: Option<String>,
    /// The reporter's browser/user-agent (auto-captured, ADR-039).
    pub browser: Option<String>,
    /// Reporter's user id (from the auth token).
    pub reported_by: Option<String>,
    /// `open` | `investigating` | `resolved` | `wont_fix`.
    pub status: String,
    pub created_at: i64,
}

/// Storage boundary for user problem reports (ADR-039).
pub trait ProblemReportRepository {
    fn create_report(&self, workspace: &WorkspaceId, report: &ProblemReport) -> Result<()>;
    /// Newest first; open-only unless `include_resolved`. Optional severity filter.
    /// Paginated via `limit`/`offset`.
    fn list_reports(
        &self,
        workspace: &WorkspaceId,
        include_resolved: bool,
        severity: Option<&str>,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<ProblemReport>>;
    /// Total matching the same filters as `list_reports` (ignoring limit/offset).
    fn count_reports(
        &self,
        workspace: &WorkspaceId,
        include_resolved: bool,
        severity: Option<&str>,
    ) -> Result<i64>;
    /// Set a report's status; returns whether a row changed.
    fn set_report_status(&self, workspace: &WorkspaceId, id: &str, status: &str) -> Result<bool>;
    fn open_report_count(&self, workspace: &WorkspaceId) -> Result<i64>;
}

const COLS: &str = "id, category, severity, description, resource_type, resource_id, page_url, \
     browser, reported_by, status, created_at";

fn row_to_report(row: &rusqlite::Row) -> rusqlite::Result<ProblemReport> {
    Ok(ProblemReport {
        id: row.get(0)?,
        category: row.get(1)?,
        severity: row.get(2)?,
        description: row.get(3)?,
        resource_type: row.get(4)?,
        resource_id: row.get(5)?,
        page_url: row.get(6)?,
        browser: row.get(7)?,
        reported_by: row.get(8)?,
        status: row.get(9)?,
        created_at: row.get(10)?,
    })
}

impl ProblemReportRepository for SqliteRepository {
    fn create_report(&self, workspace: &WorkspaceId, r: &ProblemReport) -> Result<()> {
        self.conn.execute(
            "INSERT INTO problem_reports
               (id, workspace_id, category, severity, description, resource_type, resource_id,
                page_url, browser, reported_by, status, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                r.id,
                workspace.as_str(),
                r.category,
                r.severity,
                r.description,
                r.resource_type,
                r.resource_id,
                r.page_url,
                r.browser,
                r.reported_by,
                r.status,
                r.created_at,
            ],
        )?;
        Ok(())
    }

    fn list_reports(
        &self,
        workspace: &WorkspaceId,
        include_resolved: bool,
        severity: Option<&str>,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<ProblemReport>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLS} FROM problem_reports
             WHERE workspace_id = ?1 AND (?2 OR status IN ('open','investigating'))
               AND (?3 IS NULL OR severity = ?3)
             ORDER BY created_at DESC, id LIMIT ?4 OFFSET ?5"
        ))?;
        let rows = stmt
            .query_map(
                params![workspace.as_str(), include_resolved, severity, limit, offset],
                row_to_report,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn count_reports(
        &self,
        workspace: &WorkspaceId,
        include_resolved: bool,
        severity: Option<&str>,
    ) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM problem_reports
             WHERE workspace_id = ?1 AND (?2 OR status IN ('open','investigating'))
               AND (?3 IS NULL OR severity = ?3)",
            params![workspace.as_str(), include_resolved, severity],
            |row| row.get(0),
        )?)
    }

    fn set_report_status(&self, workspace: &WorkspaceId, id: &str, status: &str) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE problem_reports SET status = ?3 WHERE workspace_id = ?1 AND id = ?2",
            params![workspace.as_str(), id, status],
        )?;
        Ok(n > 0)
    }

    fn open_report_count(&self, workspace: &WorkspaceId) -> Result<i64> {
        Ok(self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM problem_reports
                 WHERE workspace_id = ?1 AND status IN ('open','investigating')",
                params![workspace.as_str()],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> WorkspaceId {
        WorkspaceId::parse("ws-1").unwrap()
    }
    fn rep(id: &str, cat: &str, sev: &str) -> ProblemReport {
        ProblemReport {
            id: id.into(),
            category: cat.into(),
            severity: sev.into(),
            description: "something is off".into(),
            resource_type: Some("summary".into()),
            resource_id: Some("sum_1".into()),
            page_url: Some("/summaries".into()),
            browser: Some("Mozilla/5.0".into()),
            reported_by: Some("u1".into()),
            status: "open".into(),
            created_at: 10,
        }
    }

    #[test]
    fn create_list_resolve_count() {
        let repo = SqliteRepository::in_memory().unwrap();
        let ws = ws();
        repo.create_report(&ws, &rep("r1", "summary_quality", "high")).unwrap();
        repo.create_report(&ws, &rep("r2", "ui_bug", "low")).unwrap();
        assert_eq!(repo.open_report_count(&ws).unwrap(), 2);
        assert_eq!(repo.list_reports(&ws, false, None, 50, 0).unwrap().len(), 2);
        assert_eq!(repo.count_reports(&ws, false, None).unwrap(), 2);

        // severity filter + round-trip
        let highs = repo.list_reports(&ws, false, Some("high"), 50, 0).unwrap();
        assert_eq!(highs.len(), 1);
        assert_eq!(highs[0].severity, "high");
        assert_eq!(highs[0].browser.as_deref(), Some("Mozilla/5.0"));
        assert_eq!(repo.count_reports(&ws, false, Some("low")).unwrap(), 1);

        // pagination
        assert_eq!(repo.list_reports(&ws, false, None, 1, 0).unwrap().len(), 1);
        assert_eq!(repo.list_reports(&ws, false, None, 1, 1).unwrap().len(), 1);
        assert_eq!(repo.list_reports(&ws, false, None, 1, 2).unwrap().len(), 0);

        assert!(repo.set_report_status(&ws, "r1", "resolved").unwrap());
        assert_eq!(repo.open_report_count(&ws).unwrap(), 1);
        assert_eq!(repo.list_reports(&ws, false, None, 50, 0).unwrap().len(), 1);
        assert_eq!(repo.list_reports(&ws, true, None, 50, 0).unwrap().len(), 2);
    }
}
