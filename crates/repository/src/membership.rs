//! Membership + invite persistence (PRD §6.2/§6.3 TEN-005).
//!
//! The persistence half of the Phase 8 tenancy model: [`Membership`]s bind a
//! user to a tenant with a [`Role`], and [`Invite`]s carry the pending-join
//! state machine. Tenant isolation (TEN-007) is enforced here — every read is
//! scoped to a `TenantId`, so one tenant can never see another's members or
//! invites. The authorization *policy* (who may do what) lives in the domain
//! ([`Role::allows`]); this only stores and transitions.
//!
//! [`accept_invite`](MembershipRepository::accept_invite) folds the domain's
//! pure [`evaluate_accept`] together with the storage transition: on a valid
//! accept it atomically marks the invite accepted and grants the membership, so
//! a double-accept can't re-grant (the second call sees `NotPending`).

use crate::SqliteRepository;
use anyhow::{anyhow, Result};
use domain::{
    evaluate_accept, AcceptOutcome, AcceptReject, Invite, InviteStatus, Membership, Role, TenantId,
    UserId,
};
use rusqlite::{params, OptionalExtension};

/// Storage boundary for tenant memberships and invites. Every read is
/// tenant-scoped (TEN-007); invites are addressed by their opaque token hash.
pub trait MembershipRepository {
    /// Insert or update a user's role in a tenant (idempotent on the
    /// `(tenant, user)` key).
    fn upsert_membership(&self, membership: &Membership) -> Result<()>;
    /// Fetch a user's membership in a tenant, or `None` if they aren't a member.
    fn get_membership(&self, tenant: &TenantId, user: &UserId) -> Result<Option<Membership>>;
    /// All members of a tenant, ordered by user id for determinism.
    fn list_members(&self, tenant: &TenantId) -> Result<Vec<Membership>>;
    /// Remove a membership. Returns whether a row was removed.
    fn remove_membership(&self, tenant: &TenantId, user: &UserId) -> Result<bool>;

    /// Record a freshly-issued invite (status is whatever the caller set,
    /// normally `Pending`).
    fn create_invite(&self, invite: &Invite) -> Result<()>;
    /// Look up an invite by its token hash.
    fn get_invite(&self, token_hash: &str) -> Result<Option<Invite>>;
    /// All invites for a tenant, newest first.
    fn list_invites(&self, tenant: &TenantId) -> Result<Vec<Invite>>;
    /// Force an invite's status (e.g. revoke). Returns whether a row changed.
    fn set_invite_status(&self, token_hash: &str, status: InviteStatus) -> Result<bool>;

    /// Accept an invite *for* `user`: evaluate the pure accept policy
    /// ([`evaluate_accept`]) and, only on `Accept`, atomically mark the invite
    /// accepted and grant the membership. The transaction makes the double
    /// guarantee — accept-and-grant happen together or not at all — so a
    /// concurrent or repeated accept can't re-grant (it sees `NotPending`).
    /// Returns the outcome; `Reject` leaves storage untouched.
    fn accept_invite(&self, token_hash: &str, user: &UserId, now: i64) -> Result<AcceptOutcome>;
}

/// Reconstruct a [`Role`] from a stored string (storage should already be
/// valid; we never trust it blindly — TEN-007 defense in depth).
fn role_from_row(raw: &str) -> Result<Role> {
    Role::parse(raw).ok_or_else(|| anyhow!("invalid role in storage: {raw}"))
}

fn status_from_row(raw: &str) -> Result<InviteStatus> {
    InviteStatus::parse(raw).ok_or_else(|| anyhow!("invalid invite status in storage: {raw}"))
}

/// Build an [`Invite`] from a query row laid out as the `SELECT` below.
fn invite_from_row(row: &rusqlite::Row) -> rusqlite::Result<Result<Invite>> {
    let token_hash: String = row.get(0)?;
    let tenant_raw: String = row.get(1)?;
    let email: String = row.get(2)?;
    let role_raw: String = row.get(3)?;
    let created_at: i64 = row.get(4)?;
    let expires_at: i64 = row.get(5)?;
    let status_raw: String = row.get(6)?;
    Ok((|| {
        Ok(Invite {
            token_hash,
            tenant_id: TenantId::parse(tenant_raw).map_err(anyhow::Error::new)?,
            email,
            role: role_from_row(&role_raw)?,
            created_at,
            expires_at,
            status: status_from_row(&status_raw)?,
        })
    })())
}

impl MembershipRepository for SqliteRepository {
    fn upsert_membership(&self, membership: &Membership) -> Result<()> {
        self.conn.execute(
            "INSERT INTO memberships (tenant_id, user_id, role) VALUES (?1, ?2, ?3)
             ON CONFLICT(tenant_id, user_id) DO UPDATE SET role = excluded.role",
            params![
                membership.tenant_id.as_str(),
                membership.user_id.as_str(),
                membership.role.as_str(),
            ],
        )?;
        Ok(())
    }

