//! Schedule persistence (PRD §3.1 SCH-005/006).
//!
//! Schedules are persisted so they survive restarts (SCH-006): on startup the
//! executor calls [`ScheduleRepository::list_enabled`] to restore them, then
//! re-evaluates each against the clock (the domain's `evaluate_tick` decides
//! fire/skip for any runs missed while down). Runtime state — `next_run` and the
//! consecutive-failure count that drives auto-disable — lives alongside the
//! recurrence definition.

use crate::SqliteRepository;
use anyhow::{anyhow, Context, Result};
use chrono::Weekday;
use domain::{Schedule, ScheduleType, TimeOfDay, WorkspaceId};
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
    /// Persist execution state after a tick (new `next_run`, failure count,
    /// and possibly auto-disabled).
    fn update_runtime(
        &self,
        id: &str,
        next_run: i64,
        consecutive_failures: u32,
        enabled: bool,
    ) -> Result<()>;
}

/// Weekdays → "0,3" (Mon=0). Inverse parses the same.
fn join_days(days: &[Weekday]) -> String {
    days.iter()
        .map(|d| d.num_days_from_monday().to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn parse_days(s: &str) -> Result<Vec<Weekday>> {
    s.split(',')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let n: u8 = p.parse().context("bad weekday number")?;
            weekday_from_mon(n).ok_or_else(|| anyhow!("weekday out of range: {n}"))
        })
        .collect()
}

fn weekday_from_mon(n: u8) -> Option<Weekday> {
    Some(match n {
        0 => Weekday::Mon,
        1 => Weekday::Tue,
        2 => Weekday::Wed,
        3 => Weekday::Thu,
        4 => Weekday::Fri,
        5 => Weekday::Sat,
        6 => Weekday::Sun,
        _ => return None,
    })
}

impl ScheduleRepository for SqliteRepository {
    fn create_schedule(&self, stored: &StoredSchedule) -> Result<()> {
        let s = &stored.schedule;
        self.conn.execute(
            "INSERT INTO schedules
               (id, workspace_id, schedule_type, at_hour, at_minute, days, day_of_month,
                timezone, once_at, custom_interval_secs, enabled, next_run, consecutive_failures)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                stored.id,
                s.workspace_id.as_str(),
                s.schedule_type.as_str(),
                s.at.hour,
                s.at.minute,
                join_days(&s.days),
                s.day_of_month,
                s.timezone.name(),
                s.once_at,
                s.custom_interval_secs,
                s.enabled,
                stored.next_run,
                stored.consecutive_failures,
            ],
        )?;
        Ok(())
    }

    fn get_schedule(&self, workspace: &WorkspaceId, id: &str) -> Result<Option<StoredSchedule>> {
        self.conn
            .query_row(
                "SELECT id, workspace_id, schedule_type, at_hour, at_minute, days, day_of_month,
                        timezone, once_at, custom_interval_secs, enabled, next_run,
                        consecutive_failures
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
                    consecutive_failures
             FROM schedules WHERE enabled = 1 ORDER BY next_run",
        )?;
        let rows = stmt.query_map([], row_to_stored)?;
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

    Ok((|| {
        let schedule = Schedule {
            workspace_id: WorkspaceId::parse(workspace_raw).map_err(anyhow::Error::new)?,
            schedule_type: ScheduleType::parse(&type_raw)
                .ok_or_else(|| anyhow!("unknown schedule_type: {type_raw}"))?,
            at: TimeOfDay {
                hour: at_hour,
                minute: at_minute,
            },
            days: parse_days(&days_raw)?,
            day_of_month,
            timezone: tz_raw
                .parse()
                .map_err(|_| anyhow!("bad timezone: {tz_raw}"))?,
            once_at,
            custom_interval_secs,
            enabled,
        };
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
        StoredSchedule {
            id: id.into(),
            schedule: Schedule {
                workspace_id: WorkspaceId::parse("ws-1").unwrap(),
                schedule_type: ScheduleType::Weekly,
                at: TimeOfDay {
                    hour: 9,
                    minute: 30,
                },
                days: vec![Weekday::Mon, Weekday::Thu],
                day_of_month: 1,
                timezone: chrono_tz::Europe::London,
                once_at: None,
                custom_interval_secs: 0,
                enabled,
            },
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
        assert_eq!(got.schedule.schedule_type, ScheduleType::Weekly);
        assert_eq!(
            got.schedule.at,
            TimeOfDay {
                hour: 9,
                minute: 30
            }
        );
        assert_eq!(got.schedule.days, vec![Weekday::Mon, Weekday::Thu]);
        assert_eq!(got.schedule.timezone, chrono_tz::Europe::London);
        assert_eq!(got.next_run, 1_000);
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
