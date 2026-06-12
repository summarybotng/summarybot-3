//! Synthesized wiki pages (PRD §8.2 WIK-001..003; ADR-127).
//!
//! A wiki page is emergent knowledge synthesized from a workspace's knowledge
//! units (markdown with topic headings), regenerable on demand. Workspace-scoped
//! (TEN-007). v1 keeps one "knowledge-base" page per workspace; multi-page
//! emergent structure is a refinement.

use crate::SqliteRepository;
use anyhow::Result;
use domain::WorkspaceId;
use rusqlite::{params, OptionalExtension};

/// A stored wiki page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiPage {
    pub slug: String,
    pub title: String,
    pub content_md: String,
    /// How many knowledge units fed this synthesis (WIK-002 ingestion status).
    pub unit_count: i64,
    pub updated_at: i64,
}

/// Storage boundary for wiki pages.
pub trait WikiRepository {
    /// Insert or replace a page (keyed by `(workspace, slug)`).
    fn upsert_page(&self, workspace: &WorkspaceId, page: &WikiPage) -> Result<()>;
    /// Fetch one page by slug.
    fn get_page(&self, workspace: &WorkspaceId, slug: &str) -> Result<Option<WikiPage>>;
    /// List a workspace's pages (newest first), without bodies for the index.
    fn list_pages(&self, workspace: &WorkspaceId) -> Result<Vec<WikiPage>>;
}

impl WikiRepository for SqliteRepository {
    fn upsert_page(&self, workspace: &WorkspaceId, page: &WikiPage) -> Result<()> {
        self.conn.execute(
            "INSERT INTO wiki_pages (workspace_id, slug, title, content_md, unit_count, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(workspace_id, slug) DO UPDATE SET
                 title = excluded.title,
                 content_md = excluded.content_md,
                 unit_count = excluded.unit_count,
                 updated_at = excluded.updated_at",
            params![
                workspace.as_str(),
                page.slug,
                page.title,
                page.content_md,
                page.unit_count,
                page.updated_at,
            ],
        )?;
        Ok(())
    }

    fn get_page(&self, workspace: &WorkspaceId, slug: &str) -> Result<Option<WikiPage>> {
        self.conn
            .query_row(
                "SELECT slug, title, content_md, unit_count, updated_at
                 FROM wiki_pages WHERE workspace_id = ?1 AND slug = ?2",
                params![workspace.as_str(), slug],
                |row| {
                    Ok(WikiPage {
                        slug: row.get(0)?,
                        title: row.get(1)?,
                        content_md: row.get(2)?,
                        unit_count: row.get(3)?,
                        updated_at: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    fn list_pages(&self, workspace: &WorkspaceId) -> Result<Vec<WikiPage>> {
        let mut stmt = self.conn.prepare(
            "SELECT slug, title, content_md, unit_count, updated_at
             FROM wiki_pages WHERE workspace_id = ?1 ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map(params![workspace.as_str()], |row| {
            Ok(WikiPage {
                slug: row.get(0)?,
                title: row.get(1)?,
                content_md: row.get(2)?,
                unit_count: row.get(3)?,
                updated_at: row.get(4)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
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
    fn upsert_get_list_round_trips() {
        let repo = repo();
        assert!(repo.list_pages(&ws("w1")).unwrap().is_empty());
        let page = WikiPage {
            slug: "knowledge-base".into(),
            title: "Knowledge Base".into(),
            content_md: "## Deploys\n- migration first".into(),
            unit_count: 3,
            updated_at: 100,
        };
        repo.upsert_page(&ws("w1"), &page).unwrap();
        assert_eq!(
            repo.get_page(&ws("w1"), "knowledge-base").unwrap().as_ref(),
            Some(&page)
        );
        assert_eq!(repo.list_pages(&ws("w1")).unwrap().len(), 1);
        assert!(repo.list_pages(&ws("w2")).unwrap().is_empty());

        // Regenerate replaces in place.
        let mut updated = page.clone();
        updated.content_md = "## Deploys\n- migration before Friday".into();
        updated.unit_count = 5;
        repo.upsert_page(&ws("w1"), &updated).unwrap();
        assert_eq!(repo.list_pages(&ws("w1")).unwrap().len(), 1);
        assert_eq!(
            repo.get_page(&ws("w1"), "knowledge-base")
                .unwrap()
                .unwrap()
                .unit_count,
            5
        );
    }
}
