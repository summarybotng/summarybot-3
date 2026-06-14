//! Per-tenant delivery plugin enablement + account credentials (the tenant half
//! of the two-layer plugin model; ADR-126).
//!
//! A tenant admin **enables** a delivery plugin and **connects/configures** its
//! shared account credentials once (Confluence base+creds, SMTP creds, a Google
//! Drive refresh token captured via OAuth). Workspaces then add destinations that
//! pick only the non-secret *target* (channel / space key / folder). At delivery
//! time the tenant credentials are merged under the workspace target.
//!
//! `config_enc` is encrypted JSON of the tenant-scoped fields (the host's
//! secretbox / operator master key, like [`TenantLlmConfig`](crate::TenantLlmConfig));
//! the repository never sees the plaintext. Tenant-scoped like everything else
//! (TEN-007).

use crate::SqliteRepository;
use anyhow::Result;
use domain::TenantId;
use rusqlite::{params, OptionalExtension};

/// A tenant's settings for one delivery plugin `kind`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TenantPlugin {
    /// The plugin kind id this row is keyed on ("confluence", "gdrive", …).
    pub kind: String,
    /// Whether the tenant has enabled this plugin (workspaces may use it).
    pub enabled: bool,
    /// Encrypted JSON of the tenant-scoped config fields (+ any OAuth refresh
    /// token). `None` when nothing has been configured yet.
    pub config_enc: Option<String>,
    /// For OAuth plugins: a refresh token has been captured via the connect flow.
    pub connected: bool,
    /// ADR-131: a platform operator's hard veto — when true this plugin is off
    /// for the tenant regardless of `enabled`, and the tenant can't override it.
    pub operator_disabled: bool,
    pub updated_at: i64,
}

/// Storage boundary for per-tenant plugin enablement + config.
pub trait TenantPluginRepository {
    /// Fetch a tenant's settings for one plugin kind, or `None` if never set.
    fn get_tenant_plugin(&self, tenant: &TenantId, kind: &str) -> Result<Option<TenantPlugin>>;
    /// All plugin rows a tenant has touched (enabled or configured), by kind.
    fn list_tenant_plugins(&self, tenant: &TenantId) -> Result<Vec<TenantPlugin>>;
    /// Insert or replace a tenant's settings for `plugin.kind`.
    fn upsert_tenant_plugin(&self, tenant: &TenantId, plugin: &TenantPlugin) -> Result<()>;
    /// Remove a tenant's row for a plugin kind. Returns whether a row was removed.
    fn delete_tenant_plugin(&self, tenant: &TenantId, kind: &str) -> Result<bool>;
}

fn row_to_plugin(row: &rusqlite::Row) -> rusqlite::Result<TenantPlugin> {
    Ok(TenantPlugin {
        kind: row.get(0)?,
        enabled: row.get(1)?,
        config_enc: row.get(2)?,
        connected: row.get(3)?,
        updated_at: row.get(4)?,
        operator_disabled: row.get(5)?,
    })
}

impl TenantPluginRepository for SqliteRepository {
    fn get_tenant_plugin(&self, tenant: &TenantId, kind: &str) -> Result<Option<TenantPlugin>> {
        self.conn
            .query_row(
                "SELECT kind, enabled, config_enc, connected, updated_at, operator_disabled
                 FROM tenant_plugins WHERE tenant_id = ?1 AND kind = ?2",
                params![tenant.as_str(), kind],
                row_to_plugin,
            )
            .optional()
            .map_err(Into::into)
    }

