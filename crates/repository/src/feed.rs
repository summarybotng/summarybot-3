//! RSS/Atom feeds of a workspace's summaries (ADR-133 A4).
//!
//! A feed exposes a workspace's (optionally channel-scoped) summaries at a
//! public, **unguessable-token** URL so RSS readers — which can't authenticate —
//! can subscribe. The token is the capability; `is_public` is an advisory flag
//! (private feeds are served by token but marked `noindex` and never listed
//! publicly). New table (baseline `CREATE TABLE IF NOT EXISTS`).

use crate::SqliteRepository;
use anyhow::Result;
use domain::WorkspaceId;
use rusqlite::{params, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Feed {
    pub id: String,
    /// Optional channel scope; `None` = all of the workspace's summaries.
    pub channel_id: Option<String>,
    /// `rss` | `atom`.
    pub feed_type: String,
    pub is_public: bool,
    /// Unguessable token in the public URL (`/feeds/<token>`).
    pub url_token: String,
    pub title: Option<String>,
    pub access_count: i64,
    pub created_at: i64,
    pub last_accessed: Option<i64>,
}

/// Storage boundary for feeds (ADR-133 A4).
pub trait FeedRepository {
    fn create_feed(&self, workspace: &WorkspaceId, feed: &Feed) -> Result<()>;
    fn list_feeds(&self, workspace: &WorkspaceId) -> Result<Vec<Feed>>;
    /// Resolve a public token to its owning workspace + feed (the render path).
    fn get_feed_by_token(&self, token: &str) -> Result<Option<(WorkspaceId, Feed)>>;
    fn delete_feed(&self, workspace: &WorkspaceId, id: &str) -> Result<bool>;
    /// Bump access_count + last_accessed when the public feed is fetched.
    fn bump_feed_access(&self, token: &str, now: i64) -> Result<()>;
}

fn row_to_feed(row: &rusqlite::Row, base: usize) -> rusqlite::Result<Feed> {
    Ok(Feed {
        id: row.get(base)?,
        channel_id: row.get(base + 1)?,
        feed_type: row.get(base + 2)?,
        is_public: row.get::<_, i64>(base + 3)? != 0,
        url_token: row.get(base + 4)?,
        title: row.get(base + 5)?,
        access_count: row.get(base + 6)?,
        created_at: row.get(base + 7)?,
        last_accessed: row.get(base + 8)?,
    })
}

const COLS: &str =
    "id, channel_id, feed_type, is_public, url_token, title, access_count, created_at, last_accessed";

impl FeedRepository for SqliteRepository {
    fn create_feed(&self, workspace: &WorkspaceId, feed: &Feed) -> Result<()> {
        self.conn.execute(
            "INSERT INTO feeds
               (id, workspace_id, channel_id, feed_type, is_public, url_token, title,
                access_count, created_at, last_accessed)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                feed.id,
                workspace.as_str(),
                feed.channel_id,
                feed.feed_type,
                feed.is_public,
                feed.url_token,
                feed.title,
                feed.access_count,
                feed.created_at,
                feed.last_accessed,
            ],
        )?;
        Ok(())
    }

    fn list_feeds(&self, workspace: &WorkspaceId) -> Result<Vec<Feed>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLS} FROM feeds WHERE workspace_id = ?1 ORDER BY created_at DESC, id"
        ))?;
        let rows = stmt
            .query_map(params![workspace.as_str()], |r| row_to_feed(r, 0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn get_feed_by_token(&self, token: &str) -> Result<Option<(WorkspaceId, Feed)>> {
        self.conn
            .query_row(
                &format!("SELECT workspace_id, {COLS} FROM feeds WHERE url_token = ?1"),
                params![token],
                |row| {
                    let ws: String = row.get(0)?;
                    Ok((ws, row_to_feed(row, 1)?))
                },
            )
            .optional()?
            .map(|(ws, feed)| Ok((WorkspaceId::parse(ws).map_err(anyhow::Error::new)?, feed)))
            .transpose()
    }

    fn delete_feed(&self, workspace: &WorkspaceId, id: &str) -> Result<bool> {
        let n = self.conn.execute(
            "DELETE FROM feeds WHERE workspace_id = ?1 AND id = ?2",
            params![workspace.as_str(), id],
        )?;
        Ok(n > 0)
    }

    fn bump_feed_access(&self, token: &str, now: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE feeds SET access_count = access_count + 1, last_accessed = ?2
             WHERE url_token = ?1",
            params![token, now],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> WorkspaceId {
        WorkspaceId::parse("ws-1").unwrap()
    }
    fn feed(id: &str, token: &str) -> Feed {
        Feed {
            id: id.into(),
            channel_id: None,
            feed_type: "rss".into(),
            is_public: true,
            url_token: token.into(),
            title: Some("Eng".into()),
            access_count: 0,
            created_at: 10,
            last_accessed: None,
        }
    }

    #[test]
    fn create_list_resolve_bump_delete() {
        let repo = SqliteRepository::in_memory().unwrap();
        let ws = ws();
        repo.create_feed(&ws, &feed("f1", "tok-abc")).unwrap();
        assert_eq!(repo.list_feeds(&ws).unwrap().len(), 1);

        let (rws, f) = repo.get_feed_by_token("tok-abc").unwrap().unwrap();
        assert_eq!(rws.as_str(), "ws-1");
        assert_eq!(f.id, "f1");

        repo.bump_feed_access("tok-abc", 99).unwrap();
        let (_, f) = repo.get_feed_by_token("tok-abc").unwrap().unwrap();
        assert_eq!(f.access_count, 1);
        assert_eq!(f.last_accessed, Some(99));

        assert!(repo.delete_feed(&ws, "f1").unwrap());
        assert!(repo.get_feed_by_token("tok-abc").unwrap().is_none());
    }
}
