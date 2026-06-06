//! Roles, permissions, memberships + invite lifecycle (PRD §6.2, §6.3 TEN-005).
//!
//! Pure authorization model: a [`Role`] grants a fixed set of [`Permission`]s
//! (Owner ⊃ Admin ⊃ Member ⊃ Viewer), a [`Membership`] binds a user to a tenant
//! with a role, and [`evaluate_accept`] is the invite state machine. Generating
//! and hashing invite tokens (and the DNS/email I/O) is host-side; this decides
//! who may do what.

use crate::{TenantId, UserId};

/// Tenant membership role (§6.2). Strictly ordered by capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Viewer,
    Member,
    Admin,
    Owner,
}

/// A capability that can be checked against a role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    ViewSummaries,
    CreateSummary,
    ManageSchedules,
    ManageMembers,
    ManageSettings,
    /// Billing is the tenant entity's concern (TEN-008) — owner only.
    ManageBilling,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Member => "member",
            Role::Admin => "admin",
            Role::Owner => "owner",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "viewer" => Role::Viewer,
            "member" => Role::Member,
            "admin" => Role::Admin,
            "owner" => Role::Owner,
            _ => return None,
        })
    }

    /// Whether this role grants `permission`. Higher roles inherit everything a
    /// lower role can do; the cutoffs are the only policy here.
    pub fn allows(self, permission: Permission) -> bool {
        let required = match permission {
            Permission::ViewSummaries => Role::Viewer,
            Permission::CreateSummary => Role::Member,
            Permission::ManageSchedules => Role::Admin,
            Permission::ManageMembers => Role::Admin,
            Permission::ManageSettings => Role::Admin,
            Permission::ManageBilling => Role::Owner,
        };
        self >= required
    }
}

/// A user's membership in a tenant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Membership {
    pub tenant_id: TenantId,
    pub user_id: UserId,
    pub role: Role,
}

impl Membership {
    pub fn new(tenant_id: TenantId, user_id: UserId, role: Role) -> Self {
        Self {
            tenant_id,
            user_id,
            role,
        }
    }

    /// Convenience: does this membership grant `permission`?
    pub fn can(&self, permission: Permission) -> bool {
        self.role.allows(permission)
    }
}

/// Lifecycle of an invite (TEN-005).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InviteStatus {
    Pending,
    Accepted,
    Revoked,
}

impl InviteStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            InviteStatus::Pending => "pending",
            InviteStatus::Accepted => "accepted",
            InviteStatus::Revoked => "revoked",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "pending" => InviteStatus::Pending,
            "accepted" => InviteStatus::Accepted,
            "revoked" => InviteStatus::Revoked,
            _ => return None,
        })
    }
}

/// An invite to join a tenant. The raw token is shown to the invitee once; only
/// its hash is stored (host computes it), like a refresh token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invite {
    pub token_hash: String,
    pub tenant_id: TenantId,
    pub email: String,
    pub role: Role,
    pub created_at: i64,
    pub expires_at: i64,
    pub status: InviteStatus,
}

/// Why an accept was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptReject {
    /// Already accepted or revoked.
    NotPending,
    /// Past its expiry.
    Expired,
}

/// Outcome of accepting an invite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptOutcome {
    /// Accept it and grant this role.
    Accept(Role),
    Reject(AcceptReject),
}

/// Pure accept policy: an invite may be accepted only while `Pending` and before
/// `expires_at`. Accepting a non-pending invite is idempotently refused (so a
/// double-accept can't re-grant), and an expired one is refused.
pub fn evaluate_accept(invite: &Invite, now: i64) -> AcceptOutcome {
    if invite.status != InviteStatus::Pending {
        AcceptOutcome::Reject(AcceptReject::NotPending)
    } else if now >= invite.expires_at {
        AcceptOutcome::Reject(AcceptReject::Expired)
    } else {
        AcceptOutcome::Accept(invite.role)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_hierarchy_is_ordered() {
        assert!(Role::Owner > Role::Admin);
        assert!(Role::Admin > Role::Member);
        assert!(Role::Member > Role::Viewer);
    }

    #[test]
    fn viewer_can_only_read() {
        let r = Role::Viewer;
        assert!(r.allows(Permission::ViewSummaries));
        assert!(!r.allows(Permission::CreateSummary));
        assert!(!r.allows(Permission::ManageMembers));
    }

    #[test]
    fn member_can_create_but_not_manage() {
        let r = Role::Member;
        assert!(r.allows(Permission::CreateSummary));
        assert!(!r.allows(Permission::ManageSchedules));
        assert!(!r.allows(Permission::ManageMembers));
    }

    #[test]
    fn admin_manages_but_not_billing() {
        let r = Role::Admin;
        assert!(r.allows(Permission::ManageMembers));
        assert!(r.allows(Permission::ManageSettings));
        assert!(r.allows(Permission::ManageSchedules));
        assert!(!r.allows(Permission::ManageBilling));
    }

    #[test]
    fn owner_can_do_everything_including_billing() {
        let r = Role::Owner;
        for p in [
            Permission::ViewSummaries,
            Permission::CreateSummary,
            Permission::ManageSchedules,
            Permission::ManageMembers,
            Permission::ManageSettings,
            Permission::ManageBilling,
        ] {
            assert!(r.allows(p));
        }
    }

    #[test]
    fn membership_can_delegates_to_role() {
        let m = Membership::new(
            TenantId::parse("t1").unwrap(),
            UserId::parse("u1").unwrap(),
            Role::Member,
        );
        assert!(m.can(Permission::CreateSummary));
        assert!(!m.can(Permission::ManageBilling));
    }

    #[test]
    fn role_round_trips() {
        for r in [Role::Viewer, Role::Member, Role::Admin, Role::Owner] {
            assert_eq!(Role::parse(r.as_str()), Some(r));
        }
        assert_eq!(Role::parse("god"), None);
    }

    #[test]
    fn invite_status_round_trips() {
        for s in [
            InviteStatus::Pending,
            InviteStatus::Accepted,
            InviteStatus::Revoked,
        ] {
            assert_eq!(InviteStatus::parse(s.as_str()), Some(s));
        }
        assert_eq!(InviteStatus::parse("expired"), None);
    }

    fn invite(status: InviteStatus, expires_at: i64) -> Invite {
        Invite {
            token_hash: "h".into(),
            tenant_id: TenantId::parse("t1").unwrap(),
            email: "new@example.com".into(),
            role: Role::Member,
            created_at: 0,
            expires_at,
            status,
        }
    }

    #[test]
    fn pending_unexpired_invite_accepts_with_role() {
        assert_eq!(
            evaluate_accept(&invite(InviteStatus::Pending, 1_000), 500),
            AcceptOutcome::Accept(Role::Member)
        );
    }

    #[test]
    fn expired_invite_is_rejected() {
        assert_eq!(
            evaluate_accept(&invite(InviteStatus::Pending, 1_000), 1_000),
            AcceptOutcome::Reject(AcceptReject::Expired)
        );
    }

    #[test]
    fn non_pending_invite_is_rejected_idempotently() {
        assert_eq!(
            evaluate_accept(&invite(InviteStatus::Accepted, 9_999), 1),
            AcceptOutcome::Reject(AcceptReject::NotPending)
        );
        assert_eq!(
            evaluate_accept(&invite(InviteStatus::Revoked, 9_999), 1),
            AcceptOutcome::Reject(AcceptReject::NotPending)
        );
    }
}
