//! Workspace / tenancy domain model (PRD §7.1, ADR-066/ADR-079).
//!
//! `workspace_id` is the canonical tenant key (WSP-002); a platform's native id
//! (`guild_id`, `team_id`, …) lives *only* on [`WorkspaceConnection::platform_id`].
//! Workspaces are created **explicitly** (WSP-009) and may attach zero or more
//! platform connections (WSP-007). Time is supplied by the caller (the host
//! owns the clock) so this crate stays pure.

use crate::{string_id, ValidationError, WorkspaceId};

/// A platform a workspace can connect to. Platform-native vocabulary
/// ("guild"/"team") stays inside adapters; the domain speaks "workspace".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    Discord,
    Slack,
    WhatsApp,
}

impl Platform {
    pub fn as_str(self) -> &'static str {
        match self {
            Platform::Discord => "discord",
            Platform::Slack => "slack",
            Platform::WhatsApp => "whatsapp",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, ValidationError> {
        match raw {
            "discord" => Ok(Platform::Discord),
            "slack" => Ok(Platform::Slack),
            "whatsapp" => Ok(Platform::WhatsApp),
            _ => Err(ValidationError::Invalid {
                field: "platform",
                reason: "unknown platform",
            }),
        }
    }
}

string_id!(TenantId, "tenant id", 256);
string_id!(UserId, "user id", 256);
string_id!(PlatformId, "platform id", 256);

/// Billing + isolation boundary (TEN-008: the tenant is the billing entity).
/// Branding (TEN-003) is deferred to Phase 8; modelled minimally here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tenant {
    pub id: TenantId,
    pub name: String,
    pub subdomain: Option<String>,     // TEN-001
    pub custom_domain: Option<String>, // TEN-002
}

impl Tenant {
    /// Maximum tenant display-name length.
    pub const MAX_NAME: usize = 200;

    pub fn new(
        id: TenantId,
        name: impl Into<String>,
        subdomain: Option<String>,
        custom_domain: Option<String>,
    ) -> Result<Self, ValidationError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(ValidationError::Empty {
                field: "tenant name",
            });
        }
        if name.len() > Self::MAX_NAME {
            return Err(ValidationError::TooLong {
                field: "tenant name",
                len: name.len(),
                max: Self::MAX_NAME,
            });
        }
        Ok(Self {
            id,
            name,
            subdomain,
            custom_domain,
        })
    }
}

/// A workspace: the primary tenant key replacing `guild_id` (WSP-002).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub tenant_id: TenantId,
    pub name: String,
    pub owner_user_id: UserId,
    /// Unix seconds; supplied by the host (the domain holds no clock).
    pub created_at: i64,
}

impl Workspace {
    pub const MAX_NAME: usize = 200;

    /// Explicit creation (WSP-009): a named workspace is created first, then
    /// platform connections are attached separately — never auto-created.
    pub fn create(
        id: WorkspaceId,
        tenant_id: TenantId,
        name: impl Into<String>,
        owner_user_id: UserId,
        created_at: i64,
    ) -> Result<Self, ValidationError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(ValidationError::Empty {
                field: "workspace name",
            });
        }
        if name.len() > Self::MAX_NAME {
            return Err(ValidationError::TooLong {
                field: "workspace name",
                len: name.len(),
                max: Self::MAX_NAME,
            });
        }
        Ok(Self {
            id,
            tenant_id,
            name,
            owner_user_id,
            created_at,
        })
    }
}

/// Maps an external platform id to a workspace (WSP-008). The pair
/// (platform, platform_id) is globally unique — enforced by the repository —
/// so a platform account binds to exactly one workspace (WSP-010).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceConnection {
    pub workspace_id: WorkspaceId,
    pub platform: Platform,
    pub platform_id: PlatformId,
}

impl WorkspaceConnection {
    pub fn new(workspace_id: WorkspaceId, platform: Platform, platform_id: PlatformId) -> Self {
        Self {
            workspace_id,
            platform,
            platform_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_roundtrips() {
        for p in [Platform::Discord, Platform::Slack, Platform::WhatsApp] {
            assert_eq!(Platform::parse(p.as_str()), Ok(p));
        }
        assert!(Platform::parse("irc").is_err());
    }

    #[test]
    fn workspace_requires_non_empty_name() {
        let id = WorkspaceId::parse("ws").unwrap();
        let tenant = TenantId::parse("t").unwrap();
        let owner = UserId::parse("u").unwrap();
        assert!(matches!(
            Workspace::create(id, tenant, "  ", owner, 0),
            Err(ValidationError::Empty { .. })
        ));
    }

    #[test]
    fn tenant_rejects_overlong_name() {
        let id = TenantId::parse("t").unwrap();
        let name = "x".repeat(Tenant::MAX_NAME + 1);
        assert!(matches!(
            Tenant::new(id, name, None, None),
            Err(ValidationError::TooLong { .. })
        ));
    }
}
