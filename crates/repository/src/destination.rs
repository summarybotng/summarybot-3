//! Per-workspace summary delivery destinations (DSH-010/011).
//!
//! Where a workspace's summaries go *besides* the always-on dashboard: a webhook
//! URL, an email, etc. The address is opaque ciphertext here — the host encrypts
//! it with the operator master key (like a BYO LLM key, ADR-125 Phase 2b) and
//! the repository never sees the plaintext. Workspace-scoped (TEN-007); the
//! domain delivery policy ([`domain::resolve_delivery`]) gates the actual send.

use crate::SqliteRepository;
use anyhow::Result;
use domain::WorkspaceId;
use rusqlite::params;

/// A stored destination row. `kind` is the [`domain::DestinationKind`] as a
/// lowercase string (`"webhook"`, `"email"`); `address_enc` is the host's
/// ciphertext (never plaintext at rest).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredDestination {
    pub id: String,
    pub kind: String,
    pub address_enc: Option<String>,
    pub enabled: bool,
    pub created_at: i64,
}

/// Storage boundary for a workspace's delivery destinations.
pub trait DestinationRepository {
    /// All destinations for a workspace, oldest first.
    fn list_destinations(&self, workspace: &WorkspaceId) -> Result<Vec<StoredDestination>>;
    /// Insert or replace a destination (keyed by `(workspace, id)`).
    fn upsert_destination(&self, workspace: &WorkspaceId, dest: &StoredDestination) -> Result<()>;
    /// Remove a destination by id. Returns whether a row was removed.
    fn delete_destination(&self, workspace: &WorkspaceId, id: &str) -> Result<bool>;
}

impl DestinationRepository for SqliteRepository {
    fn list_destinations(&self, workspace: &WorkspaceId) -> Result<Vec<StoredDestination>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, address_enc, enabled, created_at
             FROM workspace_destinations WHERE workspace_id = ?1
             ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map(params![workspace.as_str()], |row| {
            Ok(StoredDestination {
                id: row.get(0)?,
                kind: row.get(1)?,
                address_enc: row.get(2)?,
                enabled: row.get::<_, i64>(3)? != 0,
                created_at: row.get(4)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    fn upsert_destination(&self, workspace: &WorkspaceId, dest: &StoredDestination) -> Result<()> {
        self.conn.execute(
            "INSERT INTO workspace_destinations
                 (workspace_id, id, kind, address_enc, enabled, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(workspace_id, id) DO UPDATE SET
                 kind = excluded.kind,
                 address_enc = excluded.address_enc,
                 enabled = excluded.enabled",
            params![
                workspace.as_str(),
                dest.id,
                dest.kind,
                dest.address_enc,
                dest.enabled as i64,
                dest.created_at,
            ],
        )?;
        Ok(())
    }

    fn delete_destination(&self, workspace: &WorkspaceId, id: &str) -> Result<bool> {
        let n = self.conn.execute(
            "DELETE FROM workspace_destinations WHERE workspace_id = ?1 AND id = ?2",
            params![workspace.as_str(), id],
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
    fn ws(id: &str) -> WorkspaceId {
        WorkspaceId::parse(id).unwrap()
    }
    fn dest(id: &str, enabled: bool) -> StoredDestination {
        StoredDestination {
            id: id.into(),
            kind: "webhook".into(),
            address_enc: Some(format!("cipher-{id}")),
            enabled,
            created_at: 100,
        }
    }

    #[test]
    fn upsert_list_update_delete_round_trip() {
        let repo = repo();
        assert!(repo.list_destinations(&ws("w1")).unwrap().is_empty());

        repo.upsert_destination(&ws("w1"), &dest("d1", true))
            .unwrap();
        repo.upsert_destination(&ws("w1"), &dest("d2", false))
            .unwrap();
        let got = repo.list_destinations(&ws("w1")).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].id, "d1");
        assert!(got[0].enabled);
        assert!(!got[1].enabled);
        assert_eq!(got[0].address_enc.as_deref(), Some("cipher-d1"));

        // Upsert replaces in place (toggle enabled, change address).
        let mut updated = dest("d1", false);
        updated.address_enc = Some("cipher-new".into());
        repo.upsert_destination(&ws("w1"), &updated).unwrap();
        let got = repo.list_destinations(&ws("w1")).unwrap();
        assert_eq!(got.len(), 2, "upsert must not duplicate");
        let d1 = got.iter().find(|d| d.id == "d1").unwrap();
        assert!(!d1.enabled);
        assert_eq!(d1.address_enc.as_deref(), Some("cipher-new"));

        // Scoped per workspace.
        assert!(repo.list_destinations(&ws("w2")).unwrap().is_empty());

        assert!(repo.delete_destination(&ws("w1"), "d1").unwrap());
        assert!(!repo.delete_destination(&ws("w1"), "d1").unwrap());
        assert_eq!(repo.list_destinations(&ws("w1")).unwrap().len(), 1);
    }
}
