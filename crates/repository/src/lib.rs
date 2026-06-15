//! Persistence layer (PRD §12.1 task 4). Thin trait over storage; tenant
//! isolation is enforced here, at the repository layer (TEN-007), by scoping
//! every read/write to a `WorkspaceId`. Phase 0 ships a real SQLite backend
//! to prove the walking skeleton end-to-end.

use anyhow::Result;
use domain::{Summary, WorkspaceId};
use rusqlite::Connection;

mod budget;
mod coverage_store;
mod destination;
mod identity;
mod identity_claim;
mod job;
mod knowledge;
mod llm_config;
mod membership;
mod operational_error;
mod platform_credential;
mod prompt_template;
mod rolling_store;
mod schedule;
mod schedule_destination;
mod schedule_run;
mod schedule_source;
mod session;
mod summary_store;
mod tenant_plugin;
mod whatsapp;
mod wiki;
mod workspace;
mod workspace_settings;
pub use budget::{BudgetRepository, BudgetRow};
pub use coverage_store::{ChannelContent, CoverageRepository, SummarySpan};
pub use destination::{DestinationRepository, StoredDestination};
pub use identity::{AuditEntry, IdentityRepository, LinkError};
pub use identity_claim::{IdentityClaim, IdentityClaimRepository};
pub use job::JobRepository;
pub use knowledge::{KnowledgeRepository, StoredKnowledgeUnit};
pub use llm_config::{LlmConfigRepository, TenantLlmConfig};
pub use membership::MembershipRepository;
pub use operational_error::{OperationalError, OperationalErrorRepository};
pub use platform_credential::PlatformCredentialRepository;
pub use prompt_template::{PromptTemplate, PromptTemplateRepository};
pub use rolling_store::{RollingConfig, RollingRepository, RollingSummaryRow};
pub use schedule::{ScheduleRepository, StoredSchedule};
pub use schedule_destination::ScheduleDestinationRepository;
pub use schedule_run::{RunStatus, ScheduleRun, ScheduleRunRepository};
pub use schedule_source::{ScheduleSource, ScheduleSourceRepository};
pub use session::SessionRepository;
pub use summary_store::{
    ModelSpend, SpendBreakdown, StructuredSummaryRepository, SummaryQuery, SummaryRecord,
};
pub use tenant_plugin::{TenantPlugin, TenantPluginRepository};
pub use whatsapp::{
    ChatSummary, ImportInvitation, ImportOutcome, ImportRecord, NewImportInvitation, Participant,
    StoredImport, WhatsAppRepository,
};
pub use wiki::{WikiPage, WikiRepository};
pub use workspace::{AttachError, WorkspaceRepository};
pub use workspace_settings::{WorkspaceSettings, WorkspaceSettingsRepository};

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

/// Coarse operational counts for the `/metrics` endpoint — process-wide gauges
/// derived from the DB, not per-tenant data.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpsCounts {
    pub tenants: i64,
    pub workspaces: i64,
    pub summaries: i64,
    pub schedules: i64,
    pub total_spend_micros: i64,
}

