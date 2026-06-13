//! Host-side invite I/O (PRD §6.3 TEN-005) — the seam that turns the pure
//! invite policy into real tokens + storage.
//!
//! The split mirrors auth (§12.0): the **domain** owns the accept policy
//! ([`evaluate_accept`](domain::evaluate_accept)), the **repository** owns the
//! atomic accept-and-grant transition, and this service owns the I/O the domain
//! avoids — generating the opaque invite token, hashing it (only the hash is
//! stored, like a refresh token), and stamping created/expiry times. The raw
//! token is shown to the invitee exactly once.

use crate::auth::{generate_token, sha256_hex, AuthError};
use domain::{AcceptOutcome, Invite, InviteStatus, Role, TenantId, UserId};
use repository::MembershipRepository;

/// Default invite lifetime: 7 days.
pub const DEFAULT_INVITE_TTL_SECS: i64 = 7 * 24 * 60 * 60;

/// A freshly issued invite: the stored [`Invite`] plus the raw token, returned
/// to the caller **once** so it can be sent to the invitee. Only the hash on
/// `invite.token_hash` is persisted.
pub struct IssuedInvite {
    pub invite: Invite,
    /// The opaque bearer token to deliver to the invitee; never stored.
    pub raw_token: String,
}

/// Ties the pure invite policy to token I/O + storage. Generic over a backend
/// implementing [`MembershipRepository`] (the SQLite repo does).
pub struct InviteService<'a, R> {
    repo: &'a R,
    ttl_secs: i64,
}

impl<'a, R: MembershipRepository> InviteService<'a, R> {
    /// Build with the default invite lifetime.
    pub fn new(repo: &'a R) -> Self {
        Self {
            repo,
            ttl_secs: DEFAULT_INVITE_TTL_SECS,
        }
    }

    /// Override the invite lifetime (seconds).
    pub fn with_ttl(mut self, ttl_secs: i64) -> Self {
        self.ttl_secs = ttl_secs;
        self
    }

    /// Issue an invite: generate an opaque token, persist only its hash, and
    /// return the raw token once for delivery.
    pub fn issue(
        &self,
        tenant: TenantId,
        email: impl Into<String>,
        role: Role,
        now: i64,
    ) -> Result<IssuedInvite, AuthError> {
        let raw_token = generate_token()?;
        let invite = Invite {
            token_hash: sha256_hex(&raw_token),
            tenant_id: tenant,
            email: email.into(),
            role,
            created_at: now,
            expires_at: now + self.ttl_secs,
            status: InviteStatus::Pending,
        };
        self.repo.create_invite(&invite)?;
        Ok(IssuedInvite { invite, raw_token })
    }

    /// Accept an invite by its raw token (hashed here) on behalf of `user`. The
    /// atomic accept-and-grant transition is the repository's job; this only
    /// resolves the token to its stored hash.
    pub fn accept(
        &self,
        raw_token: &str,
        user: &UserId,
        now: i64,
    ) -> Result<AcceptOutcome, AuthError> {
        Ok(self.repo.accept_invite(&sha256_hex(raw_token), user, now)?)
    }

    /// Revoke an invite by its raw token. Returns whether a row changed.
    pub fn revoke(&self, raw_token: &str) -> Result<bool, AuthError> {
        self.revoke_by_hash(&sha256_hex(raw_token))
    }

    /// Revoke by the stored `token_hash` — what an admin sees in the invite list
    /// (the raw token is shown only once at issue). Returns whether a row changed.
    pub fn revoke_by_hash(&self, token_hash: &str) -> Result<bool, AuthError> {
        Ok(self
            .repo
            .set_invite_status(token_hash, InviteStatus::Revoked)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::AcceptReject;
    use repository::SqliteRepository;

    fn repo() -> SqliteRepository {
        SqliteRepository::in_memory().unwrap()
    }

    fn tenant() -> TenantId {
        TenantId::parse("t1").unwrap()
    }

    #[test]
    fn issue_stores_only_the_hash_and_returns_a_raw_token() {
        let repo = repo();
        let issued = InviteService::new(&repo)
            .issue(tenant(), "new@example.com", Role::Member, 1_000)
            .unwrap();
        // The raw token is opaque and is NOT what we stored.
        assert!(!issued.raw_token.is_empty());
        assert_ne!(issued.raw_token, issued.invite.token_hash);
        // The stored invite is found by hash, pending, with the right expiry.
        let stored = repo.get_invite(&issued.invite.token_hash).unwrap().unwrap();
        assert_eq!(stored.status, InviteStatus::Pending);
        assert_eq!(stored.expires_at, 1_000 + DEFAULT_INVITE_TTL_SECS);
    }

    #[test]
    fn accept_with_raw_token_grants_membership() {
        let repo = repo();
        let svc = InviteService::new(&repo);
        let issued = svc.issue(tenant(), "a@b.com", Role::Admin, 0).unwrap();
        let user = UserId::parse("u1").unwrap();

        let outcome = svc.accept(&issued.raw_token, &user, 10).unwrap();
        assert_eq!(outcome, AcceptOutcome::Accept(Role::Admin));
        assert_eq!(
            repo.get_membership(&tenant(), &user).unwrap().unwrap().role,
            Role::Admin
        );
    }

    #[test]
    fn accept_with_wrong_token_is_a_clean_reject() {
        let repo = repo();
        let svc = InviteService::new(&repo);
        svc.issue(tenant(), "a@b.com", Role::Member, 0).unwrap();
        let user = UserId::parse("u1").unwrap();
        // A token that hashes to nothing stored: not pending → refused.
        assert_eq!(
            svc.accept("not-the-real-token", &user, 10).unwrap(),
            AcceptOutcome::Reject(AcceptReject::NotPending)
        );
    }

    #[test]
    fn revoked_invite_cannot_be_accepted() {
        let repo = repo();
        let svc = InviteService::new(&repo).with_ttl(3_600);
        let issued = svc.issue(tenant(), "a@b.com", Role::Member, 0).unwrap();
        assert!(svc.revoke(&issued.raw_token).unwrap());
        let user = UserId::parse("u1").unwrap();
        assert_eq!(
            svc.accept(&issued.raw_token, &user, 10).unwrap(),
            AcceptOutcome::Reject(AcceptReject::NotPending)
        );
    }

    #[test]
    fn revoke_by_hash_matches_revoke_by_token() {
        // Admins revoke by the stored token_hash (the raw token is shown once).
        let repo = repo();
        let svc = InviteService::new(&repo).with_ttl(3_600);
        let issued = svc.issue(tenant(), "a@b.com", Role::Member, 0).unwrap();
        assert!(svc.revoke_by_hash(&issued.invite.token_hash).unwrap());
        let user = UserId::parse("u1").unwrap();
        assert_eq!(
            svc.accept(&issued.raw_token, &user, 10).unwrap(),
            AcceptOutcome::Reject(AcceptReject::NotPending)
        );
        // Revoking an unknown hash changes nothing.
        assert!(!svc.revoke_by_hash("deadbeef").unwrap());
    }
}
