//! Session & token lifecycle (PRD §12.2 item 3; security boundary).
//!
//! Two token kinds, the standard split:
//!   * a **short-lived access token** — carried as signed [`AccessClaims`]
//!     (`sub = user_uuid`, plus the workspaces it grants), validated by expiry;
//!   * a **long-lived refresh token** — opaque and random, backing a [`Session`]
//!     that can be **revoked** server-side and is **rotated** on every use.
//!
//! Pure (§12.0): the domain never sees raw secrets. A refresh token is stored
//! and matched only by its [`RefreshTokenHash`]; generating the random token,
//! hashing it, reading the clock, and signing the JWT are all the host's job.
//! [`evaluate_refresh`] is the single place the revoke/expire/rotate policy
//! lives, so it can be exhaustively tested and never bypassed.

use crate::{string_id, UserId, WorkspaceId};

string_id!(
    /// Server-issued opaque session identifier (also the JWT `sid` claim,
    /// linking an access token back to the revocable session that minted it).
    SessionId,
    "session id",
    256
);

string_id!(
    /// Hash of the opaque refresh token (e.g. SHA-256 hex). The raw token is
    /// returned to the client **once** and never stored: a database leak yields
    /// only hashes, not usable tokens. Lookups match on this hash.
    RefreshTokenHash,
    "refresh token hash",
    128
);

/// A refresh-token-backed session — the unit of revocation. Access tokens are
/// stateless and expire on their own; this is what lets a logout or a security
/// event cut access *before* the short access TTL elapses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: SessionId,
    pub user_id: UserId,
    pub refresh_hash: RefreshTokenHash,
    /// Unix seconds; supplied by the host (the domain holds no clock).
    pub issued_at: i64,
    pub expires_at: i64,
    pub revoked: bool,
}

impl Session {
    /// Default refresh-token lifetime: 30 days.
    pub const DEFAULT_TTL_SECS: i64 = 60 * 60 * 24 * 30;

    /// Open a session expiring `ttl_secs` after `issued_at`.
    pub fn open(
        id: SessionId,
        user_id: UserId,
        refresh_hash: RefreshTokenHash,
        issued_at: i64,
        ttl_secs: i64,
    ) -> Self {
        Self {
            id,
            user_id,
            refresh_hash,
            issued_at,
            expires_at: issued_at.saturating_add(ttl_secs),
            revoked: false,
        }
    }

    /// Usable for refresh: not revoked and not past expiry at `now`.
    pub fn is_active(&self, now: i64) -> bool {
        !self.revoked && now < self.expires_at
    }
}

/// The signed, short-lived access token's payload. `sub` is the application
/// `user_uuid` (never a platform id — WSP-002); `workspaces` are the workspace
/// ids this token authorizes, so a request can be checked without a DB hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessClaims {
    pub sub: UserId,
    /// The session that minted this token (revocation linkage).
    pub session_id: SessionId,
    pub workspaces: Vec<WorkspaceId>,
    pub issued_at: i64,
    pub expires_at: i64,
}

impl AccessClaims {
    /// Default access-token lifetime: 15 minutes ("short-lived").
    pub const DEFAULT_TTL_SECS: i64 = 60 * 15;

    /// Mint claims expiring `ttl_secs` after `issued_at`.
    pub fn issue(
        sub: UserId,
        session_id: SessionId,
        workspaces: Vec<WorkspaceId>,
        issued_at: i64,
        ttl_secs: i64,
    ) -> Self {
        Self {
            sub,
            session_id,
            workspaces,
            issued_at,
            expires_at: issued_at.saturating_add(ttl_secs),
        }
    }

    /// Still valid at `now` (purely by expiry — signature checking is the
    /// host's crypto step, done before these claims are trusted).
    pub fn is_valid_at(&self, now: i64) -> bool {
        now < self.expires_at
    }
}

/// Why a refresh attempt was refused. Distinguished so the host can respond
/// precisely (and audit-log the reason) — e.g. `Revoked` signals possible
/// token theft/replay, `Expired` is routine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshReject {
    /// No session matches the presented refresh-token hash.
    Unknown,
    /// The session was explicitly revoked (logout / security event).
    Revoked,
    /// The session passed its expiry.
    Expired,
}

/// The decision [`evaluate_refresh`] reaches for a presented refresh token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// Valid: issue a fresh access token and a **new** refresh token, revoking
    /// the presented session (one-time use — a replayed old token then fails).
    Rotate { user_id: UserId },
    /// Refused; carries the reason.
    Reject(RefreshReject),
}

/// Pure refresh policy. `session` is the row matched by the presented refresh
/// hash (or `None` if nothing matched). Revocation is checked *before* expiry
/// so a revoked-and-expired token still reports the stronger `Revoked` signal.
pub fn evaluate_refresh(session: Option<&Session>, now: i64) -> RefreshOutcome {
    match session {
        None => RefreshOutcome::Reject(RefreshReject::Unknown),
        Some(s) if s.revoked => RefreshOutcome::Reject(RefreshReject::Revoked),
        Some(s) if now >= s.expires_at => RefreshOutcome::Reject(RefreshReject::Expired),
        Some(s) => RefreshOutcome::Rotate {
            user_id: s.user_id.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(now: i64, ttl: i64) -> Session {
        Session::open(
            SessionId::parse("sess-1").unwrap(),
            UserId::parse("user-1").unwrap(),
            RefreshTokenHash::parse("deadbeef").unwrap(),
            now,
            ttl,
        )
    }

    #[test]
    fn fresh_session_is_active_until_expiry() {
        let s = session(1_000, 100);
        assert!(s.is_active(1_000));
        assert!(s.is_active(1_099));
        assert!(!s.is_active(1_100)); // expiry is exclusive
    }

    #[test]
    fn revoked_session_is_never_active() {
        let mut s = session(1_000, 100);
        s.revoked = true;
        assert!(!s.is_active(1_000));
    }

    #[test]
    fn access_claims_expire_after_ttl() {
        let claims = AccessClaims::issue(
            UserId::parse("u").unwrap(),
            SessionId::parse("s").unwrap(),
            vec![WorkspaceId::parse("ws-1").unwrap()],
            500,
            AccessClaims::DEFAULT_TTL_SECS,
        );
        assert_eq!(claims.expires_at, 500 + 900);
        assert!(claims.is_valid_at(500 + 899));
        assert!(!claims.is_valid_at(500 + 900));
    }

    #[test]
    fn refresh_unknown_token_is_rejected() {
        assert_eq!(
            evaluate_refresh(None, 0),
            RefreshOutcome::Reject(RefreshReject::Unknown)
        );
    }

    #[test]
    fn refresh_valid_session_rotates() {
        let s = session(1_000, 100);
        assert_eq!(
            evaluate_refresh(Some(&s), 1_050),
            RefreshOutcome::Rotate {
                user_id: UserId::parse("user-1").unwrap()
            }
        );
    }

    #[test]
    fn refresh_expired_session_is_rejected() {
        let s = session(1_000, 100);
        assert_eq!(
            evaluate_refresh(Some(&s), 1_100),
            RefreshOutcome::Reject(RefreshReject::Expired)
        );
    }

    #[test]
    fn revoked_takes_precedence_over_expired() {
        // A token that is both revoked and expired reports the stronger signal,
        // so theft/replay isn't masked as a routine expiry.
        let mut s = session(1_000, 100);
        s.revoked = true;
        assert_eq!(
            evaluate_refresh(Some(&s), 5_000),
            RefreshOutcome::Reject(RefreshReject::Revoked)
        );
    }
}
