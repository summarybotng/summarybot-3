//! Operational error log (ADR-133 A3; grounds: ADR-031 comprehensive error
//! logging, ADR-024 failure taxonomy, ADR-041/097 soft-fail channel access).
//!
//! Operational failures (a sync that couldn't read a channel, a delivery that
//! failed, a summarize that errored) are *recorded* — not just logged — so the
//! dashboard can surface them with operation/severity/scope and let an admin
//! mark them resolved. New table (baseline `CREATE TABLE IF NOT EXISTS`,
//! `tenant_plugins` precedent — no migration entry). Per ADR-031, the stored
//! `message` is sanitized (no secrets) by the caller.

use crate::SqliteRepository;
use anyhow::Result;
use domain::WorkspaceId;
use rusqlite::{params, OptionalExtension};

/// A recorded operational failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationalError {
    pub id: String,
    /// What was being attempted, e.g. `sync` / `summarize` / `deliver` (ADR-031).
    pub operation: String,
    /// `FailureClass::as_str` when known, else a coarse class.
    pub error_class: String,
    /// `error` | `warning` (ADR-133).
    pub severity: String,
    /// Optional channel scope (e.g. an unreadable channel, ADR-041/097).
    pub channel_id: Option<String>,
    /// Sanitized human-readable detail (ADR-031 — never secrets).
    pub message: String,
    pub resolved: bool,
    pub created_at: i64,
}

/// Storage boundary for the operational error log (ADR-133).
pub trait OperationalErrorRepository {
    fn record_error(&self, workspace: &WorkspaceId, e: &OperationalError) -> Result<()>;
    /// Newest first; unresolved-only unless `include_resolved`. Capped at `limit`.
    fn list_errors(
        &self,
        workspace: &WorkspaceId,
        include_resolved: bool,
        limit: u32,
    ) -> Result<Vec<OperationalError>>;
    /// Mark one resolved; returns whether a row changed.
    fn resolve_error(&self, workspace: &WorkspaceId, id: &str) -> Result<bool>;
    /// Mark all of a workspace's errors resolved; returns the count changed.
    fn resolve_all_errors(&self, workspace: &WorkspaceId) -> Result<u64>;
    /// Count of unresolved errors (the nav badge / header).
    fn unresolved_error_count(&self, workspace: &WorkspaceId) -> Result<i64>;
}

impl OperationalErrorRepository for SqliteRepository {
    fn record_error(&self, workspace: &WorkspaceId, e: &OperationalError) -> Result<()> {
        self.conn.execute(
            "INSERT INTO operational_errors
               (id, workspace_id, operation, error_class, severity, channel_id, message,
                resolved, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                e.id,
                workspace.as_str(),
                e.operation,
                e.error_class,
                e.severity,
                e.channel_id,
                e.message,
                e.resolved,
                e.created_at,
            ],
        )?;
        Ok(())
    }

    fn list_errors(
        &self,
        workspace: &WorkspaceId,
        include_resolved: bool,
        limit: u32,
    ) -> Result<Vec<OperationalError>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, operation, error_class, severity, channel_id, message, resolved, created_at
             FROM operational_errors
             WHERE workspace_id = ?1 AND (?2 OR resolved = 0)
             ORDER BY created_at DESC, id LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![workspace.as_str(), include_resolved, limit], |row| {
                Ok(OperationalError {
                    id: row.get(0)?,
                    operation: row.get(1)?,
                    error_class: row.get(2)?,
                    severity: row.get(3)?,
                    channel_id: row.get(4)?,
                    message: row.get(5)?,
                    resolved: row.get(6)?,
                    created_at: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn resolve_error(&self, workspace: &WorkspaceId, id: &str) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE operational_errors SET resolved = 1 WHERE workspace_id = ?1 AND id = ?2",
            params![workspace.as_str(), id],
        )?;
        Ok(n > 0)
    }

    fn resolve_all_errors(&self, workspace: &WorkspaceId) -> Result<u64> {
        let n = self.conn.execute(
            "UPDATE operational_errors SET resolved = 1 WHERE workspace_id = ?1 AND resolved = 0",
            params![workspace.as_str()],
        )?;
        Ok(n as u64)
    }

    fn unresolved_error_count(&self, workspace: &WorkspaceId) -> Result<i64> {
        Ok(self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM operational_errors WHERE workspace_id = ?1 AND resolved = 0",
                params![workspace.as_str()],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }
}

/// Convenience constructor for the common case (id minted from a clock value the
/// caller supplies, since the repository layer is clock-free).
impl OperationalError {
    pub fn new(
        id: impl Into<String>,
        operation: impl Into<String>,
        error_class: impl Into<String>,
        severity: impl Into<String>,
        channel_id: Option<String>,
        message: impl Into<String>,
        now: i64,
    ) -> Self {
        Self {
            id: id.into(),
            operation: operation.into(),
            error_class: error_class.into(),
            severity: severity.into(),
            channel_id,
            message: message.into(),
            resolved: false,
            created_at: now,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> WorkspaceId {
        WorkspaceId::parse("ws-1").unwrap()
    }

    #[test]
    fn record_list_resolve_and_count() {
        let repo = SqliteRepository::in_memory().unwrap();
        let ws = ws();
        repo.record_error(
            &ws,
            &OperationalError::new("e1", "sync", "unknown", "error", Some("c1".into()), "no access", 10),
        )
        .unwrap();
        repo.record_error(
            &ws,
            &OperationalError::new("e2", "summarize", "rate_limited", "warning", None, "429", 20),
        )
        .unwrap();

        // Newest first; both unresolved.
        let open = repo.list_errors(&ws, false, 50).unwrap();
        assert_eq!(open.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["e2", "e1"]);
        assert_eq!(repo.unresolved_error_count(&ws).unwrap(), 2);

        // Resolve one.
        assert!(repo.resolve_error(&ws, "e1").unwrap());
        assert_eq!(repo.list_errors(&ws, false, 50).unwrap().len(), 1);
        assert_eq!(repo.list_errors(&ws, true, 50).unwrap().len(), 2);
        assert_eq!(repo.unresolved_error_count(&ws).unwrap(), 1);

        // Bulk resolve the rest.
        assert_eq!(repo.resolve_all_errors(&ws).unwrap(), 1);
        assert_eq!(repo.unresolved_error_count(&ws).unwrap(), 0);
    }
}
