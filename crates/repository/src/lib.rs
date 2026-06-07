//! Persistence layer (PRD §12.1 task 4). Thin trait over storage; tenant
//! isolation is enforced here, at the repository layer (TEN-007), by scoping
//! every read/write to a `WorkspaceId`. Phase 0 ships a real SQLite backend
//! to prove the walking skeleton end-to-end.

use anyhow::Result;
use domain::{Summary, WorkspaceId};
use rusqlite::Connection;

mod identity;
mod job;
mod llm_config;
mod membership;
mod schedule;
mod schedule_run;
mod session;
mod summary_store;
mod whatsapp;
mod workspace;
pub use identity::{AuditEntry, IdentityRepository, LinkError};
pub use job::JobRepository;
pub use llm_config::{LlmConfigRepository, TenantLlmConfig};
pub use membership::MembershipRepository;
pub use schedule::{ScheduleRepository, StoredSchedule};
pub use schedule_run::{RunStatus, ScheduleRun, ScheduleRunRepository};
pub use session::SessionRepository;
pub use summary_store::{StructuredSummaryRepository, SummaryQuery, SummaryRecord};
pub use whatsapp::{ImportOutcome, ImportRecord, Participant, WhatsAppRepository};
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

    /// Open (creating if absent) a file-backed database and apply the schema.
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
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
            -- A subdomain/custom domain routes to at most one tenant (TEN-006).
            -- Partial unique so many tenants can leave them unset (NULL).
            CREATE UNIQUE INDEX IF NOT EXISTS idx_tenants_subdomain
                ON tenants(subdomain) WHERE subdomain IS NOT NULL;
            CREATE UNIQUE INDEX IF NOT EXISTS idx_tenants_custom_domain
                ON tenants(custom_domain) WHERE custom_domain IS NOT NULL;
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
            -- WhatsApp ingestion (PRD §2.3, ADR-121). Imports are attributed and
            -- file-hash-deduped per (workspace, chat) (WHA-009/010).
            CREATE TABLE IF NOT EXISTS whatsapp_imports (
                id            TEXT PRIMARY KEY,
                workspace_id  TEXT    NOT NULL,
                chat_id       TEXT    NOT NULL,
                file_hash     TEXT    NOT NULL,
                uploader      TEXT    NOT NULL,
                imported_at   INTEGER NOT NULL,
                format        TEXT    NOT NULL,
                message_count INTEGER NOT NULL,
                date_start    INTEGER NOT NULL,
                date_end      INTEGER NOT NULL,
                UNIQUE (workspace_id, chat_id, file_hash)
            );
            -- Resolved per-chat participant identities (WHA-013). phone_hash is
            -- unique per chat; aliases are newline-delimited display names.
            CREATE TABLE IF NOT EXISTS whatsapp_participants (
                id           TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL,
                chat_id      TEXT NOT NULL,
                phone_hash   TEXT,
                pseudonym    TEXT NOT NULL,
                aliases      TEXT NOT NULL DEFAULT '',
                UNIQUE (workspace_id, chat_id, phone_hash)
            );
            -- Normalized messages keyed by fingerprint id, so re-ingest is
            -- idempotent (WHA-012). One generic attachment slot for now.
            CREATE TABLE IF NOT EXISTS messages (
                id              TEXT PRIMARY KEY,
                workspace_id    TEXT    NOT NULL,
                platform        TEXT    NOT NULL,
                channel_id      TEXT    NOT NULL,
                author_id       TEXT    NOT NULL,
                author_name     TEXT    NOT NULL,
                content         TEXT    NOT NULL,
                timestamp       INTEGER NOT NULL,
                is_system       INTEGER NOT NULL,
                reply_to        TEXT,
                attachment_kind TEXT,
                attachment_name TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_messages_channel_time
                ON messages(workspace_id, channel_id, timestamp);
            -- Job lifecycle (ADR-013): recorded before async work, updated
            -- through Pending→Running→Completed/Failed; Running→Paused on restart.
            CREATE TABLE IF NOT EXISTS jobs (
                id               TEXT PRIMARY KEY,
                workspace_id     TEXT    NOT NULL,
                job_type         TEXT    NOT NULL,
                status           TEXT    NOT NULL,
                progress_current INTEGER NOT NULL,
                progress_total   INTEGER NOT NULL,
                cost_micros      INTEGER NOT NULL,
                failure_reason   TEXT,
                created_at       INTEGER NOT NULL,
                updated_at       INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_jobs_workspace_status
                ON jobs(workspace_id, status);
            -- Structured summaries (PRD §4/§5.1): the always-on dashboard sink.
            -- Lists are newline-joined columns; action items + citations are
            -- child tables (citations keep the grounding message-id link).
            CREATE TABLE IF NOT EXISTS summary_records (
                id              TEXT PRIMARY KEY,
                workspace_id    TEXT    NOT NULL,
                channel_id      TEXT,
                model           TEXT    NOT NULL,
                cost_micros     INTEGER NOT NULL,
                degraded        INTEGER NOT NULL,
                created_at      INTEGER NOT NULL,
                text            TEXT    NOT NULL,
                key_points      TEXT    NOT NULL,
                technical_terms TEXT    NOT NULL,
                participants    TEXT    NOT NULL,
                pinned          INTEGER NOT NULL DEFAULT 0,
                archived        INTEGER NOT NULL DEFAULT 0,
                tags            TEXT    NOT NULL DEFAULT ''
            );
            CREATE INDEX IF NOT EXISTS idx_summary_records_workspace
                ON summary_records(workspace_id, created_at);
            CREATE TABLE IF NOT EXISTS summary_action_items (
                summary_id TEXT    NOT NULL,
                idx        INTEGER NOT NULL,
                text       TEXT    NOT NULL,
                assignee   TEXT,
                PRIMARY KEY (summary_id, idx)
            );
            CREATE TABLE IF NOT EXISTS summary_citations (
                summary_id TEXT    NOT NULL,
                idx        INTEGER NOT NULL,
                message_id TEXT    NOT NULL,
                quote      TEXT,
                PRIMARY KEY (summary_id, idx)
            );
            -- Persistent schedules (SCH-005/006): recurrence definition + run
            -- state, restored on restart.
            CREATE TABLE IF NOT EXISTS schedules (
                id                   TEXT PRIMARY KEY,
                workspace_id         TEXT    NOT NULL,
                schedule_type        TEXT    NOT NULL,
                at_hour              INTEGER NOT NULL,
                at_minute            INTEGER NOT NULL,
                days                 TEXT    NOT NULL,
                day_of_month         INTEGER NOT NULL,
                timezone             TEXT    NOT NULL,
                once_at              INTEGER,
                custom_interval_secs INTEGER NOT NULL,
                enabled              INTEGER NOT NULL,
                next_run             INTEGER NOT NULL,
                consecutive_failures INTEGER NOT NULL,
                channel              TEXT,
                lookback_secs        INTEGER NOT NULL DEFAULT 86400
            );
            CREATE INDEX IF NOT EXISTS idx_schedules_enabled
                ON schedules(enabled, next_run);
            -- Schedule execution history (SCM-005): one row per fire/fail/skip,
            -- whether by the scheduler or a manual trigger. `detail` carries the
            -- produced summary id (success) or the failure reason.
            CREATE TABLE IF NOT EXISTS schedule_runs (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                schedule_id  TEXT    NOT NULL,
                workspace_id TEXT    NOT NULL,
                ran_at       INTEGER NOT NULL,
                status       TEXT    NOT NULL,
                detail       TEXT,
                manual       INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_schedule_runs_lookup
                ON schedule_runs(workspace_id, schedule_id, ran_at);
            CREATE TABLE IF NOT EXISTS summaries (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                workspace_id  TEXT    NOT NULL,
                text          TEXT    NOT NULL,
                message_count INTEGER NOT NULL,
                word_count    INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_summaries_workspace
                ON summaries(workspace_id);
            -- Tenant memberships (PRD §6.2 TEN-005): one role per (tenant, user).
            -- Tenant-scoped, like every other row (TEN-007).
            CREATE TABLE IF NOT EXISTS memberships (
                tenant_id TEXT NOT NULL,
                user_id   TEXT NOT NULL,
                role      TEXT NOT NULL,
                PRIMARY KEY (tenant_id, user_id)
            );
            -- Pending/accepted/revoked invites (PRD §6.3 TEN-005). Only the token
            -- *hash* is stored (host computes it), like a refresh token.
            CREATE TABLE IF NOT EXISTS invites (
                token_hash TEXT    PRIMARY KEY,
                tenant_id  TEXT    NOT NULL,
                email      TEXT    NOT NULL,
                role       TEXT    NOT NULL,
                created_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL,
                status     TEXT    NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_invites_tenant
                ON invites(tenant_id, created_at);
            -- Per-tenant LLM provider config (ADR-125 Phase 2a): a tenant's own
            -- OpenAI-compatible endpoint and/or model. Keyless for now (the
            -- encrypted BYO-key column is Phase 2b).
            CREATE TABLE IF NOT EXISTS tenant_llm_config (
                tenant_id TEXT PRIMARY KEY,
                base_url  TEXT,
                model     TEXT
            );",
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
