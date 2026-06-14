//! Named prompt templates / custom perspectives (ADR-133 A2).
//!
//! A reusable instruction preset a workspace can apply to a summary — beyond the
//! built-in [`domain::summarize::Perspective`]s and the single per-workspace
//! `summary_instructions`. New table (baseline `CREATE TABLE IF NOT EXISTS`,
//! following the `tenant_plugins` precedent — no migration entry needed).

use crate::SqliteRepository;
use anyhow::Result;
use domain::WorkspaceId;
use rusqlite::{params, OptionalExtension};

/// A stored prompt template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptTemplate {
    pub id: String,
    pub name: String,
    pub content: String,
    /// The built-in perspective id this was seeded from, if any (provenance).
    pub based_on: Option<String>,
    pub usage_count: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Storage boundary for prompt templates (ADR-133).
pub trait PromptTemplateRepository {
    fn create_template(&self, workspace: &WorkspaceId, t: &PromptTemplate) -> Result<()>;
    fn list_templates(&self, workspace: &WorkspaceId) -> Result<Vec<PromptTemplate>>;
    fn get_template(&self, workspace: &WorkspaceId, id: &str) -> Result<Option<PromptTemplate>>;
    /// Update name/content; returns whether a row changed. Bumps `updated_at`.
    fn update_template(&self, workspace: &WorkspaceId, t: &PromptTemplate) -> Result<bool>;
    fn delete_template(&self, workspace: &WorkspaceId, id: &str) -> Result<bool>;
    /// Increment `usage_count` (best-effort; called when a summary uses it).
    fn bump_template_usage(&self, workspace: &WorkspaceId, id: &str) -> Result<()>;
}

impl PromptTemplateRepository for SqliteRepository {
    fn create_template(&self, workspace: &WorkspaceId, t: &PromptTemplate) -> Result<()> {
        self.conn.execute(
            "INSERT INTO prompt_templates
               (id, workspace_id, name, content, based_on, usage_count, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                t.id,
                workspace.as_str(),
                t.name,
                t.content,
                t.based_on,
                t.usage_count,
                t.created_at,
                t.updated_at,
            ],
        )?;
        Ok(())
    }

    fn list_templates(&self, workspace: &WorkspaceId) -> Result<Vec<PromptTemplate>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, content, based_on, usage_count, created_at, updated_at
             FROM prompt_templates WHERE workspace_id = ?1 ORDER BY name",
        )?;
        let rows = stmt
            .query_map(params![workspace.as_str()], row_to_template)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn get_template(&self, workspace: &WorkspaceId, id: &str) -> Result<Option<PromptTemplate>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, name, content, based_on, usage_count, created_at, updated_at
                 FROM prompt_templates WHERE workspace_id = ?1 AND id = ?2",
                params![workspace.as_str(), id],
                row_to_template,
            )
            .optional()?)
    }

    fn update_template(&self, workspace: &WorkspaceId, t: &PromptTemplate) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE prompt_templates SET name = ?3, content = ?4, updated_at = ?5
             WHERE workspace_id = ?1 AND id = ?2",
            params![workspace.as_str(), t.id, t.name, t.content, t.updated_at],
        )?;
        Ok(n > 0)
    }

    fn delete_template(&self, workspace: &WorkspaceId, id: &str) -> Result<bool> {
        let n = self.conn.execute(
            "DELETE FROM prompt_templates WHERE workspace_id = ?1 AND id = ?2",
            params![workspace.as_str(), id],
        )?;
        Ok(n > 0)
    }

    fn bump_template_usage(&self, workspace: &WorkspaceId, id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE prompt_templates SET usage_count = usage_count + 1
             WHERE workspace_id = ?1 AND id = ?2",
            params![workspace.as_str(), id],
        )?;
        Ok(())
    }
}

fn row_to_template(row: &rusqlite::Row) -> rusqlite::Result<PromptTemplate> {
    Ok(PromptTemplate {
        id: row.get(0)?,
        name: row.get(1)?,
        content: row.get(2)?,
        based_on: row.get(3)?,
        usage_count: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> WorkspaceId {
        WorkspaceId::parse("ws-1").unwrap()
    }
    fn tmpl(id: &str, name: &str) -> PromptTemplate {
        PromptTemplate {
            id: id.into(),
            name: name.into(),
            content: "Focus on security issues.".into(),
            based_on: None,
            usage_count: 0,
            created_at: 100,
            updated_at: 100,
        }
    }

    #[test]
    fn crud_and_usage_round_trip() {
        let repo = SqliteRepository::in_memory().unwrap();
        let ws = ws();
        repo.create_template(&ws, &tmpl("t1", "Security")).unwrap();
        repo.create_template(&ws, &tmpl("t2", "Alpha")).unwrap();
        // Listed alphabetically by name.
        let all = repo.list_templates(&ws).unwrap();
        assert_eq!(all.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["Alpha", "Security"]);

        // Update content.
        let mut t = repo.get_template(&ws, "t1").unwrap().unwrap();
        t.content = "Focus on launches.".into();
        t.updated_at = 200;
        assert!(repo.update_template(&ws, &t).unwrap());
        assert_eq!(repo.get_template(&ws, "t1").unwrap().unwrap().content, "Focus on launches.");

        // Usage bump.
        repo.bump_template_usage(&ws, "t1").unwrap();
        assert_eq!(repo.get_template(&ws, "t1").unwrap().unwrap().usage_count, 1);

        // Delete.
        assert!(repo.delete_template(&ws, "t1").unwrap());
        assert!(repo.get_template(&ws, "t1").unwrap().is_none());
        assert_eq!(repo.list_templates(&ws).unwrap().len(), 1);
    }
}
