//! Per-schedule steering options (ADR-133 §B) — the v2 schedule attributes that
//! shape *how* a scheduled summary reads, kept in a side-table (like
//! `rolling_config`) so the core `Schedule` value object stays small.
//!
//! - `prompt_template_id` / `perspective` — steer the summary's voice/audience
//!   (resolved to instructions at run time);
//! - `title_template` — a title applied to the produced summary;
//! - `enable_continuity` — carry the previous digest forward as context.

use crate::SqliteRepository;
use anyhow::Result;
use rusqlite::{params, OptionalExtension};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScheduleOptions {
    pub prompt_template_id: Option<String>,
    pub perspective: Option<String>,
    pub title_template: Option<String>,
    pub enable_continuity: bool,
}

impl ScheduleOptions {
    /// Whether any option is actually set (else there's no row worth keeping).
    pub fn is_empty(&self) -> bool {
        self.prompt_template_id.is_none()
            && self.perspective.is_none()
            && self.title_template.is_none()
            && !self.enable_continuity
    }
}

/// Storage boundary for per-schedule steering options (ADR-133 §B).
pub trait ScheduleOptionsRepository {
    fn set_schedule_options(&self, schedule_id: &str, opts: &ScheduleOptions) -> Result<()>;
    fn get_schedule_options(&self, schedule_id: &str) -> Result<Option<ScheduleOptions>>;
    fn delete_schedule_options(&self, schedule_id: &str) -> Result<()>;
}

impl ScheduleOptionsRepository for SqliteRepository {
    fn set_schedule_options(&self, schedule_id: &str, opts: &ScheduleOptions) -> Result<()> {
        self.conn.execute(
            "INSERT INTO schedule_options
               (schedule_id, prompt_template_id, perspective, title_template, enable_continuity)
             VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(schedule_id) DO UPDATE SET
               prompt_template_id = excluded.prompt_template_id,
               perspective        = excluded.perspective,
               title_template     = excluded.title_template,
               enable_continuity  = excluded.enable_continuity",
            params![
                schedule_id,
                opts.prompt_template_id,
                opts.perspective,
                opts.title_template,
                opts.enable_continuity,
            ],
        )?;
        Ok(())
    }

    fn get_schedule_options(&self, schedule_id: &str) -> Result<Option<ScheduleOptions>> {
        Ok(self
            .conn
            .query_row(
                "SELECT prompt_template_id, perspective, title_template, enable_continuity
                 FROM schedule_options WHERE schedule_id = ?1",
                params![schedule_id],
                |row| {
                    Ok(ScheduleOptions {
                        prompt_template_id: row.get(0)?,
                        perspective: row.get(1)?,
                        title_template: row.get(2)?,
                        enable_continuity: row.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    fn delete_schedule_options(&self, schedule_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM schedule_options WHERE schedule_id = ?1",
            params![schedule_id],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_and_read_options() {
        let repo = SqliteRepository::in_memory().unwrap();
        assert!(repo.get_schedule_options("s1").unwrap().is_none());
        let opts = ScheduleOptions {
            prompt_template_id: Some("tmpl_1".into()),
            perspective: Some("developer".into()),
            title_template: Some("Weekly Eng Digest".into()),
            enable_continuity: true,
        };
        repo.set_schedule_options("s1", &opts).unwrap();
        assert_eq!(repo.get_schedule_options("s1").unwrap().unwrap(), opts);
        // Upsert overwrites.
        let opts2 = ScheduleOptions { enable_continuity: false, ..opts.clone() };
        repo.set_schedule_options("s1", &opts2).unwrap();
        assert!(!repo.get_schedule_options("s1").unwrap().unwrap().enable_continuity);
    }
}
