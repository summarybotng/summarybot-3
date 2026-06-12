//! Rolling-period summary storage (ADR-101 / ADR-129) — wires the pure
//! `decide_rolling` policy to durable state.
//!
//! Two tables, two concerns:
//! - `rolling_schedules` — **config**: a schedule's period/strategy/end-day, set
//!   when the schedule is made rolling.
//! - `rolling_summaries` — **active state**: the one in-flight (non-finalized)
//!   accumulator per schedule. The PK on `schedule_id` *is* the one-active-per-
//!   schedule invariant; finalizing deletes the row (next run starts fresh) and
//!   emits a normal `summary_records` row, so finalized history lives there.

use crate::SqliteRepository;
use anyhow::Result;
use rusqlite::{params, OptionalExtension};

/// A schedule's rolling configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollingConfig {
    /// `weekly` | `biweekly` | `monthly`.
    pub period: String,
    /// `append` | `resummarize` | `hybrid`.
    pub strategy: String,
    /// Weekday a weekly period ends on (Mon=0..Sun=6); ignored for bi/monthly.
    pub end_day: u32,
}

/// The active (non-finalized) rolling accumulator for a schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollingSummaryRow {
    pub schedule_id: String,
    pub workspace_id: String,
    pub channel: String,
    pub period_start: i64,
    pub period_end: i64,
    /// Messages up to here have been folded in.
    pub accumulated_through: i64,
    pub accumulation_count: u32,
    /// The running merged document (markdown).
    pub content_md: String,
    /// Accumulated spend across the period's accumulation passes (micro-dollars).
    pub cost_micros: i64,
    /// Model of the most recent accumulation pass.
    pub model: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Storage boundary for rolling config + active accumulators.
pub trait RollingRepository {
    fn set_rolling_config(&self, schedule_id: &str, config: &RollingConfig) -> Result<()>;
    fn get_rolling_config(&self, schedule_id: &str) -> Result<Option<RollingConfig>>;
    fn delete_rolling_config(&self, schedule_id: &str) -> Result<bool>;

    /// The active accumulator for a schedule, if one is in flight.
    fn get_active_rolling(&self, schedule_id: &str) -> Result<Option<RollingSummaryRow>>;
    /// Insert or replace the active accumulator (keyed by `schedule_id`).
    fn upsert_active_rolling(&self, row: &RollingSummaryRow) -> Result<()>;
    /// Clear the active accumulator (on finalize). Returns whether a row was removed.
    fn delete_active_rolling(&self, schedule_id: &str) -> Result<bool>;
}

impl RollingRepository for SqliteRepository {
    fn set_rolling_config(&self, schedule_id: &str, config: &RollingConfig) -> Result<()> {
        self.conn.execute(
            "INSERT INTO rolling_schedules (schedule_id, period, strategy, end_day)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(schedule_id) DO UPDATE SET
                 period = excluded.period,
                 strategy = excluded.strategy,
                 end_day = excluded.end_day",
            params![schedule_id, config.period, config.strategy, config.end_day],
        )?;
        Ok(())
    }

