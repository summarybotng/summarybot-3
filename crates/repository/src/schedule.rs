//! Schedule persistence (PRD §3.1 SCH-005/006).
//!
//! Schedules are persisted so they survive restarts (SCH-006): on startup the
//! executor calls [`ScheduleRepository::list_enabled`] to restore them, then
//! re-evaluates each against the clock (the domain's `evaluate_tick` decides
//! fire/skip for any runs missed while down). Runtime state — `next_run` and the
//! consecutive-failure count that drives auto-disable — lives alongside the
//! recurrence definition.

use crate::SqliteRepository;
use anyhow::{Context, Result};
use domain::{Schedule, WorkspaceId};
use rusqlite::{params, OptionalExtension};

/// A persisted schedule: its recurrence definition + execution state.
#[derive(Debug, Clone)]
pub struct StoredSchedule {
    pub id: String,
    pub schedule: Schedule,
    pub next_run: i64,
    pub consecutive_failures: u32,
}

/// Storage boundary for schedules.
pub trait ScheduleRepository {
    fn create_schedule(&self, stored: &StoredSchedule) -> Result<()>;
    fn get_schedule(&self, workspace: &WorkspaceId, id: &str) -> Result<Option<StoredSchedule>>;
    /// All enabled schedules (restart restore, SCH-006).
    fn list_enabled(&self) -> Result<Vec<StoredSchedule>>;
    /// All schedules for a workspace (the management API list).
    fn list_for_workspace(&self, workspace: &WorkspaceId) -> Result<Vec<StoredSchedule>>;
    /// Persist execution state after a tick (new `next_run`, failure count,
    /// and possibly auto-disabled).
    fn update_runtime(
        &self,
        id: &str,
        next_run: i64,
        consecutive_failures: u32,
        enabled: bool,
    ) -> Result<()>;
    /// Pause/resume a schedule (the management API). Returns whether a row changed.
    fn set_enabled(&self, workspace: &WorkspaceId, id: &str, enabled: bool) -> Result<bool>;
    /// Delete a schedule. Returns whether a row was removed.
    fn delete_schedule(&self, workspace: &WorkspaceId, id: &str) -> Result<bool>;
}

/// Weekday numbers (Mon=0) → "0,3"; the inverse parses to a `Vec<u32>`.
fn join_days(days: &[u32]) -> String {
    days.iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn parse_day_numbers(s: &str) -> Result<Vec<u32>> {
    s.split(',')
        .filter(|p| !p.is_empty())
        .map(|p| p.parse::<u32>().context("bad weekday number"))
        .collect()
}

impl ScheduleRepository for SqliteRepository {
    fn create_schedule(&self, stored: &StoredSchedule) -> Result<()> {
        let s = &stored.schedule;
        self.conn.execute(
            "INSERT INTO schedules
               (id, workspace_id, schedule_type, at_hour, at_minute, days, day_of_month,
                timezone, once_at, custom_interval_secs, enabled, next_run, consecutive_failures,
                channel, lookback_secs)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
            params![
                stored.id,
                s.workspace_id.as_str(),
                s.schedule_type.as_str(),
                s.at.hour,
                s.at.minute,
                join_days(&s.day_numbers()),
                s.day_of_month,
                s.timezone_name(),
                s.once_at,
                s.custom_interval_secs,
                s.enabled,
                stored.next_run,
                stored.consecutive_failures,
                s.channel.as_ref().map(|c| c.as_str()),
                s.lookback_secs,
            ],
        )?;
        Ok(())
    }

    fn get_schedule(&self, workspace: &WorkspaceId, id: &str) -> Result<Option<StoredSchedule>> {
        self.conn
            .query_row(
                "SELECT id, workspace_id, schedule_type, at_hour, at_minute, days, day_of_month,
                        timezone, once_at, custom_interval_secs, enabled, next_run,
                        consecutive_failures, channel, lookback_secs
                 FROM schedules WHERE id = ?1 AND workspace_id = ?2",
                params![id, workspace.as_str()],
                row_to_stored,
            )
            .optional()?
            .transpose()
    }

    fn list_enabled(&self) -> Result<Vec<StoredSchedule>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, workspace_id, schedule_type, at_hour, at_minute, days, day_of_month,
                    timezone, once_at, custom_interval_secs, enabled, next_run,
                    consecutive_failures, channel, lookback_secs
             FROM schedules WHERE enabled = 1 ORDER BY next_run",
        )?;
        let rows = stmt.query_map([], row_to_stored)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r??);
        }
        Ok(out)
    }

    fn list_for_workspace(&self, workspace: &WorkspaceId) -> Result<Vec<StoredSchedule>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, workspace_id, schedule_type, at_hour, at_minute, days, day_of_month,
                    timezone, once_at, custom_interval_secs, enabled, next_run,
                    consecutive_failures, channel, lookback_secs
             FROM schedules WHERE workspace_id = ?1 ORDER BY next_run",
        )?;
        let rows = stmt.query_map(params![workspace.as_str()], row_to_stored)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r??);
        }
        Ok(out)
    }

    fn update_runtime(
        &self,
        id: &str,
        next_run: i64,
        consecutive_failures: u32,
        enabled: bool,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE schedules SET next_run = ?2, consecutive_failures = ?3, enabled = ?4
             WHERE id = ?1",
            params![id, next_run, consecutive_failures, enabled],
        )?;
        Ok(())
    }

    fn set_enabled(&self, workspace: &WorkspaceId, id: &str, enabled: bool) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE schedules SET enabled = ?3 WHERE id = ?1 AND workspace_id = ?2",
            params![id, workspace.as_str(), enabled],
        )?;
        Ok(n > 0)
    }

    fn delete_schedule(&self, workspace: &WorkspaceId, id: &str) -> Result<bool> {
        let n = self.conn.execute(
            "DELETE FROM schedules WHERE id = ?1 AND workspace_id = ?2",
            params![id, workspace.as_str()],
        )?;
        Ok(n > 0)
    }
}

