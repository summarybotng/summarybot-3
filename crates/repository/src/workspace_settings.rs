//! Per-workspace summarization settings (SUM-007).
//!
//! Free-text guidance a workspace can attach to its summaries (e.g. "summarize
//! from a product-management perspective; emphasize decisions and risks"). The
//! host appends it to the prompt. Workspace-scoped (TEN-007).

use crate::SqliteRepository;
use anyhow::Result;
use domain::WorkspaceId;
use rusqlite::{params, OptionalExtension};

/// A workspace's summarization settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceSettings {
    /// Extra prompt guidance; `None`/empty uses the default prompt.
    pub summary_instructions: Option<String>,
}

/// Storage boundary for per-workspace settings.
pub trait WorkspaceSettingsRepository {
    /// Fetch a workspace's settings (defaults when unset).
    fn get_settings(&self, workspace: &WorkspaceId) -> Result<WorkspaceSettings>;
    /// Upsert a workspace's settings.
    fn set_settings(&self, workspace: &WorkspaceId, settings: &WorkspaceSettings) -> Result<()>;
}

impl WorkspaceSettingsRepository for SqliteRepository {
    fn get_settings(&self, workspace: &WorkspaceId) -> Result<WorkspaceSettings> {
        let instructions = self
            .conn
            .query_row(
                "SELECT summary_instructions FROM workspace_settings WHERE workspace_id = ?1",
                params![workspace.as_str()],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
            .filter(|s| !s.trim().is_empty());
        Ok(WorkspaceSettings {
            summary_instructions: instructions,
        })
    }

    fn set_settings(&self, workspace: &WorkspaceId, settings: &WorkspaceSettings) -> Result<()> {
        self.conn.execute(
            "INSERT INTO workspace_settings (workspace_id, summary_instructions)
             VALUES (?1, ?2)
             ON CONFLICT(workspace_id) DO UPDATE SET summary_instructions = excluded.summary_instructions",
            params![
                workspace.as_str(),
                settings.summary_instructions.as_deref().map(str::trim).filter(|s| !s.is_empty()),
            ],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> SqliteRepository {
        SqliteRepository::in_memory().unwrap()
    }
    fn ws(id: &str) -> WorkspaceId {
        WorkspaceId::parse(id).unwrap()
    }

    #[test]
    fn defaults_to_none_then_round_trips() {
        let repo = repo();
        assert_eq!(
            repo.get_settings(&ws("w1")).unwrap().summary_instructions,
            None
        );

        repo.set_settings(
            &ws("w1"),
            &WorkspaceSettings {
                summary_instructions: Some("Focus on decisions.".into()),
            },
        )
        .unwrap();
        assert_eq!(
            repo.get_settings(&ws("w1"))
                .unwrap()
                .summary_instructions
                .as_deref(),
            Some("Focus on decisions.")
        );

        // Blank clears it; scoped per workspace.
        repo.set_settings(
            &ws("w1"),
            &WorkspaceSettings {
                summary_instructions: Some("   ".into()),
            },
        )
        .unwrap();
        assert_eq!(
            repo.get_settings(&ws("w1")).unwrap().summary_instructions,
            None
        );
        assert_eq!(
            repo.get_settings(&ws("w2")).unwrap().summary_instructions,
            None
        );
    }
}