    fn get_membership(&self, tenant: &TenantId, user: &UserId) -> Result<Option<Membership>> {
        let row = self
            .conn
            .query_row(
                "SELECT role FROM memberships WHERE tenant_id = ?1 AND user_id = ?2",
                params![tenant.as_str(), user.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match row {
            Some(role_raw) => Ok(Some(Membership::new(
                tenant.clone(),
                user.clone(),
                role_from_row(&role_raw)?,
            ))),
            None => Ok(None),
        }
    }

    fn list_members(&self, tenant: &TenantId) -> Result<Vec<Membership>> {
        let mut stmt = self.conn.prepare(
            "SELECT user_id, role FROM memberships WHERE tenant_id = ?1 ORDER BY user_id",
        )?;
        let rows = stmt.query_map(params![tenant.as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (user_raw, role_raw) = row?;
            out.push(Membership::new(
                tenant.clone(),
                UserId::parse(user_raw).map_err(anyhow::Error::new)?,
                role_from_row(&role_raw)?,
            ));
        }
        Ok(out)
    }

    fn remove_membership(&self, tenant: &TenantId, user: &UserId) -> Result<bool> {
        let n = self.conn.execute(
            "DELETE FROM memberships WHERE tenant_id = ?1 AND user_id = ?2",
            params![tenant.as_str(), user.as_str()],
        )?;
        Ok(n > 0)
    }

    fn create_invite(&self, invite: &Invite) -> Result<()> {
        self.conn.execute(
            "INSERT INTO invites
               (token_hash, tenant_id, email, role, created_at, expires_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                invite.token_hash,
                invite.tenant_id.as_str(),
                invite.email,
                invite.role.as_str(),
                invite.created_at,
                invite.expires_at,
                invite.status.as_str(),
            ],
        )?;
        Ok(())
    }

    fn get_invite(&self, token_hash: &str) -> Result<Option<Invite>> {
        let mut stmt = self.conn.prepare(
            "SELECT token_hash, tenant_id, email, role, created_at, expires_at, status
             FROM invites WHERE token_hash = ?1",
        )?;
        let row = stmt
            .query_row(params![token_hash], invite_from_row)
            .optional()?;
        row.transpose()
    }

    fn list_invites(&self, tenant: &TenantId) -> Result<Vec<Invite>> {
        let mut stmt = self.conn.prepare(
            "SELECT token_hash, tenant_id, email, role, created_at, expires_at, status
             FROM invites WHERE tenant_id = ?1 ORDER BY created_at DESC, token_hash",
        )?;
        let rows = stmt.query_map(params![tenant.as_str()], invite_from_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    fn set_invite_status(&self, token_hash: &str, status: InviteStatus) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE invites SET status = ?2 WHERE token_hash = ?1",
            params![token_hash, status.as_str()],
        )?;
        Ok(n > 0)
    }

    fn accept_invite(&self, token_hash: &str, user: &UserId, now: i64) -> Result<AcceptOutcome> {
        let tx = self.conn.unchecked_transaction()?;
        let invite = {
            let mut stmt = tx.prepare(
                "SELECT token_hash, tenant_id, email, role, created_at, expires_at, status
                 FROM invites WHERE token_hash = ?1",
            )?;
            stmt.query_row(params![token_hash], invite_from_row)
                .optional()?
                .transpose()?
        };
        let Some(invite) = invite else {
            // No such invite: treat as a non-pending (un-acceptable) outcome
            // rather than an error, so callers map it to a clean 4xx.
            return Ok(AcceptOutcome::Reject(AcceptReject::NotPending));
        };
        let outcome = evaluate_accept(&invite, now);
        if let AcceptOutcome::Accept(role) = outcome {
            tx.execute(
                "UPDATE invites SET status = ?2 WHERE token_hash = ?1",
                params![token_hash, InviteStatus::Accepted.as_str()],
            )?;
            tx.execute(
                "INSERT INTO memberships (tenant_id, user_id, role) VALUES (?1, ?2, ?3)
                 ON CONFLICT(tenant_id, user_id) DO UPDATE SET role = excluded.role",
                params![invite.tenant_id.as_str(), user.as_str(), role.as_str()],
            )?;
        }
        tx.commit()?;
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> SqliteRepository {
        SqliteRepository::in_memory().unwrap()
    }

    fn tenant(id: &str) -> TenantId {
        TenantId::parse(id).unwrap()
    }

    fn user(id: &str) -> UserId {
        UserId::parse(id).unwrap()
    }

    fn invite(token: &str, tenant_id: &str, role: Role, expires_at: i64) -> Invite {
        Invite {
            token_hash: token.into(),
            tenant_id: tenant(tenant_id),
            email: "new@example.com".into(),
            role,
            created_at: 0,
            expires_at,
            status: InviteStatus::Pending,
        }
    }

    #[test]
    fn membership_upsert_get_and_role_change() {
        let repo = repo();
        repo.upsert_membership(&Membership::new(tenant("t1"), user("u1"), Role::Member))
            .unwrap();
        let got = repo
            .get_membership(&tenant("t1"), &user("u1"))
            .unwrap()
            .unwrap();
        assert_eq!(got.role, Role::Member);

        // Upsert again with a higher role: it updates, not duplicates.
        repo.upsert_membership(&Membership::new(tenant("t1"), user("u1"), Role::Admin))
            .unwrap();
        assert_eq!(
            repo.get_membership(&tenant("t1"), &user("u1"))
                .unwrap()
                .unwrap()
                .role,
            Role::Admin
        );
        assert_eq!(repo.list_members(&tenant("t1")).unwrap().len(), 1);
    }

    #[test]
    fn tenant_isolation_hides_other_tenants_members() {
        let repo = repo();
        repo.upsert_membership(&Membership::new(tenant("t1"), user("u1"), Role::Owner))
            .unwrap();
        // t2 sees nothing of t1's.
        assert!(repo
            .get_membership(&tenant("t2"), &user("u1"))
            .unwrap()
            .is_none());
        assert!(repo.list_members(&tenant("t2")).unwrap().is_empty());
    }

    #[test]
    fn remove_membership_reports_change() {
        let repo = repo();
        repo.upsert_membership(&Membership::new(tenant("t1"), user("u1"), Role::Member))
            .unwrap();
        assert!(repo.remove_membership(&tenant("t1"), &user("u1")).unwrap());
        // Second remove is a no-op.
        assert!(!repo.remove_membership(&tenant("t1"), &user("u1")).unwrap());
    }

    #[test]
    fn invite_create_get_list_round_trip() {
        let repo = repo();
        repo.create_invite(&invite("h1", "t1", Role::Member, 1_000))
            .unwrap();
        let got = repo.get_invite("h1").unwrap().unwrap();
        assert_eq!(got.tenant_id.as_str(), "t1");
        assert_eq!(got.role, Role::Member);
        assert_eq!(got.status, InviteStatus::Pending);
        assert_eq!(repo.list_invites(&tenant("t1")).unwrap().len(), 1);
        assert!(repo.list_invites(&tenant("other")).unwrap().is_empty());
    }

    #[test]
    fn accept_pending_invite_grants_membership_and_marks_accepted() {
        let repo = repo();
        repo.create_invite(&invite("h1", "t1", Role::Admin, 1_000))
            .unwrap();
        let outcome = repo.accept_invite("h1", &user("u1"), 500).unwrap();
        assert_eq!(outcome, AcceptOutcome::Accept(Role::Admin));

        // Membership granted at the invited role.
        assert_eq!(
            repo.get_membership(&tenant("t1"), &user("u1"))
                .unwrap()
                .unwrap()
                .role,
            Role::Admin
        );
        // Invite is now accepted.
        assert_eq!(
            repo.get_invite("h1").unwrap().unwrap().status,
            InviteStatus::Accepted
        );
    }

    #[test]
    fn double_accept_does_not_regrant() {
        let repo = repo();
        repo.create_invite(&invite("h1", "t1", Role::Member, 1_000))
            .unwrap();
        assert_eq!(
            repo.accept_invite("h1", &user("u1"), 500).unwrap(),
            AcceptOutcome::Accept(Role::Member)
        );
        // A second accept (e.g. by a different user) is idempotently refused.
        assert_eq!(
            repo.accept_invite("h1", &user("u2"), 500).unwrap(),
            AcceptOutcome::Reject(AcceptReject::NotPending)
        );
        // u2 never became a member.
        assert!(repo
            .get_membership(&tenant("t1"), &user("u2"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn expired_invite_is_rejected_and_grants_nothing() {
        let repo = repo();
        repo.create_invite(&invite("h1", "t1", Role::Member, 1_000))
            .unwrap();
        assert_eq!(
            repo.accept_invite("h1", &user("u1"), 1_000).unwrap(),
            AcceptOutcome::Reject(AcceptReject::Expired)
        );
        assert!(repo
            .get_membership(&tenant("t1"), &user("u1"))
            .unwrap()
            .is_none());
        // Still pending — an expired-but-untouched invite isn't auto-marked.
        assert_eq!(
            repo.get_invite("h1").unwrap().unwrap().status,
            InviteStatus::Pending
        );
    }

    #[test]
    fn revoked_invite_cannot_be_accepted() {
        let repo = repo();
        repo.create_invite(&invite("h1", "t1", Role::Member, 1_000))
            .unwrap();
        assert!(repo.set_invite_status("h1", InviteStatus::Revoked).unwrap());
        assert_eq!(
            repo.accept_invite("h1", &user("u1"), 500).unwrap(),
            AcceptOutcome::Reject(AcceptReject::NotPending)
        );
    }

    #[test]
    fn accepting_unknown_invite_is_a_clean_reject() {
        let repo = repo();
        assert_eq!(
            repo.accept_invite("ghost", &user("u1"), 500).unwrap(),
            AcceptOutcome::Reject(AcceptReject::NotPending)
        );
    }
}