    fn list_tenant_plugins(&self, tenant: &TenantId) -> Result<Vec<TenantPlugin>> {
        let mut stmt = self.conn.prepare(
            "SELECT kind, enabled, config_enc, connected, updated_at, operator_disabled
             FROM tenant_plugins WHERE tenant_id = ?1 ORDER BY kind",
        )?;
        let rows = stmt.query_map(params![tenant.as_str()], row_to_plugin)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    fn upsert_tenant_plugin(&self, tenant: &TenantId, plugin: &TenantPlugin) -> Result<()> {
        self.conn.execute(
            "INSERT INTO tenant_plugins
                 (tenant_id, kind, enabled, config_enc, connected, updated_at, operator_disabled)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(tenant_id, kind) DO UPDATE SET enabled = excluded.enabled,
                                                        config_enc = excluded.config_enc,
                                                        connected = excluded.connected,
                                                        updated_at = excluded.updated_at,
                                                        operator_disabled = excluded.operator_disabled",
            params![
                tenant.as_str(),
                plugin.kind,
                plugin.enabled,
                plugin.config_enc,
                plugin.connected,
                plugin.updated_at,
                plugin.operator_disabled,
            ],
        )?;
        Ok(())
    }

    fn delete_tenant_plugin(&self, tenant: &TenantId, kind: &str) -> Result<bool> {
        let n = self.conn.execute(
            "DELETE FROM tenant_plugins WHERE tenant_id = ?1 AND kind = ?2",
            params![tenant.as_str(), kind],
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
    fn t(id: &str) -> TenantId {
        TenantId::parse(id).unwrap()
    }

    #[test]
    fn set_get_list_update_clear_round_trip() {
        let repo = repo();
        assert!(repo.get_tenant_plugin(&t("t1"), "confluence").unwrap().is_none());

        repo.upsert_tenant_plugin(
            &t("t1"),
            &TenantPlugin {
                kind: "confluence".into(),
                enabled: true,
                config_enc: Some("cipher-creds".into()),
                connected: false,
                updated_at: 100,
                ..Default::default()
            },
        )
        .unwrap();
        let got = repo.get_tenant_plugin(&t("t1"), "confluence").unwrap().unwrap();
        assert!(got.enabled);
        assert_eq!(got.config_enc.as_deref(), Some("cipher-creds"));
        assert!(!got.connected);

        // A second plugin, OAuth-connected.
        repo.upsert_tenant_plugin(
            &t("t1"),
            &TenantPlugin {
                kind: "gdrive".into(),
                enabled: true,
                config_enc: Some("cipher-token".into()),
                connected: true,
                updated_at: 200,
                ..Default::default()
            },
        )
        .unwrap();
        let list = repo.list_tenant_plugins(&t("t1")).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].kind, "confluence"); // ordered by kind
        assert_eq!(list[1].kind, "gdrive");
        assert!(list[1].connected);

        // Upsert replaces (disable + clear config).
        repo.upsert_tenant_plugin(
            &t("t1"),
            &TenantPlugin {
                kind: "confluence".into(),
                enabled: false,
                config_enc: None,
                connected: false,
                updated_at: 300,
                ..Default::default()
            },
        )
        .unwrap();
        let got = repo.get_tenant_plugin(&t("t1"), "confluence").unwrap().unwrap();
        assert!(!got.enabled);
        assert!(got.config_enc.is_none());

        // Scoped per tenant.
        assert!(repo.list_tenant_plugins(&t("t2")).unwrap().is_empty());

        assert!(repo.delete_tenant_plugin(&t("t1"), "gdrive").unwrap());
        assert!(!repo.delete_tenant_plugin(&t("t1"), "gdrive").unwrap());
        assert_eq!(repo.list_tenant_plugins(&t("t1")).unwrap().len(), 1);
    }

    #[test]
    fn operator_disabled_flag_round_trips_independently_of_enabled() {
        let repo = repo();
        repo.upsert_tenant_plugin(
            &t("t1"),
            &TenantPlugin {
                kind: "webhook".into(),
                enabled: true, // tenant wants it on…
                operator_disabled: true, // …but the operator vetoes it
                updated_at: 10,
                ..Default::default()
            },
        )
        .unwrap();
        let got = repo.get_tenant_plugin(&t("t1"), "webhook").unwrap().unwrap();
        assert!(got.enabled);
        assert!(got.operator_disabled);
        // Default is false for a row that never set it.
        repo.upsert_tenant_plugin(
            &t("t1"),
            &TenantPlugin { kind: "email".into(), enabled: true, updated_at: 11, ..Default::default() },
        )
        .unwrap();
        assert!(!repo.get_tenant_plugin(&t("t1"), "email").unwrap().unwrap().operator_disabled);
    }
}
