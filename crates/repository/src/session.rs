//! Session persistence — the revocable half of auth (PRD §12.2 item 3).
//!
//! Refresh tokens are matched only by their [`RefreshTokenHash`]; the raw token
//! is never stored. Revocation is server-side state, which is what lets a logout
//! or a security event cut access before the short access-token TTL elapses.
//! Rotation on refresh is `revoke_session` (the presented one) + `create_session`
//! (the new one) — one-time-use tokens, so a replayed old token finds a revoked
//! session and is refused by the domain's `evaluate_refresh`.

use crate::SqliteRepository;
use anyhow::Result;
use domain::{RefreshTokenHash, Session, SessionId, UserId};
use rusqlite::params;

/// Storage boundary for sessions.
pub trait SessionRepository {
    fn create_session(&self, session: &Session) -> Result<()>;
    /// Match a presented refresh token (by hash) to its session, if any — the
    /// input to the domain's `evaluate_refresh` policy.
    fn find_session_by_refresh(&self, hash: &RefreshTokenHash) -> Result<Option<Session>>;
    /// Revoke a single session (logout / rotation). Returns whether a row
    /// changed (`false` if the id was unknown or already revoked).
    fn revoke_session(&self, id: &SessionId) -> Result<bool>;
    /// Revoke every active session for a user (password change, "log out
    /// everywhere", account compromise). Returns the number revoked.
    fn revoke_all_for_user(&self, user: &UserId) -> Result<u64>;
    /// Drop sessions that expired on or before `now` (housekeeping). Returns the
    /// number deleted. Revoked-but-unexpired rows are kept so a replay still
    /// matches a revoked session rather than vanishing into `Unknown`.
    fn prune_expired(&self, now: i64) -> Result<u64>;
}

