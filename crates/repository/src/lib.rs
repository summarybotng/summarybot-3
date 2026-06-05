//! Persistence layer (PRD §12.1 task 4). Thin trait over storage; tenant
//! isolation is enforced here, at the repository layer (TEN-007), by scoping
//! every read/write to a `WorkspaceId`. Phase 0 ships a real SQLite backend
//! to prove the walking skeleton end-to-end.

use anyhow::Result;
use domain::{Summary, WorkspaceId};
use rusqlite::Connection;

mod identity;
mod session;
mod workspace;
pub use identity::{AuditEntry, IdentityRepository, LinkError};
pub use session::SessionRepository;
pub use workspace::{AttachError, WorkspaceRepository};

/// A summary as persisted, with its assigned row id and owning workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSummary {
    pub id: i64,
    pub workspace_id: String,
    pub summary: Summary,
}

/// Storage boundary. Everything is workspace-scoped: callers cannot read or
/// write across tenants because the `WorkspaceId` is a required argument.
pub trait SummaryRepository {
    fn save_summary(&self, workspace: &WorkspaceId, summary: &Summary) -> Result<i64>;
    /// Fetch a summary by id, but only if it belongs to `workspace`
    /// (cross-tenant access returns `None`, never another tenant's row).
    fn get_summary(&self, workspace: &WorkspaceId, id: i64) -> Result<Option<StoredSummary>>;
}

/// SQLite-backed repository. Use [`SqliteRepository::in_memory`] for tests and
/// the walking skeleton; a file/Postgres backend lands in Phase 1.
pub struct SqliteRepository {
    conn: Connection,
}

impl SqliteRepository {
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::with_connection(conn)
    }

    pub fn with_connection(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tenants (
                id            TEXT PRIMARY KEY,
                name          TEXT NOT NULL,
                subdomain     TEXT,
                custom_domain TEXT
            );
            CREATE TABLE IF NOT EXISTS workspaces (
                id            TEXT PRIMARY KEY,
                tenant_id     TEXT    NOT NULL,
                name          TEXT    NOT NULL,
                owner_user_id TEXT    NOT NULL,
                created_at    INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_workspaces_tenant
                ON workspaces(tenant_id);
            -- A source (e.g. a shared Slack team) may feed MANY workspaces,
            -- including across tenants (ADR-120 / WSP-015). Uniqueness is
            -- per-workspace only; there is no global (platform, platform_id) key.
            CREATE TABLE IF NOT EXISTS workspace_connections (
                workspace_id TEXT NOT NULL,
                platform     TEXT NOT NULL,
                platform_id  TEXT NOT NULL,
                UNIQUE (workspace_id, platform, platform_id)
            );
            CREATE INDEX IF NOT EXISTS idx_connections_workspace
                ON workspace_connections(workspace_id);
            -- One verified identity (provider + subject) binds to exactly one
            -- user (WSP-010). The PRIMARY KEY enforces that at storage, so a
            -- second user can never silently claim a bound platform account.
            CREATE TABLE IF NOT EXISTS identity_links (
                provider  TEXT    NOT NULL,
                subject   TEXT    NOT NULL,
                user_id   TEXT    NOT NULL,
                linked_at INTEGER NOT NULL,
                PRIMARY KEY (provider, subject)
            );
            CREATE INDEX IF NOT EXISTS idx_identity_links_user
                ON identity_links(user_id);
            -- Append-only security ledger (WSP-014): every identity link, claim
            -- and transfer is recorded. `actor` is NULL for system/anonymous.
            CREATE TABLE IF NOT EXISTS audit_log (
                id     INTEGER PRIMARY KEY AUTOINCREMENT,
                ts     INTEGER NOT NULL,
                actor  TEXT,
                action TEXT    NOT NULL,
                detail TEXT    NOT NULL
            );
            -- Refresh-token-backed sessions: the server-side, revocable half of
            -- auth (PRD §12.2 item 3). Only the refresh-token *hash* is stored
            -- (UNIQUE — one session per token); raw tokens never touch the DB.
            CREATE TABLE IF NOT EXISTS sessions (
                id           TEXT    PRIMARY KEY,
                user_id      TEXT    NOT NULL,
                refresh_hash TEXT    NOT NULL UNIQUE,
                issued_at    INTEGER NOT NULL,
                expires_at   INTEGER NOT NULL,
                revoked      INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_sessions_user
                ON sessions(user_id);
            CREATE TABLE IF NOT EXISTS summaries (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                workspace_id  TEXT    NOT NULL,
                text          TEXT    NOT NULL,
                message_count INTEGER NOT NULL,
                word_count    INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_summaries_workspace
                ON summaries(workspace_id);",
        )?;
        Ok(Self { conn })
    }
}

impl SummaryRepository for SqliteRepository {
    fn save_summary(&self, workspace: &WorkspaceId, summary: &Summary) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO summaries (workspace_id, text, message_count, word_count)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                workspace.as_str(),
                summary.text,
                summary.message_count,
                summary.word_count,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    fn get_summary(&self, workspace: &WorkspaceId, id: i64) -> Result<Option<StoredSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, workspace_id, text, message_count, word_count
             FROM summaries WHERE id = ?1 AND workspace_id = ?2",
        )?;
        let mut rows = stmt.query(rusqlite::params![id, workspace.as_str()])?;
        match rows.next()? {
            Some(row) => Ok(Some(StoredSummary {
                id: row.get(0)?,
                workspace_id: row.get(1)?,
                summary: Summary {
                    text: row.get(2)?,
                    message_count: row.get(3)?,
                    word_count: row.get(4)?,
                },
            })),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Summary {
        Summary {
            text: "[2 msgs / 3 words] hello there world".to_string(),
            message_count: 2,
            word_count: 3,
        }
    }

    #[test]
    fn save_then_get_roundtrips() {
        let repo = SqliteRepository::in_memory().unwrap();
        let ws = WorkspaceId::parse("ws-1").unwrap();
        let id = repo.save_summary(&ws, &sample()).unwrap();
        let got = repo.get_summary(&ws, id).unwrap().unwrap();
        assert_eq!(got.id, id);
        assert_eq!(got.workspace_id, "ws-1");
        assert_eq!(got.summary, sample());
    }

    #[test]
    fn tenant_isolation_blocks_cross_workspace_read() {
        let repo = SqliteRepository::in_memory().unwrap();
        let ws_a = WorkspaceId::parse("ws-a").unwrap();
        let ws_b = WorkspaceId::parse("ws-b").unwrap();
        let id = repo.save_summary(&ws_a, &sample()).unwrap();
        // ws-b must not be able to read ws-a's row.
        assert_eq!(repo.get_summary(&ws_b, id).unwrap(), None);
        // owner still can.
        assert!(repo.get_summary(&ws_a, id).unwrap().is_some());
    }
}