    fn get_rolling_config(&self, schedule_id: &str) -> Result<Option<RollingConfig>> {
        self.conn
            .query_row(
                "SELECT period, strategy, end_day FROM rolling_schedules WHERE schedule_id = ?1",
                params![schedule_id],
                |row| {
                    Ok(RollingConfig {
                        period: row.get(0)?,
                        strategy: row.get(1)?,
                        end_day: row.get::<_, i64>(2)? as u32,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    fn delete_rolling_config(&self, schedule_id: &str) -> Result<bool> {
        let n = self.conn.execute(
            "DELETE FROM rolling_schedules WHERE schedule_id = ?1",
            params![schedule_id],
        )?;
        Ok(n > 0)
    }

    fn get_active_rolling(&self, schedule_id: &str) -> Result<Option<RollingSummaryRow>> {
        self.conn
            .query_row(
                "SELECT schedule_id, workspace_id, channel, period_start, period_end,
                        accumulated_through, accumulation_count, content_md, cost_micros,
                        model, created_at, updated_at
                 FROM rolling_summaries WHERE schedule_id = ?1",
                params![schedule_id],
                |row| {
                    Ok(RollingSummaryRow {
                        schedule_id: row.get(0)?,
                        workspace_id: row.get(1)?,
                        channel: row.get(2)?,
                        period_start: row.get(3)?,
                        period_end: row.get(4)?,
                        accumulated_through: row.get(5)?,
                        accumulation_count: row.get::<_, i64>(6)? as u32,
                        content_md: row.get(7)?,
                        cost_micros: row.get(8)?,
                        model: row.get(9)?,
                        created_at: row.get(10)?,
                        updated_at: row.get(11)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    fn upsert_active_rolling(&self, row: &RollingSummaryRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO rolling_summaries
                 (schedule_id, workspace_id, channel, period_start, period_end,
                  accumulated_through, accumulation_count, content_md, cost_micros,
                  model, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
             ON CONFLICT(schedule_id) DO UPDATE SET
                 period_start = excluded.period_start,
                 period_end = excluded.period_end,
                 accumulated_through = excluded.accumulated_through,
                 accumulation_count = excluded.accumulation_count,
                 content_md = excluded.content_md,
                 cost_micros = excluded.cost_micros,
                 model = excluded.model,
                 updated_at = excluded.updated_at",
            params![
                row.schedule_id,
                row.workspace_id,
                row.channel,
                row.period_start,
                row.period_end,
                row.accumulated_through,
                row.accumulation_count,
                row.content_md,
                row.cost_micros,
                row.model,
                row.created_at,
                row.updated_at,
            ],
        )?;
        Ok(())
    }

    fn delete_active_rolling(&self, schedule_id: &str) -> Result<bool> {
        let n = self.conn.execute(
            "DELETE FROM rolling_summaries WHERE schedule_id = ?1",
            params![schedule_id],
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

    fn row(id: &str) -> RollingSummaryRow {
        RollingSummaryRow {
            schedule_id: id.into(),
            workspace_id: "ws-1".into(),
            channel: "c1".into(),
            period_start: 1_000,
            period_end: 8_000,
            accumulated_through: 3_000,
            accumulation_count: 2,
            content_md: "## Mon\n- shipped".into(),
            cost_micros: 42,
            model: "demo".into(),
            created_at: 1_000,
            updated_at: 3_000,
        }
    }

    #[test]
    fn config_round_trips() {
        let repo = repo();
        assert_eq!(repo.get_rolling_config("s1").unwrap(), None);
        let cfg = RollingConfig {
            period: "weekly".into(),
            strategy: "append".into(),
            end_day: 6,
        };
        repo.set_rolling_config("s1", &cfg).unwrap();
        assert_eq!(repo.get_rolling_config("s1").unwrap(), Some(cfg));
        // Replace.
        let cfg2 = RollingConfig {
            period: "monthly".into(),
            strategy: "hybrid".into(),
            end_day: 0,
        };
        repo.set_rolling_config("s1", &cfg2).unwrap();
        assert_eq!(
            repo.get_rolling_config("s1").unwrap().unwrap().period,
            "monthly"
        );
        assert!(repo.delete_rolling_config("s1").unwrap());
        assert_eq!(repo.get_rolling_config("s1").unwrap(), None);
    }

    #[test]
    fn active_accumulator_round_trips_and_is_one_per_schedule() {
        let repo = repo();
        assert_eq!(repo.get_active_rolling("s1").unwrap(), None);
        repo.upsert_active_rolling(&row("s1")).unwrap();
        assert_eq!(
            repo.get_active_rolling("s1").unwrap().as_ref(),
            Some(&row("s1"))
        );

        // Upsert (same schedule) replaces in place — one active per schedule.
        let mut updated = row("s1");
        updated.accumulated_through = 5_000;
        updated.accumulation_count = 3;
        updated.content_md = "## Mon\n- shipped\n\n## Tue\n- deployed".into();
        repo.upsert_active_rolling(&updated).unwrap();
        let got = repo.get_active_rolling("s1").unwrap().unwrap();
        assert_eq!(got.accumulation_count, 3);
        assert_eq!(got.accumulated_through, 5_000);

        // Finalize clears it; another schedule is independent.
        repo.upsert_active_rolling(&row("s2")).unwrap();
        assert!(repo.delete_active_rolling("s1").unwrap());
        assert_eq!(repo.get_active_rolling("s1").unwrap(), None);
        assert!(repo.get_active_rolling("s2").unwrap().is_some());
    }
}