impl SessionRepository for SqliteRepository {
    fn create_session(&self, session: &Session) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sessions (id, user_id, refresh_hash, issued_at, expires_at, revoked)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                session.id.as_str(),
                session.user_id.as_str(),
                session.refresh_hash.as_str(),
                session.issued_at,
                session.expires_at,
                session.revoked,
            ],
        )?;
        Ok(())
    }

    fn find_session_by_refresh(&self, hash: &RefreshTokenHash) -> Result<Option<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, user_id, refresh_hash, issued_at, expires_at, revoked
             FROM sessions WHERE refresh_hash = ?1",
        )?;
        let mut rows = stmt.query(params![hash.as_str()])?;
        match rows.next()? {
            Some(row) => Ok(Some(Session {
                id: SessionId::parse(row.get::<_, String>(0)?).map_err(anyhow::Error::new)?,
                user_id: UserId::parse(row.get::<_, String>(1)?).map_err(anyhow::Error::new)?,
                refresh_hash: RefreshTokenHash::parse(row.get::<_, String>(2)?)
                    .map_err(anyhow::Error::new)?,
                issued_at: row.get(3)?,
                expires_at: row.get(4)?,
                revoked: row.get(5)?,
            })),
            None => Ok(None),
        }
    }

    fn revoke_session(&self, id: &SessionId) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE sessions SET revoked = 1 WHERE id = ?1 AND revoked = 0",
            params![id.as_str()],
        )?;
        Ok(changed > 0)
    }

    fn revoke_all_for_user(&self, user: &UserId) -> Result<u64> {
        let changed = self.conn.execute(
            "UPDATE sessions SET revoked = 1 WHERE user_id = ?1 AND revoked = 0",
            params![user.as_str()],
        )?;
        Ok(changed as u64)
    }

    fn prune_expired(&self, now: i64) -> Result<u64> {
        let deleted = self
            .conn
            .execute("DELETE FROM sessions WHERE expires_at <= ?1", params![now])?;
        Ok(deleted as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{evaluate_refresh, RefreshOutcome, RefreshReject};

    fn repo() -> SqliteRepository {
        SqliteRepository::in_memory().unwrap()
    }

    fn session(id: &str, user: &str, hash: &str, issued: i64) -> Session {
        Session::open(
            SessionId::parse(id).unwrap(),
            UserId::parse(user).unwrap(),
            RefreshTokenHash::parse(hash).unwrap(),
            issued,
            Session::DEFAULT_TTL_SECS,
        )
    }

    #[test]
    fn create_then_find_by_refresh_roundtrips() {
        let repo = repo();
        let s = session("sess-1", "user-1", "hash-1", 1_700_000_000);
        repo.create_session(&s).unwrap();
        let got = repo
            .find_session_by_refresh(&RefreshTokenHash::parse("hash-1").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(got, s);
    }

    #[test]
    fn unknown_refresh_hash_resolves_to_none() {
        let repo = repo();
        assert!(repo
            .find_session_by_refresh(&RefreshTokenHash::parse("nope").unwrap())
            .unwrap()
            .is_none());
    }

    #[test]
    fn revoking_makes_refresh_report_revoked() {
        // End-to-end: the stored revocation flag drives the domain policy.
        let repo = repo();
        repo.create_session(&session("sess-1", "user-1", "hash-1", 1_000))
            .unwrap();
        assert!(repo
            .revoke_session(&SessionId::parse("sess-1").unwrap())
            .unwrap());

        let s = repo
            .find_session_by_refresh(&RefreshTokenHash::parse("hash-1").unwrap())
            .unwrap();
        assert_eq!(
            evaluate_refresh(s.as_ref(), 1_001),
            RefreshOutcome::Reject(RefreshReject::Revoked)
        );
    }

    #[test]
    fn revoking_unknown_or_already_revoked_returns_false() {
        let repo = repo();
        // Unknown id.
        assert!(!repo
            .revoke_session(&SessionId::parse("ghost").unwrap())
            .unwrap());
        // Already revoked.
        repo.create_session(&session("sess-1", "user-1", "hash-1", 1_000))
            .unwrap();
        let id = SessionId::parse("sess-1").unwrap();
        assert!(repo.revoke_session(&id).unwrap());
        assert!(!repo.revoke_session(&id).unwrap());
    }

    #[test]
    fn rotation_revokes_old_and_replays_fail() {
        // Refresh = revoke presented session + create a new one. The old token's
        // hash now matches a revoked session, so a replay is refused.
        let repo = repo();
        repo.create_session(&session("sess-1", "user-1", "old-hash", 1_000))
            .unwrap();
        // ... client refreshes: rotate.
        repo.revoke_session(&SessionId::parse("sess-1").unwrap())
            .unwrap();
        repo.create_session(&session("sess-2", "user-1", "new-hash", 1_050))
            .unwrap();

        let replayed = repo
            .find_session_by_refresh(&RefreshTokenHash::parse("old-hash").unwrap())
            .unwrap();
        assert_eq!(
            evaluate_refresh(replayed.as_ref(), 1_051),
            RefreshOutcome::Reject(RefreshReject::Revoked)
        );
        let current = repo
            .find_session_by_refresh(&RefreshTokenHash::parse("new-hash").unwrap())
            .unwrap();
        assert!(matches!(
            evaluate_refresh(current.as_ref(), 1_051),
            RefreshOutcome::Rotate { .. }
        ));
    }

    #[test]
    fn revoke_all_for_user_cuts_every_session() {
        let repo = repo();
        repo.create_session(&session("a", "user-1", "ha", 1_000))
            .unwrap();
        repo.create_session(&session("b", "user-1", "hb", 1_000))
            .unwrap();
        repo.create_session(&session("c", "other", "hc", 1_000))
            .unwrap();

        assert_eq!(
            repo.revoke_all_for_user(&UserId::parse("user-1").unwrap())
                .unwrap(),
            2
        );
        // The other user's session is untouched.
        let other = repo
            .find_session_by_refresh(&RefreshTokenHash::parse("hc").unwrap())
            .unwrap()
            .unwrap();
        assert!(!other.revoked);
    }

    #[test]
    fn prune_drops_expired_keeps_revoked_unexpired() {
        let repo = repo();
        // Expired (issued long ago, default 30d TTL).
        repo.create_session(&session("old", "user-1", "h-old", 0))
            .unwrap();
        // Current but revoked — must be kept so replays still see Revoked.
        repo.create_session(&session("cur", "user-1", "h-cur", 1_000_000))
            .unwrap();
        repo.revoke_session(&SessionId::parse("cur").unwrap())
            .unwrap();

        let now = Session::DEFAULT_TTL_SECS + 1; // past "old"'s expiry, before "cur"'s
        assert_eq!(repo.prune_expired(now).unwrap(), 1);
        assert!(repo
            .find_session_by_refresh(&RefreshTokenHash::parse("h-old").unwrap())
            .unwrap()
            .is_none());
        assert!(repo
            .find_session_by_refresh(&RefreshTokenHash::parse("h-cur").unwrap())
            .unwrap()
            .is_some());
    }
}