/// rusqlite row → `Result<StoredSchedule>` (parsing can fail → inner Result).
fn row_to_stored(row: &rusqlite::Row) -> rusqlite::Result<Result<StoredSchedule>> {
    let id: String = row.get(0)?;
    let workspace_raw: String = row.get(1)?;
    let type_raw: String = row.get(2)?;
    let at_hour: u32 = row.get(3)?;
    let at_minute: u32 = row.get(4)?;
    let days_raw: String = row.get(5)?;
    let day_of_month: u32 = row.get(6)?;
    let tz_raw: String = row.get(7)?;
    let once_at: Option<i64> = row.get(8)?;
    let custom_interval_secs: i64 = row.get(9)?;
    let enabled: bool = row.get(10)?;
    let next_run: i64 = row.get(11)?;
    let consecutive_failures: u32 = row.get(12)?;
    let channel: Option<String> = row.get(13)?;
    let lookback_secs: i64 = row.get(14)?;

    Ok((|| {
        let workspace = WorkspaceId::parse(workspace_raw).map_err(anyhow::Error::new)?;
        let days = parse_day_numbers(&days_raw)?;
        let schedule = Schedule::build(
            workspace,
            &type_raw,
            at_hour,
            at_minute,
            &days,
            day_of_month,
            &tz_raw,
            once_at,
            custom_interval_secs,
            enabled,
            channel.as_deref(),
            lookback_secs,
        )
        .map_err(anyhow::Error::new)?;
        Ok(StoredSchedule {
            id,
            schedule,
            next_run,
            consecutive_failures,
        })
    })())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> SqliteRepository {
        SqliteRepository::in_memory().unwrap()
    }

    fn stored(id: &str, enabled: bool, next_run: i64) -> StoredSchedule {
        // Weekly, Mon+Thu 09:30 Europe/London — built through the public ctor.
        let schedule = Schedule::build(
            WorkspaceId::parse("ws-1").unwrap(),
            "weekly",
            9,
            30,
            &[0, 3],
            1,
            "Europe/London",
            None,
            0,
            enabled,
            Some("chat-1"),
            86_400,
        )
        .unwrap();
        StoredSchedule {
            id: id.into(),
            schedule,
            next_run,
            consecutive_failures: 0,
        }
    }

    #[test]
    fn create_then_get_round_trips() {
        let repo = repo();
        let s = stored("sch_1", true, 1_000);
        repo.create_schedule(&s).unwrap();
        let got = repo
            .get_schedule(&WorkspaceId::parse("ws-1").unwrap(), "sch_1")
            .unwrap()
            .unwrap();
        assert_eq!(got.id, "sch_1");
        assert_eq!(got.schedule.day_numbers(), vec![0, 3]);
        assert_eq!(got.schedule.timezone_name(), "Europe/London");
        assert_eq!(got.next_run, 1_000);
    }

    #[test]
    fn list_for_workspace_delete_and_pause() {
        let repo = repo();
        let ws = WorkspaceId::parse("ws-1").unwrap();
        repo.create_schedule(&stored("a", true, 1_000)).unwrap();
        repo.create_schedule(&stored("b", false, 2_000)).unwrap();
        // list_for_workspace returns both (enabled and disabled).
        assert_eq!(repo.list_for_workspace(&ws).unwrap().len(), 2);
        // Pause then resume.
        assert!(repo.set_enabled(&ws, "a", false).unwrap());
        assert!(repo.list_enabled().unwrap().is_empty());
        assert!(repo.set_enabled(&ws, "a", true).unwrap());
        assert_eq!(repo.list_enabled().unwrap().len(), 1);
        // Delete.
        assert!(repo.delete_schedule(&ws, "b").unwrap());
        assert_eq!(repo.list_for_workspace(&ws).unwrap().len(), 1);
        assert!(!repo.delete_schedule(&ws, "b").unwrap()); // already gone
    }

    #[test]
    fn list_enabled_restores_only_enabled_ordered_by_next_run() {
        let repo = repo();
        repo.create_schedule(&stored("late", true, 3_000)).unwrap();
        repo.create_schedule(&stored("early", true, 1_000)).unwrap();
        repo.create_schedule(&stored("off", false, 500)).unwrap();
        let ids: Vec<String> = repo
            .list_enabled()
            .unwrap()
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids, vec!["early", "late"]); // disabled excluded, ordered by next_run
    }

    #[test]
    fn update_runtime_persists_state_and_disable() {
        let repo = repo();
        repo.create_schedule(&stored("sch_1", true, 1_000)).unwrap();
        repo.update_runtime("sch_1", 2_000, 4, false).unwrap();
        // Now disabled → not restored.
        assert!(repo.list_enabled().unwrap().is_empty());
        let got = repo
            .get_schedule(&WorkspaceId::parse("ws-1").unwrap(), "sch_1")
            .unwrap()
            .unwrap();
        assert_eq!(got.next_run, 2_000);
        assert_eq!(got.consecutive_failures, 4);
        assert!(!got.schedule.enabled);
    }
}
