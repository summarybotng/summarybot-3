//! Optional live-source binding for a schedule (ADR-128) — so a scheduled run
//! can **fetch fresh messages** from Discord/Slack before summarizing, instead of
//! only reading what was manually synced.
//!
//! Kept in its own table (not on the `Schedule` domain struct) so adding it
//! didn't churn the schedule build/storage path. One source per schedule; the
//! credential to reach it lives in [`crate::PlatformCredentialRepository`].

use crate::SqliteRepository;
use anyhow::Result;
use rusqlite::{params, OptionalExtension};

/// A schedule's live source: which platform, and its scope id (Discord guild;
/// `None` for Slack, whose token is workspace-scoped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleSource {
    pub platform: String,
    pub source_id: Option<String>,
}

/// Storage boundary for a schedule's optional live source.
pub trait ScheduleSourceRepository {
    /// Set (or replace) the live source for a schedule.
    fn set_schedule_source(&self, schedule_id: &str, source: &ScheduleSource) -> Result<()>;
    /// Fetch a schedule's live source, if any.
    fn get_schedule_source(&self, schedule_id: &str) -> Result<Option<ScheduleSource>>;
    /// Remove a schedule's live source. Returns whether a row was removed.
    fn delete_schedule_source(&self, schedule_id: &str) -> Result<bool>;
}

impl ScheduleSourceRepository for SqliteRepository {
    fn set_schedule_source(&self, schedule_id: &str, source: &ScheduleSource) -> Result<()> {
        self.conn.execute(
            "INSERT INTO schedule_sources (schedule_id, platform, source_id)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(schedule_id) DO UPDATE SET
                 platform = excluded.platform,
                 source_id = excluded.source_id",
            params![schedule_id, source.platform, source.source_id],
        )?;
        Ok(())
    }

    fn get_schedule_source(&self, schedule_id: &str) -> Result<Option<ScheduleSource>> {
        self.conn
            .query_row(
                "SELECT platform, source_id FROM schedule_sources WHERE schedule_id = ?1",
                params![schedule_id],
                |row| {
                    Ok(ScheduleSource {
                        platform: row.get(0)?,
                        source_id: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    fn delete_schedule_source(&self, schedule_id: &str) -> Result<bool> {
        let n = self.conn.execute(
            "DELETE FROM schedule_sources WHERE schedule_id = ?1",
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

    #[test]
    fn set_get_delete_round_trip() {
        let repo = repo();
        assert_eq!(repo.get_schedule_source("sch_1").unwrap(), None);

        repo.set_schedule_source(
            "sch_1",
            &ScheduleSource {
                platform: "discord".into(),
                source_id: Some("guild-1".into()),
            },
        )
        .unwrap();
        let got = repo.get_schedule_source("sch_1").unwrap().unwrap();
        assert_eq!(got.platform, "discord");
        assert_eq!(got.source_id.as_deref(), Some("guild-1"));

        // Replace (e.g. switch to Slack, no scope id).
        repo.set_schedule_source(
            "sch_1",
            &ScheduleSource {
                platform: "slack".into(),
                source_id: None,
            },
        )
        .unwrap();
        let got = repo.get_schedule_source("sch_1").unwrap().unwrap();
        assert_eq!(got.platform, "slack");
        assert_eq!(got.source_id, None);

        assert!(repo.delete_schedule_source("sch_1").unwrap());
        assert!(!repo.delete_schedule_source("sch_1").unwrap());
        assert_eq!(repo.get_schedule_source("sch_1").unwrap(), None);
    }
}
