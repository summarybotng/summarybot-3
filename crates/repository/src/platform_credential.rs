//! Per-workspace platform credentials (ADR-128) — e.g. a Discord bot token.
//!
//! The secret to reach a live platform source (`workspace_connections`, ADR-120,
//! records *which* source; this holds the secret to *reach* it). Stored as opaque
//! ciphertext — the host encrypts with the operator master key (like a BYO LLM
//! key, ADR-125 2b, and delivery configs, ADR-126); the repository never sees the
//! plaintext. Workspace-scoped (TEN-007), one credential per platform.

use crate::SqliteRepository;
use anyhow::Result;
use domain::WorkspaceId;
use rusqlite::{params, OptionalExtension};

/// Storage boundary for a workspace's per-platform credentials.
pub trait PlatformCredentialRepository {
    /// Insert or replace the credential for `(workspace, platform)`. `token_enc`
    /// is the host's ciphertext (never plaintext at rest).
    fn set_platform_token(
        &self,
        workspace: &WorkspaceId,
        platform: &str,
        token_enc: &str,
        created_at: i64,
    ) -> Result<()>;
    /// Fetch the stored ciphertext for `(workspace, platform)`, if any.
    fn get_platform_token(&self, workspace: &WorkspaceId, platform: &str)
        -> Result<Option<String>>;
    /// Remove the credential. Returns whether a row was removed.
    fn delete_platform_token(&self, workspace: &WorkspaceId, platform: &str) -> Result<bool>;
}

impl PlatformCredentialRepository for SqliteRepository {
    fn set_platform_token(
        &self,
        workspace: &WorkspaceId,
        platform: &str,
        token_enc: &str,
        created_at: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO platform_credentials (workspace_id, platform, token_enc, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(workspace_id, platform) DO UPDATE SET
                 token_enc = excluded.token_enc,
                 created_at = excluded.created_at",
            params![workspace.as_str(), platform, token_enc, created_at],
        )?;
        Ok(())
    }

    fn get_platform_token(
        &self,
        workspace: &WorkspaceId,
        platform: &str,
    ) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT token_enc FROM platform_credentials
                 WHERE workspace_id = ?1 AND platform = ?2",
                params![workspace.as_str(), platform],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(Into::into)
    }

    fn delete_platform_token(&self, workspace: &WorkspaceId, platform: &str) -> Result<bool> {
        let n = self.conn.execute(
            "DELETE FROM platform_credentials WHERE workspace_id = ?1 AND platform = ?2",
            params![workspace.as_str(), platform],
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

    #[test]
    fn set_get_update_delete_round_trip() {
        let repo = repo();
        assert_eq!(repo.get_platform_token(&ws("w1"), "discord").unwrap(), None);

        repo.set_platform_token(&ws("w1"), "discord", "cipher-1", 100)
            .unwrap();
        assert_eq!(
            repo.get_platform_token(&ws("w1"), "discord").unwrap(),
            Some("cipher-1".to_string())
        );

        // Upsert replaces in place.
        repo.set_platform_token(&ws("w1"), "discord", "cipher-2", 200)
            .unwrap();
        assert_eq!(
            repo.get_platform_token(&ws("w1"), "discord").unwrap(),
            Some("cipher-2".to_string())
        );

        // Scoped per workspace + platform.
        assert_eq!(repo.get_platform_token(&ws("w2"), "discord").unwrap(), None);
        assert_eq!(repo.get_platform_token(&ws("w1"), "slack").unwrap(), None);

        assert!(repo.delete_platform_token(&ws("w1"), "discord").unwrap());
        assert!(!repo.delete_platform_token(&ws("w1"), "discord").unwrap());
        assert_eq!(repo.get_platform_token(&ws("w1"), "discord").unwrap(), None);
    }
}
