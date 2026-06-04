//! Workspace/tenant/connection persistence (PRD §12.2 part 1).
//!
//! Tenant isolation (TEN-007) is enforced *here*: workspace reads require a
//! `TenantId` and filter on it, so one tenant can never read another's rows.
//! The (platform, platform_id) uniqueness (WSP-008/WSP-010) is enforced by the
//! DB and surfaced as a typed [`AttachError::AlreadyBound`].

use crate::SqliteRepository;
use anyhow::Result;
use domain::{
    Platform, PlatformId, Tenant, TenantId, UserId, Workspace, WorkspaceConnection, WorkspaceId,
};
use rusqlite::{params, ErrorCode};

/// Outcome of attaching a platform connection.
#[derive(Debug)]
pub enum AttachError {
    /// (platform, platform_id) is already bound — reject, never reassign
    /// (WSP-010). Resolution goes through the claim/transfer workflow.
    AlreadyBound {
        platform: &'static str,
        platform_id: String,
    },
    /// Any other storage failure.
    Db(anyhow::Error),
}

impl std::fmt::Display for AttachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AttachError::AlreadyBound {
                platform,
                platform_id,
            } => write!(
                f,
                "{platform} account {platform_id} is already bound to a workspace"
            ),
            AttachError::Db(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for AttachError {}

impl From<anyhow::Error> for AttachError {
    fn from(e: anyhow::Error) -> Self {
        AttachError::Db(e)
    }
}

/// Storage boundary for workspaces. Every workspace read is tenant-scoped.
pub trait WorkspaceRepository {
    fn create_tenant(&self, tenant: &Tenant) -> Result<()>;
    /// Persist an explicitly-created workspace (WSP-009).
    fn create_workspace(&self, workspace: &Workspace) -> Result<()>;
    /// Fetch a workspace only if it belongs to `tenant` (TEN-007).
    fn get_workspace(&self, tenant: &TenantId, id: &WorkspaceId) -> Result<Option<Workspace>>;
    /// Bind a platform account to a workspace, rejecting an existing binding.
    fn attach_connection(&self, conn: &WorkspaceConnection) -> Result<(), AttachError>;
    /// Resolve which workspace a platform account maps to (WSP-008).
    fn resolve_by_connection(
        &self,
        platform: Platform,
        platform_id: &PlatformId,
    ) -> Result<Option<WorkspaceId>>;
    fn list_connections(&self, workspace: &WorkspaceId) -> Result<Vec<WorkspaceConnection>>;
}

/// Reconstruct a validated newtype from a stored string (defense in depth:
/// rows should already be valid, but we never trust storage blindly).
fn parse_field<T, E, F>(f: F, raw: String) -> Result<T>
where
    F: FnOnce(String) -> Result<T, E>,
    E: std::error::Error + Send + Sync + 'static,
{
    f(raw).map_err(anyhow::Error::new)
}

impl WorkspaceRepository for SqliteRepository {
    fn create_tenant(&self, tenant: &Tenant) -> Result<()> {
        self.conn.execute(
            "INSERT INTO tenants (id, name, subdomain, custom_domain)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                tenant.id.as_str(),
                tenant.name,
                tenant.subdomain,
                tenant.custom_domain,
            ],
        )?;
        Ok(())
    }

    fn create_workspace(&self, workspace: &Workspace) -> Result<()> {
        self.conn.execute(
            "INSERT INTO workspaces (id, tenant_id, name, owner_user_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                workspace.id.as_str(),
                workspace.tenant_id.as_str(),
                workspace.name,
                workspace.owner_user_id.as_str(),
                workspace.created_at,
            ],
        )?;
        Ok(())
    }

    fn get_workspace(&self, tenant: &TenantId, id: &WorkspaceId) -> Result<Option<Workspace>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, tenant_id, name, owner_user_id, created_at
             FROM workspaces WHERE id = ?1 AND tenant_id = ?2",
        )?;
        let mut rows = stmt.query(params![id.as_str(), tenant.as_str()])?;
        match rows.next()? {
            Some(row) => {
                let ws = Workspace {
                    id: parse_field(WorkspaceId::parse, row.get::<_, String>(0)?)?,
                    tenant_id: parse_field(TenantId::parse, row.get::<_, String>(1)?)?,
                    name: row.get(2)?,
                    owner_user_id: parse_field(UserId::parse, row.get::<_, String>(3)?)?,
                    created_at: row.get(4)?,
                };
                Ok(Some(ws))
            }
            None => Ok(None),
        }
    }

    fn attach_connection(&self, conn: &WorkspaceConnection) -> Result<(), AttachError> {
        let result = self.conn.execute(
            "INSERT INTO workspace_connections (workspace_id, platform, platform_id)
             VALUES (?1, ?2, ?3)",
            params![
                conn.workspace_id.as_str(),
                conn.platform.as_str(),
                conn.platform_id.as_str(),
            ],
        );
        match result {
            Ok(_) => Ok(()),
            // UNIQUE(platform, platform_id) violated -> already bound (WSP-010).
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == ErrorCode::ConstraintViolation =>
            {
                Err(AttachError::AlreadyBound {
                    platform: conn.platform.as_str(),
                    platform_id: conn.platform_id.as_str().to_string(),
                })
            }
            Err(e) => Err(AttachError::Db(anyhow::Error::new(e))),
        }
    }

    fn resolve_by_connection(
        &self,
        platform: Platform,
        platform_id: &PlatformId,
    ) -> Result<Option<WorkspaceId>> {
        let mut stmt = self.conn.prepare(
            "SELECT workspace_id FROM workspace_connections
             WHERE platform = ?1 AND platform_id = ?2",
        )?;
        let mut rows = stmt.query(params![platform.as_str(), platform_id.as_str()])?;
        match rows.next()? {
            Some(row) => Ok(Some(parse_field(
                WorkspaceId::parse,
                row.get::<_, String>(0)?,
            )?)),
            None => Ok(None),
        }
    }

    fn list_connections(&self, workspace: &WorkspaceId) -> Result<Vec<WorkspaceConnection>> {
        let mut stmt = self.conn.prepare(
            "SELECT platform, platform_id FROM workspace_connections
             WHERE workspace_id = ?1 ORDER BY platform, platform_id",
        )?;
        let rows = stmt.query_map(params![workspace.as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (platform, platform_id) = row?;
            out.push(WorkspaceConnection::new(
                workspace.clone(),
                Platform::parse(&platform).map_err(anyhow::Error::new)?,
                parse_field(PlatformId::parse, platform_id)?,
            ));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> SqliteRepository {
        SqliteRepository::in_memory().unwrap()
    }

    fn workspace(id: &str, tenant: &str) -> Workspace {
        Workspace::create(
            WorkspaceId::parse(id).unwrap(),
            TenantId::parse(tenant).unwrap(),
            "Acme",
            UserId::parse("owner").unwrap(),
            1_700_000_000,
        )
        .unwrap()
    }

    #[test]
    fn explicit_create_then_tenant_scoped_get() {
        let repo = repo();
        let t = TenantId::parse("t1").unwrap();
        repo.create_tenant(&Tenant::new(t.clone(), "Acme", None, None).unwrap())
            .unwrap();
        repo.create_workspace(&workspace("ws1", "t1")).unwrap();

        let got = repo
            .get_workspace(&t, &WorkspaceId::parse("ws1").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(got.name, "Acme");
        assert_eq!(got.created_at, 1_700_000_000);
    }

    #[test]
    fn tenant_isolation_hides_other_tenants_workspace() {
        let repo = repo();
        repo.create_workspace(&workspace("ws1", "t1")).unwrap();
        let other = TenantId::parse("t2").unwrap();
        // t2 must not see t1's workspace.
        assert!(repo
            .get_workspace(&other, &WorkspaceId::parse("ws1").unwrap())
            .unwrap()
            .is_none());
    }

    #[test]
    fn connection_maps_to_workspace_and_rejects_double_bind() {
        let repo = repo();
        repo.create_workspace(&workspace("ws1", "t1")).unwrap();
        repo.create_workspace(&workspace("ws2", "t1")).unwrap();

        let conn = WorkspaceConnection::new(
            WorkspaceId::parse("ws1").unwrap(),
            Platform::Discord,
            PlatformId::parse("guild-42").unwrap(),
        );
        repo.attach_connection(&conn).unwrap();

        // WSP-008: the platform account resolves to ws1.
        let resolved = repo
            .resolve_by_connection(Platform::Discord, &PlatformId::parse("guild-42").unwrap())
            .unwrap();
        assert_eq!(resolved.unwrap().as_str(), "ws1");

        // WSP-010: re-binding the same account (even to another workspace) is rejected.
        let steal = WorkspaceConnection::new(
            WorkspaceId::parse("ws2").unwrap(),
            Platform::Discord,
            PlatformId::parse("guild-42").unwrap(),
        );
        assert!(matches!(
            repo.attach_connection(&steal),
            Err(AttachError::AlreadyBound { .. })
        ));

        // Same platform_id on a *different* platform is fine (uniqueness is per-platform).
        let slack = WorkspaceConnection::new(
            WorkspaceId::parse("ws2").unwrap(),
            Platform::Slack,
            PlatformId::parse("guild-42").unwrap(),
        );
        assert!(repo.attach_connection(&slack).is_ok());

        assert_eq!(
            repo.list_connections(&WorkspaceId::parse("ws1").unwrap())
                .unwrap()
                .len(),
            1
        );
    }
}
