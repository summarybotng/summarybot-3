//! Identity-link persistence + audit ledger (PRD §12.2 part 2).
//!
//! The `(provider, subject)` primary key makes WSP-010 enforceable at storage:
//! binding an identity already bound to another user fails with
//! [`LinkError::AlreadyBound`] rather than overwriting. The domain's pure
//! `resolve_link` decides intent *before* this is reached; the unique key is
//! defense in depth against a racing or forgetful caller.
//!
//! Every link (and, later, every claim/transfer) is appended to `audit_log`
//! (WSP-014: identity changes are a security boundary).

use crate::SqliteRepository;
use anyhow::Result;
use domain::{IdentityLink, ProviderKind, Subject, UserId};
use rusqlite::{params, ErrorCode};

/// Outcome of binding an identity to a user.
#[derive(Debug)]
pub enum LinkError {
    /// `(provider, subject)` is already bound (possibly to a different user).
    /// Callers must route this through the claim/transfer workflow (WSP-011),
    /// never overwrite.
    AlreadyBound {
        provider: &'static str,
        subject: String,
    },
    /// Any other storage failure.
    Db(anyhow::Error),
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkError::AlreadyBound { provider, subject } => {
                write!(f, "{provider} identity {subject} is already bound")
            }
            LinkError::Db(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for LinkError {}

impl From<anyhow::Error> for LinkError {
    fn from(e: anyhow::Error) -> Self {
        LinkError::Db(e)
    }
}

/// One row of the append-only audit ledger (WSP-014).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    /// Unix seconds; supplied by the host (the repository holds no clock).
    pub ts: i64,
    /// Acting user, or `None` for system/anonymous actions.
    pub actor: Option<UserId>,
    /// Stable action verb, e.g. `"identity.link"`, `"identity.claim"`.
    pub action: String,
    /// Human-readable context for the action.
    pub detail: String,
}

/// Storage boundary for identity links and the audit ledger.
pub trait IdentityRepository {
    /// Which user, if any, the verified identity is bound to — the input to the
    /// domain's `resolve_link` policy.
    fn find_user(&self, provider: ProviderKind, subject: &Subject) -> Result<Option<UserId>>;
    /// Bind an identity to a user. Fails with [`LinkError::AlreadyBound`] if the
    /// `(provider, subject)` pair is already taken (WSP-010).
    fn link_identity(&self, link: &IdentityLink, linked_at: i64) -> Result<(), LinkError>;
    /// Append an audit entry; returns its row id.
    fn append_audit(&self, entry: &AuditEntry) -> Result<i64>;
    /// Audit entries in insertion order (oldest first).
    fn list_audit(&self) -> Result<Vec<AuditEntry>>;
}

impl IdentityRepository for SqliteRepository {
    fn find_user(&self, provider: ProviderKind, subject: &Subject) -> Result<Option<UserId>> {
        let mut stmt = self
            .conn
            .prepare("SELECT user_id FROM identity_links WHERE provider = ?1 AND subject = ?2")?;
        let mut rows = stmt.query(params![provider.as_str(), subject.as_str()])?;
        match rows.next()? {
            Some(row) => Ok(Some(
                UserId::parse(row.get::<_, String>(0)?).map_err(anyhow::Error::new)?,
            )),
            None => Ok(None),
        }
    }

    fn link_identity(&self, link: &IdentityLink, linked_at: i64) -> Result<(), LinkError> {
        let result = self.conn.execute(
            "INSERT INTO identity_links (provider, subject, user_id, linked_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                link.provider.as_str(),
                link.subject.as_str(),
                link.user_id.as_str(),
                linked_at,
            ],
        );
        match result {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == ErrorCode::ConstraintViolation =>
            {
                Err(LinkError::AlreadyBound {
                    provider: link.provider.as_str(),
                    subject: link.subject.as_str().to_string(),
                })
            }
            Err(e) => Err(LinkError::Db(anyhow::Error::new(e))),
        }
    }

