//! Contested identity claims (WSP-011..014; ADR-119).
//!
//! When `resolve_link` returns a `Collision` (an identity already bound to a
//! different user), the claimant opens a **claim**. The route (self-service /
//! tenant admin / platform operator) is decided by the pure
//! [`domain::decide_claim_route`] policy and stored here; an approver later
//! resolves it, and on approval the host rebinds the identity to the claimant.

use crate::SqliteRepository;
use anyhow::Result;
use domain::{ClaimStatus, ProviderKind, Subject, UserId};
use rusqlite::{params, OptionalExtension};

/// A pending/resolved claim over a contested identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityClaim {
    pub id: String,
    pub provider: String,
    pub subject: String,
    /// The user asking to take over the identity.
    pub claimant: String,
    /// The user the identity is currently bound to.
    pub current_owner: String,
    /// Adjudication route ("self_service" | "tenant_admin" | "operator").
    pub route: String,
    pub status: ClaimStatus,
    pub created_at: i64,
}

/// Storage boundary for identity claims + the rebind on approval.
pub trait IdentityClaimRepository {
    /// Record a freshly-opened claim (status `Pending`).
    fn open_claim(&self, claim: &IdentityClaim) -> Result<()>;
    /// Fetch a claim by id.
    fn get_claim(&self, id: &str) -> Result<Option<IdentityClaim>>;
    /// All claims still `Pending`, oldest first.
    fn list_pending_claims(&self) -> Result<Vec<IdentityClaim>>;
    /// Force a claim's status (approve/deny). Returns whether a row changed.
    fn set_claim_status(&self, id: &str, status: ClaimStatus) -> Result<bool>;
    /// Re-point an identity link to a new owner (the approved transfer, WSP-013).
    fn rebind_identity(
        &self,
        provider: ProviderKind,
        subject: &Subject,
        new_owner: &UserId,
        now: i64,
    ) -> Result<bool>;
}

fn row_to_claim(row: &rusqlite::Row) -> rusqlite::Result<IdentityClaim> {
    let status: String = row.get(6)?;
    Ok(IdentityClaim {
        id: row.get(0)?,
        provider: row.get(1)?,
        subject: row.get(2)?,
        claimant: row.get(3)?,
        current_owner: row.get(4)?,
        route: row.get(5)?,
        status: ClaimStatus::parse(&status).unwrap_or(ClaimStatus::Pending),
        created_at: row.get(7)?,
    })
}

impl IdentityClaimRepository for SqliteRepository {
    fn open_claim(&self, claim: &IdentityClaim) -> Result<()> {
        self.conn.execute(
            "INSERT INTO identity_claims
                 (id, provider, subject, claimant, current_owner, route, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                claim.id,
                claim.provider,
                claim.subject,
                claim.claimant,
                claim.current_owner,
                claim.route,
                claim.status.as_str(),
                claim.created_at,
            ],
        )?;
        Ok(())
    }

    fn get_claim(&self, id: &str) -> Result<Option<IdentityClaim>> {
        self.conn
            .query_row(
                "SELECT id, provider, subject, claimant, current_owner, route, status, created_at
                 FROM identity_claims WHERE id = ?1",
                params![id],
                row_to_claim,
            )
            .optional()
            .map_err(Into::into)
    }

    fn list_pending_claims(&self) -> Result<Vec<IdentityClaim>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, provider, subject, claimant, current_owner, route, status, created_at
             FROM identity_claims WHERE status = 'pending' ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map([], row_to_claim)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    fn set_claim_status(&self, id: &str, status: ClaimStatus) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE identity_claims SET status = ?2 WHERE id = ?1",
            params![id, status.as_str()],
        )?;
        Ok(n > 0)
    }

    fn rebind_identity(
        &self,
        provider: ProviderKind,
        subject: &Subject,
        new_owner: &UserId,
        now: i64,
    ) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE identity_links SET user_id = ?3, linked_at = ?4
             WHERE provider = ?1 AND subject = ?2",
            params![provider.as_str(), subject.as_str(), new_owner.as_str(), now],
        )?;
        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(id: &str, claimant: &str, owner: &str) -> IdentityClaim {
        IdentityClaim {
            id: id.into(),
            provider: "discord".into(),
            subject: "alice#1".into(),
            claimant: claimant.into(),
            current_owner: owner.into(),
            route: "operator".into(),
            status: ClaimStatus::Pending,
            created_at: 100,
        }
    }

    #[test]
    fn open_list_resolve_round_trip() {
        let repo = SqliteRepository::in_memory().unwrap();
        repo.open_claim(&claim("c1", "u-new", "u-old")).unwrap();
        assert_eq!(repo.list_pending_claims().unwrap().len(), 1);
        assert_eq!(repo.get_claim("c1").unwrap().unwrap().claimant, "u-new");

        // Approving removes it from the pending set.
        assert!(repo.set_claim_status("c1", ClaimStatus::Approved).unwrap());
        assert!(repo.list_pending_claims().unwrap().is_empty());
        assert_eq!(repo.get_claim("c1").unwrap().unwrap().status, ClaimStatus::Approved);
    }

    #[test]
    fn rebind_repoints_an_identity_link() {
        use crate::IdentityRepository;
        let repo = SqliteRepository::in_memory().unwrap();
        let link = domain::IdentityLink {
            provider: ProviderKind::Discord,
            subject: Subject::parse("alice#1").unwrap(),
            user_id: UserId::parse("u-old").unwrap(),
        };
        repo.link_identity(&link, 1).unwrap();
        assert!(repo
            .rebind_identity(ProviderKind::Discord, &Subject::parse("alice#1").unwrap(), &UserId::parse("u-new").unwrap(), 2)
            .unwrap());
        assert_eq!(
            repo.find_user(ProviderKind::Discord, &Subject::parse("alice#1").unwrap()).unwrap().unwrap().as_str(),
            "u-new"
        );
    }
}