impl SqliteRepository {
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::with_connection(conn)
    }

    /// Aggregate process-wide counts for monitoring (no tenant data leaves here).
    pub fn ops_counts(&self) -> Result<OpsCounts> {
        let count = |table: &str| -> Result<i64> {
            Ok(self
                .conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?)
        };
        let total_spend_micros: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(cost_micros), 0) FROM summary_records",
            [],
            |r| r.get(0),
        )?;
        Ok(OpsCounts {
            tenants: count("tenants")?,
            workspaces: count("workspaces")?,
            summaries: count("summary_records")?,
            schedules: count("schedules")?,
            total_spend_micros,
        })
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
            -- Contested identity claims (WSP-011..014; ADR-119): a claimant asks
            -- to take over an identity bound to someone else; an approver (tenant
            -- admin same-tenant, platform operator cross-tenant) resolves it.
            CREATE TABLE IF NOT EXISTS identity_claims (
                id            TEXT PRIMARY KEY,
                provider      TEXT NOT NULL,
                subject       TEXT NOT NULL,
                claimant      TEXT NOT NULL,
                current_owner TEXT NOT NULL,
                route         TEXT NOT NULL,
                status        TEXT NOT NULL DEFAULT 'pending',
                created_at    INTEGER NOT NULL
            );
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
            -- Scoped import invitations (WHA-019): a persisted, tracked ask for
            -- a specific date range of a chat to be exported and uploaded. An
            -- invitation opens against a fillable coverage gap and is marked
            -- fulfilled automatically once an import covers the range (the active
            -- request-the-missing-history loop). kind mirrors the gap
            -- classification; note is the human-facing instruction.
            CREATE TABLE IF NOT EXISTS whatsapp_import_invitations (
                id           TEXT PRIMARY KEY,
                workspace_id TEXT    NOT NULL,
                chat_id      TEXT    NOT NULL,
                range_start  INTEGER NOT NULL,
                range_end    INTEGER NOT NULL,
                kind         TEXT    NOT NULL,
                note         TEXT    NOT NULL DEFAULT '',
                status       TEXT    NOT NULL DEFAULT 'open',
                created_by   TEXT    NOT NULL,
                created_at   INTEGER NOT NULL,
                fulfilled_by TEXT,
                fulfilled_at INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_wa_invitations_chat
                ON whatsapp_import_invitations(workspace_id, chat_id, status);
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
            -- Per-claim grounded key points (ADR-004): each key point's text +
            -- confidence + its message references (refs_json = JSON array of
            -- {message_id, author_name, timestamp, position, snippet}). The
            -- denormalized `summary_records.key_points` column is kept for legacy
            -- rows / quick text; this child table is authoritative when present.
            CREATE TABLE IF NOT EXISTS summary_key_points (
                summary_id TEXT    NOT NULL,
                idx        INTEGER NOT NULL,
                text       TEXT    NOT NULL,
                confidence REAL    NOT NULL DEFAULT 1.0,
                refs_json  TEXT    NOT NULL DEFAULT '[]',
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
                tenant_id   TEXT PRIMARY KEY,
                base_url    TEXT,
                model       TEXT,
                api_key_enc TEXT
            );
            -- Per-tenant delivery plugin enablement + account credentials (the
            -- tenant half of the two-layer plugin model). A tenant admin enables a
            -- plugin and configures/connects its shared credentials once
            -- (Confluence creds, SMTP creds, a Google Drive refresh token captured
            -- via OAuth); workspaces then pick only the non-secret target. config_enc
            -- is encrypted JSON of the tenant-scoped fields (master key, like
            -- tenant_llm_config); connected marks an OAuth refresh token captured.
            CREATE TABLE IF NOT EXISTS tenant_plugins (
                tenant_id  TEXT    NOT NULL,
                kind       TEXT    NOT NULL,
                enabled    INTEGER NOT NULL DEFAULT 0,
                config_enc TEXT,
                connected  INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL,
                -- ADR-131: a platform operator's hard veto for this (tenant, kind),
                -- distinct from the tenant's `enabled`. 1 = off regardless of the
                -- tenant toggle; 0 (default) = ordinary ADR-126 enablement.
                operator_disabled INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (tenant_id, kind)
            );
            -- Per-tenant LLM budget for operator-lent platform-key usage
            -- (ADR-125 Phase 3): a limit over a rolling window, with accrued
            -- spend (from summary cost_micros).
            CREATE TABLE IF NOT EXISTS tenant_budget (
                tenant_id    TEXT PRIMARY KEY,
                limit_micros INTEGER NOT NULL,
                period_secs  INTEGER NOT NULL,
                period_start INTEGER NOT NULL,
                spent_micros INTEGER NOT NULL
            );
            -- Per-workspace summary delivery destinations (DSH-010/011): the
            -- address (webhook URL / email) is encrypted at rest with the
            -- operator master key, like a BYO LLM key. The dashboard store is
            -- always-on and never stored here.
            CREATE TABLE IF NOT EXISTS workspace_destinations (
                workspace_id TEXT NOT NULL,
                id           TEXT NOT NULL,
                kind         TEXT NOT NULL,
                address_enc  TEXT,
                enabled      INTEGER NOT NULL DEFAULT 1,
                created_at   INTEGER NOT NULL DEFAULT 0,
                -- ADR-108: for rolling-period schedules, deliver to this destination
                -- on every run (1) rather than only when the period finalizes (0).
                rolling_deliver_intermediate INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (workspace_id, id)
            );
            -- Per-workspace summarization settings (SUM-007): free-text guidance
            -- appended to the prompt (e.g. a perspective/focus).
            CREATE TABLE IF NOT EXISTS workspace_settings (
                workspace_id         TEXT PRIMARY KEY,
                summary_instructions TEXT
            );
            -- Named prompt templates / custom perspectives (ADR-133 A2). A
            -- reusable instruction preset beyond the built-in perspectives and
            -- the single per-workspace summary_instructions.
            CREATE TABLE IF NOT EXISTS prompt_templates (
                id           TEXT PRIMARY KEY,
                workspace_id TEXT    NOT NULL,
                name         TEXT    NOT NULL,
                content      TEXT    NOT NULL,
                based_on     TEXT,
                usage_count  INTEGER NOT NULL DEFAULT 0,
                created_at   INTEGER NOT NULL,
                updated_at   INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_prompt_templates_workspace
                ON prompt_templates(workspace_id, name);
            -- Operational error log (ADR-133 A3; ADR-031). Recorded operational
            -- failures (sync/summarize/deliver) with operation/severity/scope,
            -- resolvable from the dashboard. `message` is sanitized by the caller.
            CREATE TABLE IF NOT EXISTS operational_errors (
                id           TEXT PRIMARY KEY,
                workspace_id TEXT    NOT NULL,
                operation    TEXT    NOT NULL,
                error_class  TEXT    NOT NULL,
                severity     TEXT    NOT NULL,
                channel_id   TEXT,
                message      TEXT    NOT NULL,
                resolved     INTEGER NOT NULL DEFAULT 0,
                created_at   INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_operational_errors_workspace
                ON operational_errors(workspace_id, resolved, created_at);
            -- Knowledge units extracted from summaries (KNO-001; ADR-127). The
            -- embedding is f32 little-endian bytes; `model` pins which embedder
            -- produced it (Q#8 — a model change invalidates vectors). `source_ids`
            -- is newline-joined message ids (provenance, COH-005).
            CREATE TABLE IF NOT EXISTS knowledge_units (
                id           TEXT PRIMARY KEY,
                workspace_id TEXT    NOT NULL,
                summary_id   TEXT    NOT NULL,
                kind         TEXT    NOT NULL,
                text         TEXT    NOT NULL,
                source_ids   TEXT    NOT NULL DEFAULT '',
                embedding    BLOB,
                model        TEXT,
                created_at   INTEGER NOT NULL,
                -- ADR-127/129/117 grounding metadata: the scope channel, the
                -- cited activity's date, and the claim's confidence.
                source_channel TEXT,
                source_date    INTEGER NOT NULL DEFAULT 0,
                confidence     REAL    NOT NULL DEFAULT 1.0
            );
            CREATE INDEX IF NOT EXISTS idx_knowledge_workspace
                ON knowledge_units(workspace_id);
            -- Re-ingest replace-set (ADR-129): clear a summary's prior units fast.
            CREATE INDEX IF NOT EXISTS idx_knowledge_summary
                ON knowledge_units(workspace_id, summary_id);
            -- Synthesized wiki pages (WIK-001..003; ADR-127): emergent topic
            -- pages built from a workspace's knowledge units. Keyed by slug.
            CREATE TABLE IF NOT EXISTS wiki_pages (
                workspace_id TEXT    NOT NULL,
                slug         TEXT    NOT NULL,
                title        TEXT    NOT NULL,
                content_md   TEXT    NOT NULL,
                unit_count   INTEGER NOT NULL DEFAULT 0,
                updated_at   INTEGER NOT NULL,
                PRIMARY KEY (workspace_id, slug)
            );

            CREATE TABLE IF NOT EXISTS platform_credentials (
                workspace_id TEXT    NOT NULL,
                platform     TEXT    NOT NULL,
                token_enc    TEXT    NOT NULL,
                created_at   INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (workspace_id, platform)
            );

            CREATE TABLE IF NOT EXISTS schedule_sources (
                schedule_id TEXT NOT NULL PRIMARY KEY,
                platform    TEXT NOT NULL,
                source_id   TEXT
            );

            -- Per-schedule delivery destination selection (ADR-014): which of the
            -- workspace's destinations this schedule delivers to. No rows for a
            -- schedule = deliver to all (the default); rows restrict to that set.
            CREATE TABLE IF NOT EXISTS schedule_destinations (
                schedule_id    TEXT NOT NULL,
                destination_id TEXT NOT NULL,
                PRIMARY KEY (schedule_id, destination_id)
            );

            CREATE TABLE IF NOT EXISTS rolling_schedules (
                schedule_id TEXT NOT NULL PRIMARY KEY,
                period      TEXT NOT NULL,
                strategy    TEXT NOT NULL,
                end_day     INTEGER NOT NULL DEFAULT 6
            );

            CREATE TABLE IF NOT EXISTS rolling_summaries (
                schedule_id         TEXT    NOT NULL PRIMARY KEY,
                workspace_id        TEXT    NOT NULL,
                channel             TEXT    NOT NULL,
                period_start        INTEGER NOT NULL,
                period_end          INTEGER NOT NULL,
                accumulated_through INTEGER NOT NULL,
                accumulation_count  INTEGER NOT NULL DEFAULT 0,
                content_md          TEXT    NOT NULL,
                cost_micros         INTEGER NOT NULL DEFAULT 0,
                model               TEXT    NOT NULL DEFAULT '',
                created_at          INTEGER NOT NULL,
                updated_at          INTEGER NOT NULL
            );",
        )?;
        run_migrations(&conn)?;
        Ok(Self { conn })
    }
}

/// Ordered, tracked schema migrations applied after the idempotent baseline
/// (the big `CREATE TABLE IF NOT EXISTS` batch above, recorded as the baseline).
/// Each runs once; `schema_migrations` records what's applied so adding a future
/// `(id, sql)` here evolves any existing DB in order — no more ad-hoc ALTERs.
const MIGRATIONS: &[(&str, &str)] = &[
    // The baseline CREATE already includes `api_key_enc`; this migration brings
    // pre-baseline DBs up to date (and is a no-op recorded cleanly on fresh ones).
    (
        "0002_tenant_llm_api_key_enc",
        "ALTER TABLE tenant_llm_config ADD COLUMN api_key_enc TEXT",
    ),
    // Coherence-gate score on summaries (COH-001; ADR-127) — added via migration
    // (not in the baseline CREATE), so fresh and existing DBs get it identically.
    (
        "0003_summary_coherence_score",
        "ALTER TABLE summary_records ADD COLUMN coherence_score REAL",
    ),
    // Detected group-creation instant for a WhatsApp chat (WHA-015), the anchor
    // for `before_join` coverage gaps. NULL when no creation event was found.
    (
        "0004_whatsapp_group_created_at",
        "ALTER TABLE whatsapp_imports ADD COLUMN group_created_at INTEGER",
    ),
    // Per-destination intermediate rolling delivery (ADR-108) — the baseline
    // CREATE already includes it; this brings pre-baseline DBs up to date.
    (
        "0005_dest_rolling_intermediate",
        "ALTER TABLE workspace_destinations ADD COLUMN rolling_deliver_intermediate INTEGER NOT NULL DEFAULT 0",
    ),
    // Platform-operator per-tenant plugin veto (ADR-131) — baseline includes it;
    // this brings pre-baseline DBs up to date.
    (
        "0006_tenant_plugin_operator_disabled",
        "ALTER TABLE tenant_plugins ADD COLUMN operator_disabled INTEGER NOT NULL DEFAULT 0",
    ),
    // Per-unit grounding metadata (ADR-127/129/117) — added via migration so
    // fresh and existing DBs match.
    (
        "0007_knowledge_unit_source_channel",
        "ALTER TABLE knowledge_units ADD COLUMN source_channel TEXT",
    ),
    (
        "0008_knowledge_unit_source_date",
        "ALTER TABLE knowledge_units ADD COLUMN source_date INTEGER NOT NULL DEFAULT 0",
    ),
    (
        "0009_knowledge_unit_confidence",
        "ALTER TABLE knowledge_units ADD COLUMN confidence REAL NOT NULL DEFAULT 1.0",
    ),
    // Token + latency usage on summaries (ADR-106 metadata) — added via migration
    // so fresh and existing DBs match.
    (
        "0010_summary_input_tokens",
        "ALTER TABLE summary_records ADD COLUMN input_tokens INTEGER NOT NULL DEFAULT 0",
    ),
    (
        "0011_summary_output_tokens",
        "ALTER TABLE summary_records ADD COLUMN output_tokens INTEGER NOT NULL DEFAULT 0",
    ),
    (
        "0012_summary_latency_ms",
        "ALTER TABLE summary_records ADD COLUMN latency_ms INTEGER NOT NULL DEFAULT 0",
    ),
    // Covered message-time window on summaries (ADR-133 coverage) — added via
    // migration so fresh and existing DBs match.
    (
        "0013_summary_period_start",
        "ALTER TABLE summary_records ADD COLUMN period_start INTEGER NOT NULL DEFAULT 0",
    ),
    (
        "0014_summary_period_end",
        "ALTER TABLE summary_records ADD COLUMN period_end INTEGER NOT NULL DEFAULT 0",
    ),
];

/// Apply any unapplied migrations in order. Tolerates an additive ALTER whose
/// column already exists (a fresh DB whose baseline CREATE already has it) so the
/// migration is still recorded as applied.
fn run_migrations(conn: &Connection) -> Result<()> {
    use rusqlite::OptionalExtension;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            id          TEXT PRIMARY KEY,
            applied_at  INTEGER NOT NULL
        );",
    )?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // Record the baseline so the ledger reflects the full current schema.
    conn.execute(
        "INSERT OR IGNORE INTO schema_migrations (id, applied_at) VALUES ('0001_baseline', ?1)",
        rusqlite::params![now],
    )?;
    for (id, sql) in MIGRATIONS {
        let applied = conn
            .query_row(
                "SELECT 1 FROM schema_migrations WHERE id = ?1",
                rusqlite::params![id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if applied {
            continue;
        }
        match conn.execute_batch(sql) {
            Ok(()) => {}
            // Additive ALTER whose column is already present (fresh DB) — fine.
            Err(e) if e.to_string().contains("duplicate column name") => {}
            Err(e) => return Err(e.into()),
        }
        conn.execute(
            "INSERT INTO schema_migrations (id, applied_at) VALUES (?1, ?2)",
            rusqlite::params![id, now],
        )?;
    }
    Ok(())
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
    fn migrations_are_recorded_and_idempotent() {
        let repo = SqliteRepository::in_memory().unwrap();
        let count = |r: &SqliteRepository| -> i64 {
            r.conn
                .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                    row.get(0)
                })
                .unwrap()
        };
        // Baseline + the api_key_enc migration are recorded.
        assert!(count(&repo) >= 2);
        assert!(repo
            .conn
            .query_row(
                "SELECT 1 FROM schema_migrations WHERE id = '0002_tenant_llm_api_key_enc'",
                [],
                |_| Ok(())
            )
            .is_ok());
        // Re-running the runner on the same connection changes nothing.
        let before = count(&repo);
        run_migrations(&repo.conn).unwrap();
        assert_eq!(count(&repo), before);
        // And the migrated column is usable.
        repo.conn
            .execute(
                "INSERT INTO tenant_llm_config (tenant_id, api_key_enc) VALUES ('t', 'enc')",
                [],
            )
            .unwrap();
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