    fn append_audit(&self, entry: &AuditEntry) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO audit_log (ts, actor, action, detail) VALUES (?1, ?2, ?3, ?4)",
            params![
                entry.ts,
                entry.actor.as_ref().map(|u| u.as_str()),
                entry.action,
                entry.detail,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    fn list_audit(&self) -> Result<Vec<AuditEntry>> {
        let mut stmt = self
            .conn
            .prepare("SELECT ts, actor, action, detail FROM audit_log ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            let actor: Option<String> = row.get(1)?;
            Ok((
                row.get::<_, i64>(0)?,
                actor,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (ts, actor, action, detail) = row?;
            let actor = match actor {
                Some(a) => Some(UserId::parse(a).map_err(anyhow::Error::new)?),
                None => None,
            };
            out.push(AuditEntry {
                ts,
                actor,
                action,
                detail,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{IdentityProvider, LinkIntent, LinkOutcome, ProviderClaims};

    fn repo() -> SqliteRepository {
        SqliteRepository::in_memory().unwrap()
    }

    fn verified(
        provider: &dyn IdentityProvider,
        subject: &str,
        email: Option<&str>,
    ) -> domain::VerifiedIdentity {
        provider
            .normalize(&ProviderClaims {
                subject: subject.to_string(),
                email: email.map(str::to_string),
            })
            .unwrap()
    }

    #[test]
    fn unbound_identity_resolves_to_none() {
        let repo = repo();
        let id = verified(&domain::DiscordProvider, "42", None);
        assert!(repo.find_user(id.provider, &id.subject).unwrap().is_none());
    }

    #[test]
    fn link_then_find_roundtrips() {
        let repo = repo();
        let id = verified(&domain::DiscordProvider, "42", None);
        let user = UserId::parse("user-uuid-1").unwrap();
        repo.link_identity(&IdentityLink::new(&id, user.clone()), 1_700_000_000)
            .unwrap();
        assert_eq!(
            repo.find_user(id.provider, &id.subject).unwrap(),
            Some(user)
        );
    }

    #[test]
    fn binding_an_already_bound_identity_to_another_user_is_rejected() {
        // The storage half of WSP-010: even if a caller skipped resolve_link,
        // the unique key refuses the silent reassign.
        let repo = repo();
        let id = verified(&domain::GoogleProvider, "sub-1", Some("a@b.com"));
        repo.link_identity(&IdentityLink::new(&id, UserId::parse("owner").unwrap()), 1)
            .unwrap();
        let outcome = repo.link_identity(
            &IdentityLink::new(&id, UserId::parse("intruder").unwrap()),
            2,
        );
        assert!(matches!(outcome, Err(LinkError::AlreadyBound { .. })));
        // The original binding is untouched.
        assert_eq!(
            repo.find_user(id.provider, &id.subject).unwrap(),
            Some(UserId::parse("owner").unwrap())
        );
    }

    #[test]
    fn end_to_end_login_provisions_then_signs_in() {
        // Walking the real flow against storage: first login provisions, second
        // login of the same identity signs the same user back in.
        let repo = repo();
        let id = verified(&domain::EmailProvider, "ignored", Some("Me@Example.com"));

        let existing = repo.find_user(id.provider, &id.subject).unwrap();
        assert_eq!(
            domain::resolve_link(&LinkIntent::Login, existing.as_ref()),
            LinkOutcome::Provision
        );
        let user = UserId::parse("provisioned-1").unwrap();
        repo.link_identity(&IdentityLink::new(&id, user.clone()), 10)
            .unwrap();

        let existing = repo.find_user(id.provider, &id.subject).unwrap();
        assert_eq!(
            domain::resolve_link(&LinkIntent::Login, existing.as_ref()),
            LinkOutcome::SignIn(user)
        );
    }

    #[test]
    fn audit_entries_are_appended_in_order() {
        let repo = repo();
        repo.append_audit(&AuditEntry {
            ts: 1,
            actor: None,
            action: "identity.link".to_string(),
            detail: "discord:42 -> user-1".to_string(),
        })
        .unwrap();
        repo.append_audit(&AuditEntry {
            ts: 2,
            actor: Some(UserId::parse("admin").unwrap()),
            action: "identity.claim".to_string(),
            detail: "approved".to_string(),
        })
        .unwrap();
        let log = repo.list_audit().unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].action, "identity.link");
        assert_eq!(log[0].actor, None);
        assert_eq!(log[1].actor, Some(UserId::parse("admin").unwrap()));
    }
}
