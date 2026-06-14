//! Per-schedule delivery destination selection (ADR-014).
//!
//! By default a scheduled summary delivers to all of the workspace's enabled
//! destinations. A schedule may restrict that to a chosen subset — stored here as
//! (schedule, destination) rows. **No rows = deliver to all** (the default);
//! rows restrict delivery to exactly those destination ids.

use crate::SqliteRepository;
use anyhow::Result;
use rusqlite::params;

/// Storage boundary for a schedule's destination selection.
pub trait ScheduleDestinationRepository {
    /// Replace a schedule's destination selection with `destination_ids`
    /// (empty clears it → back to "all destinations").
    fn set_schedule_destinations(&self, schedule_id: &str, destination_ids: &[String])
        -> Result<()>;
    /// The destination ids a schedule is restricted to (empty = all).
    fn list_schedule_destinations(&self, schedule_id: &str) -> Result<Vec<String>>;
}

impl ScheduleDestinationRepository for SqliteRepository {
    fn set_schedule_destinations(
        &self,
        schedule_id: &str,
        destination_ids: &[String],
    ) -> Result<()> {
        self.conn.execute(
            "DELETE FROM schedule_destinations WHERE schedule_id = ?1",
            params![schedule_id],
        )?;
        for id in destination_ids {
            self.conn.execute(
                "INSERT OR IGNORE INTO schedule_destinations (schedule_id, destination_id)
                 VALUES (?1, ?2)",
                params![schedule_id, id],
            )?;
        }
        Ok(())
    }

    fn list_schedule_destinations(&self, schedule_id: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT destination_id FROM schedule_destinations
             WHERE schedule_id = ?1 ORDER BY destination_id",
        )?;
        let rows = stmt.query_map(params![schedule_id], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_replaces_and_list_returns_selection() {
        let repo = SqliteRepository::in_memory().unwrap();
        assert!(repo.list_schedule_destinations("s1").unwrap().is_empty());
        repo.set_schedule_destinations("s1", &["d1".into(), "d2".into()])
            .unwrap();
        assert_eq!(repo.list_schedule_destinations("s1").unwrap(), vec!["d1", "d2"]);
        // Replace (not append).
        repo.set_schedule_destinations("s1", &["d3".into()]).unwrap();
        assert_eq!(repo.list_schedule_destinations("s1").unwrap(), vec!["d3"]);
        // Clear → back to all.
        repo.set_schedule_destinations("s1", &[]).unwrap();
        assert!(repo.list_schedule_destinations("s1").unwrap().is_empty());
    }
}
