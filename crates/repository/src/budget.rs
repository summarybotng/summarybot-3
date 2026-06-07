//! Per-tenant LLM budget persistence (ADR-125 Phase 3).
//!
//! Stores the operator-granted budget + accrued spend for the current window.
//! The pure rollover/within-budget *policy* lives in `domain::budget`; this is
//! just storage, tenant-scoped (TEN-007).

use crate::SqliteRepository;
use anyhow::Result;
use domain::TenantId;
use rusqlite::{params, OptionalExtension};

/// A tenant's budget grant + current-window spend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetRow {
    pub limit_micros: i64,
    pub period_secs: i64,
    pub period_start: i64,
    pub spent_micros: i64,
}

/// Storage boundary for per-tenant budgets.
pub trait BudgetRepository {
    fn get_budget(&self, tenant: &TenantId) -> Result<Option<BudgetRow>>;
    /// Insert or replace the full budget row (limit, period, window state).
    fn upsert_budget(&self, tenant: &TenantId, row: &BudgetRow) -> Result<()>;
    /// Remove the grant (revert the tenant to unmetered process-default access).
    fn clear_budget(&self, tenant: &TenantId) -> Result<bool>;
}

impl BudgetRepository for SqliteRepository {
    fn get_budget(&self, tenant: &TenantId) -> Result<Option<BudgetRow>> {
        self.conn
            .query_row(
                "SELECT limit_micros, period_secs, period_start, spent_micros
                 FROM tenant_budget WHERE tenant_id = ?1",
                params![tenant.as_str()],
                |row| {
                    Ok(BudgetRow {
                        limit_micros: row.get(0)?,
                        period_secs: row.get(1)?,
                        period_start: row.get(2)?,
                        spent_micros: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    fn upsert_budget(&self, tenant: &TenantId, row: &BudgetRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO tenant_budget
               (tenant_id, limit_micros, period_secs, period_start, spent_micros)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(tenant_id) DO UPDATE SET
               limit_micros = excluded.limit_micros,
               period_secs = excluded.period_secs,
               period_start = excluded.period_start,
               spent_micros = excluded.spent_micros",
            params![
                tenant.as_str(),
                row.limit_micros,
                row.period_secs,
                row.period_start,
                row.spent_micros
            ],
        )?;
        Ok(())
    }

    fn clear_budget(&self, tenant: &TenantId) -> Result<bool> {
        let n = self.conn.execute(
            "DELETE FROM tenant_budget WHERE tenant_id = ?1",
            params![tenant.as_str()],
        )?;
        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> SqliteRepository {
        SqliteRepository::in_memory().unwrap()
    }
    fn t(id: &str) -> TenantId {
        TenantId::parse(id).unwrap()
    }

    #[test]
    fn upsert_get_clear_round_trip() {
        let repo = repo();
        assert!(repo.get_budget(&t("t1")).unwrap().is_none());

        let row = BudgetRow {
            limit_micros: 1_000_000,
            period_secs: 2_592_000,
            period_start: 100,
            spent_micros: 250,
        };
        repo.upsert_budget(&t("t1"), &row).unwrap();
        assert_eq!(repo.get_budget(&t("t1")).unwrap().unwrap(), row);

        // Update accrues spend.
        let charged = BudgetRow {
            spent_micros: 700,
            ..row
        };
        repo.upsert_budget(&t("t1"), &charged).unwrap();
        assert_eq!(
            repo.get_budget(&t("t1")).unwrap().unwrap().spent_micros,
            700
        );

        // Scoped + clearable.
        assert!(repo.get_budget(&t("other")).unwrap().is_none());
        assert!(repo.clear_budget(&t("t1")).unwrap());
        assert!(repo.get_budget(&t("t1")).unwrap().is_none());
        assert!(!repo.clear_budget(&t("t1")).unwrap());
    }
}
