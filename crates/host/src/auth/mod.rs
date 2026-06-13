//! Host-side auth I/O — the seam that closes Phase 1 item 3.
//!
//! The domain owns the *pure* policy (`resolve_link`, `evaluate_refresh`, the
//! claims/session models); this module owns the *I/O* the domain deliberately
//! avoids (§12.0): generating random tokens, hashing them, and signing/verifying
//! short-lived access tokens (see [`token`]). [`AuthService`] wires those to the
//! identity and session repositories into the three real flows: **login**
//! (sign-in or provision), **refresh** (rotate, replay-safe), and **link**
//! (attach another identity, WSP-010-safe).
//!
//! The HMAC signing key is held as a [`Secret`] so it cannot leak into logs.

mod token;
pub use token::new_correlation_id;
// Opaque-token primitives shared with the invite service (host owns token I/O).
pub(crate) use token::{generate_token, sha256_hex};
use token::{hash_token, new_session_id, new_user_id, sign_access, verify_access};

/// Verify an access token against the signing key, statelessly (no DB). Used by
/// the web layer's auth middleware (Phase 5), which has the key but shouldn't
/// take a repo lock just to validate a JWT.
pub fn verify_token(
    token: &str,
    key: &Secret<Vec<u8>>,
    now: i64,
) -> Result<AccessClaims, AuthError> {
    verify_access(token, key.expose_secret(), now)
}

use domain::{
    evaluate_refresh, resolve_link, AccessClaims, IdentityLink, IdentityProvider, LinkIntent,
    LinkOutcome, ProviderClaims, RefreshOutcome, RefreshReject, Secret, Session, SessionId, UserId,
    WorkspaceId,
};
use repository::{
    AuditEntry, IdentityRepository, MembershipRepository, SessionRepository, WorkspaceRepository,
};

/// Failure modes of the auth flows, kept distinct so the caller (web layer) can
/// map each to the right response and audit signal.
#[derive(Debug)]
pub enum AuthError {
    /// Provider claims failed normalization at the boundary.
    InvalidClaims(String),
    /// A presented refresh token was refused (unknown / revoked / expired).
    Refresh(RefreshReject),
    /// A presented access token was malformed, mis-signed, or expired.
    InvalidToken(&'static str),
    /// Linking an identity already bound to a *different* user (WSP-010).
    Collision { bound_to: UserId },
    /// Crypto/entropy failure.
    Crypto(String),
    /// Storage failure.
    Db(anyhow::Error),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::InvalidClaims(e) => write!(f, "invalid identity claims: {e}"),
            AuthError::Refresh(r) => write!(f, "refresh refused: {r:?}"),
            AuthError::InvalidToken(why) => write!(f, "invalid access token: {why}"),
            AuthError::Collision { bound_to } => {
                write!(f, "identity already bound to user {}", bound_to.as_str())
            }
            AuthError::Crypto(e) => write!(f, "crypto failure: {e}"),
            AuthError::Db(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for AuthError {}

impl From<anyhow::Error> for AuthError {
    fn from(e: anyhow::Error) -> Self {
        AuthError::Db(e)
    }
}

/// What a successful login/refresh hands back. The `refresh_token` is the raw
/// opaque secret — shown to the client **once**; only its hash is stored.
#[derive(Debug, Clone)]
pub struct TokenPair {
    pub access_token: String,
    pub refresh_token: String,
    pub user_id: UserId,
    pub session_id: SessionId,
    pub access_expires_at: i64,
}

/// Ties the pure domain policy to real token I/O + storage. Generic over a
/// backend implementing both repositories (the SQLite repo does).
pub struct AuthService<'a, R> {
    repo: &'a R,
    signing_key: &'a Secret<Vec<u8>>,
    access_ttl: i64,
    refresh_ttl: i64,
}

impl<'a, R> AuthService<'a, R>
where
    R: IdentityRepository + SessionRepository + WorkspaceRepository + MembershipRepository,
{
    /// Build with the default token lifetimes (15-min access, 30-day refresh).
    pub fn new(repo: &'a R, signing_key: &'a Secret<Vec<u8>>) -> Self {
        Self {
            repo,
            signing_key,
            access_ttl: AccessClaims::DEFAULT_TTL_SECS,
            refresh_ttl: Session::DEFAULT_TTL_SECS,
        }
    }

    /// Override the access/refresh token lifetimes (seconds).
    pub fn with_ttls(mut self, access_ttl: i64, refresh_ttl: i64) -> Self {
        self.access_ttl = access_ttl;
        self.refresh_ttl = refresh_ttl;
        self
    }

    /// Sign in via a provider. Normalizes the verified claims, then applies the
    /// pure `resolve_link(Login)` policy: an existing binding signs that user in;
    /// no binding provisions a fresh user and binds it. Either way a session is
    /// opened and a token pair issued. `workspaces` are the ids this access token
    /// should grant (resolved by the caller).
    pub fn login(
        &self,
        provider: &dyn IdentityProvider,
        claims: &ProviderClaims,
        workspaces: Vec<WorkspaceId>,
        now: i64,
    ) -> Result<TokenPair, AuthError> {
        let identity = provider
            .normalize(claims)
            .map_err(|e| AuthError::InvalidClaims(e.to_string()))?;
        let existing = self.repo.find_user(identity.provider, &identity.subject)?;

        let user = match resolve_link(&LinkIntent::Login, existing.as_ref()) {
            LinkOutcome::SignIn(user) => user,
            LinkOutcome::Provision => {
                let user = new_user_id()?;
                self.repo
                    .link_identity(&IdentityLink::new(&identity, user.clone()), now)
                    .map_err(|e| AuthError::Db(anyhow::Error::new(e)))?;
                self.audit(now, None, "identity.provision", identity.provider.as_str());
                user
            }
            // Login intent yields only SignIn/Provision (see resolve_link).
            other => unreachable!("login produced {other:?}"),
        };
        self.issue_pair(user, workspaces, now)
    }

    /// Exchange a refresh token for a new pair. Looks the session up by hash,
    /// applies the pure `evaluate_refresh` policy, and on success **rotates**:
    /// the presented session is revoked and a brand-new one issued, so a replayed
    /// old token now matches a revoked session and is refused.
    pub fn refresh(
        &self,
        refresh_token: &str,
        workspaces: Vec<WorkspaceId>,
        now: i64,
    ) -> Result<TokenPair, AuthError> {
        let hash = hash_token(refresh_token)?;
        let session = self.repo.find_session_by_refresh(&hash)?;
        match evaluate_refresh(session.as_ref(), now) {
            RefreshOutcome::Rotate { user_id } => {
                // session is Some on the Rotate path.
                let old = session.expect("rotate implies a matched session");
                self.repo.revoke_session(&old.id)?;
                self.issue_pair(user_id, workspaces, now)
            }
            RefreshOutcome::Reject(reason) => Err(AuthError::Refresh(reason)),
        }
    }

    /// Attach another verified identity to an already-authenticated user
    /// (WSP-005 unified identity). Enforces WSP-010 via the pure policy: binding
    /// an identity owned by a *different* user is a [`AuthError::Collision`],
    /// never a silent merge; re-linking one's own identity is idempotent.
    pub fn link(
        &self,
        acting_user: &UserId,
        provider: &dyn IdentityProvider,
        claims: &ProviderClaims,
        now: i64,
    ) -> Result<(), AuthError> {
        let identity = provider
            .normalize(claims)
            .map_err(|e| AuthError::InvalidClaims(e.to_string()))?;
        let existing = self.repo.find_user(identity.provider, &identity.subject)?;
        match resolve_link(
            &LinkIntent::Link {
                acting_user: acting_user.clone(),
            },
            existing.as_ref(),
        ) {
            LinkOutcome::Link(user) => {
                self.repo
                    .link_identity(&IdentityLink::new(&identity, user), now)
                    .map_err(|e| AuthError::Db(anyhow::Error::new(e)))?;
                self.audit(
                    now,
                    Some(acting_user),
                    "identity.link",
                    identity.provider.as_str(),
                );
                Ok(())
            }
            LinkOutcome::AlreadyLinked(_) => Ok(()),
            LinkOutcome::Collision { bound_to, .. } => {
                self.audit(
                    now,
                    Some(acting_user),
                    "identity.link.collision",
                    identity.provider.as_str(),
                );
                Err(AuthError::Collision { bound_to })
            }
            other => unreachable!("link produced {other:?}"),
        }
    }

    /// Verify a presented access token: checks the HS256 signature and expiry,
    /// returning the validated [`AccessClaims`]. Stateless — no DB hit (that is
    /// the point of the short-lived access token).
    pub fn verify_access(&self, token: &str, now: i64) -> Result<AccessClaims, AuthError> {
        verify_access(token, self.signing_key.expose_secret(), now)
    }

    /// Filter the *requested* workspaces down to the ones `user` is actually
    /// entitled to (WSP — the access token must never out-grant entitlement).
    ///
    /// A user is entitled to a workspace if they own it or are a member of its
    /// tenant. A requested workspace with **no stored row** is treated as
    /// *unclaimed* and granted: real tenant workspaces always carry a tenant +
    /// owner, so this protects them while leaving dev/unclaimed workspaces (and
    /// the dev sign-in flow) usable. Each dropped grant is audit-logged.
    fn entitle(&self, user: &UserId, requested: Vec<WorkspaceId>, now: i64) -> Vec<WorkspaceId> {
        requested
            .into_iter()
            .filter(|ws| match self.repo.find_workspace(ws) {
                // Unclaimed / does not exist → open (dev + first-run UX).
                Ok(None) => true,
                Ok(Some(w)) => {
                    let entitled = &w.owner_user_id == user
                        || matches!(self.repo.get_membership(&w.tenant_id, user), Ok(Some(_)));
                    if !entitled {
                        self.audit(now, Some(user), "auth.workspace.denied", ws.as_str());
                    }
                    entitled
                }
                // On a storage error, fail closed (do not grant).
                Err(_) => false,
            })
            .collect()
    }

    /// Open a session + mint a token pair for an established user. The requested
    /// `workspaces` are filtered to the user's entitlement before being signed in.
    fn issue_pair(
        &self,
        user: UserId,
        workspaces: Vec<WorkspaceId>,
        now: i64,
    ) -> Result<TokenPair, AuthError> {
        let workspaces = self.entitle(&user, workspaces, now);
        let raw = generate_token()?;
        let hash = hash_token(&raw)?;
        let session_id = new_session_id()?;
        let session = Session::open(
            session_id.clone(),
            user.clone(),
            hash,
            now,
            self.refresh_ttl,
        );
        self.repo.create_session(&session)?;

        let claims = AccessClaims::issue(
            user.clone(),
            session_id.clone(),
            workspaces,
            now,
            self.access_ttl,
        );
        let access_token = sign_access(&claims, self.signing_key.expose_secret())?;

        Ok(TokenPair {
            access_token,
            refresh_token: raw,
            user_id: user,
            session_id,
            access_expires_at: claims.expires_at,
        })
    }

    fn audit(&self, now: i64, actor: Option<&UserId>, action: &str, detail: &str) {
        // Audit is best-effort: a logging failure must not abort a successful
        // auth. (Surfacing audit-write failures is a Phase 9 hardening concern.)
        let _ = self.repo.append_audit(&AuditEntry {
            ts: now,
            actor: actor.cloned(),
            action: action.to_string(),
            detail: detail.to_string(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{DiscordProvider, EmailProvider, GoogleProvider, Membership, Role, TenantId};
    use repository::SqliteRepository;

    fn key() -> Secret<Vec<u8>> {
        Secret::new(b"test-signing-key-not-for-production".to_vec())
    }

    fn claims(subject: &str, email: Option<&str>) -> ProviderClaims {
        ProviderClaims {
            subject: subject.to_string(),
            email: email.map(str::to_string),
        }
    }

    #[test]
    fn login_provisions_then_subsequent_login_signs_in_same_user() {
        let repo = SqliteRepository::in_memory().unwrap();
        let key = key();
        let svc = AuthService::new(&repo, &key);

        let first = svc
            .login(&DiscordProvider, &claims("123", None), vec![], 1_000)
            .unwrap();
        let second = svc
            .login(&DiscordProvider, &claims("123", None), vec![], 2_000)
            .unwrap();
        // Same identity → same provisioned user, but distinct sessions.
        assert_eq!(first.user_id, second.user_id);
        assert_ne!(first.session_id, second.session_id);
        assert_ne!(first.refresh_token, second.refresh_token);
    }

    #[test]
    fn login_grants_only_entitled_or_unclaimed_workspaces() {
        let repo = SqliteRepository::in_memory().unwrap();
        let key = key();
        let tenant = TenantId::parse("acme").unwrap();
        let other = UserId::parse("u-other").unwrap();
        // A real workspace under a tenant, owned by someone else.
        repo.create_workspace(
            &domain::Workspace::create(
                WorkspaceId::parse("ws-eng").unwrap(),
                tenant.clone(),
                "Eng",
                other.clone(),
                10,
            )
            .unwrap(),
        )
        .unwrap();

        let svc = AuthService::new(&repo, &key);
        let req = || {
            vec![
                WorkspaceId::parse("ws-eng").unwrap(),
                WorkspaceId::parse("ws-demo").unwrap(), // unclaimed (no row)
            ]
        };

        // A brand-new user: ws-eng is a real workspace they're not in → dropped;
        // ws-demo is unclaimed → granted (dev/first-run UX preserved).
        let pair = svc
            .login(&DiscordProvider, &claims("u", None), req(), 1_000)
            .unwrap();
        let granted = svc.verify_access(&pair.access_token, 1_010).unwrap().workspaces;
        assert_eq!(granted, vec![WorkspaceId::parse("ws-demo").unwrap()]);

        // Grant the user membership in the tenant → ws-eng is now entitled.
        repo.upsert_membership(&Membership::new(
            tenant.clone(),
            pair.user_id.clone(),
            Role::Member,
        ))
        .unwrap();
        let pair2 = svc
            .login(&DiscordProvider, &claims("u", None), req(), 2_000)
            .unwrap();
        let granted2 = svc.verify_access(&pair2.access_token, 2_010).unwrap().workspaces;
        assert_eq!(
            granted2,
            vec![
                WorkspaceId::parse("ws-eng").unwrap(),
                WorkspaceId::parse("ws-demo").unwrap()
            ]
        );
    }

    #[test]
    fn workspace_owner_is_entitled_without_a_membership_row() {
        let repo = SqliteRepository::in_memory().unwrap();
        let key = key();
        let svc = AuthService::new(&repo, &key);
        // Provision the user first so we know their id, then create a workspace
        // they own (no membership row).
        let me = svc
            .login(&DiscordProvider, &claims("owner", None), vec![], 0)
            .unwrap()
            .user_id;
        repo.create_workspace(
            &domain::Workspace::create(
                WorkspaceId::parse("ws-mine").unwrap(),
                TenantId::parse("acme").unwrap(),
                "Mine",
                me.clone(),
                10,
            )
            .unwrap(),
        )
        .unwrap();
        let pair = svc
            .login(
                &DiscordProvider,
                &claims("owner", None),
                vec![WorkspaceId::parse("ws-mine").unwrap()],
                20,
            )
            .unwrap();
        let granted = svc.verify_access(&pair.access_token, 30).unwrap().workspaces;
        assert_eq!(granted, vec![WorkspaceId::parse("ws-mine").unwrap()]);
    }

    #[test]
    fn issued_access_token_verifies_and_carries_claims() {
        let repo = SqliteRepository::in_memory().unwrap();
        let key = key();
        let svc = AuthService::new(&repo, &key);
        let ws = vec![WorkspaceId::parse("ws-1").unwrap()];

        let pair = svc
            .login(
                &EmailProvider,
                &claims("x", Some("Me@Ex.com")),
                ws.clone(),
                5_000,
            )
            .unwrap();
        let verified = svc.verify_access(&pair.access_token, 5_100).unwrap();
        assert_eq!(verified.sub, pair.user_id);
        assert_eq!(verified.session_id, pair.session_id);
        assert_eq!(verified.workspaces, ws);
    }

    #[test]
    fn refresh_rotates_and_replayed_old_token_is_refused() {
        let repo = SqliteRepository::in_memory().unwrap();
        let key = key();
        let svc = AuthService::new(&repo, &key);

        let first = svc
            .login(&DiscordProvider, &claims("1", None), vec![], 1_000)
            .unwrap();
        let rotated = svc.refresh(&first.refresh_token, vec![], 1_050).unwrap();
        assert_eq!(first.user_id, rotated.user_id);
        assert_ne!(first.session_id, rotated.session_id);

        // The original refresh token now maps to a revoked session.
        assert!(matches!(
            svc.refresh(&first.refresh_token, vec![], 1_060),
            Err(AuthError::Refresh(RefreshReject::Revoked))
        ));
        // The rotated token still works.
        assert!(svc.refresh(&rotated.refresh_token, vec![], 1_070).is_ok());
    }

    #[test]
    fn refresh_with_unknown_token_is_rejected() {
        let repo = SqliteRepository::in_memory().unwrap();
        let key = key();
        let svc = AuthService::new(&repo, &key);
        assert!(matches!(
            svc.refresh("never-issued", vec![], 0),
            Err(AuthError::Refresh(RefreshReject::Unknown))
        ));
    }

    #[test]
    fn linking_an_identity_owned_by_another_user_collides() {
        let repo = SqliteRepository::in_memory().unwrap();
        let key = key();
        let svc = AuthService::new(&repo, &key);

        // User A provisions via Discord.
        let a = svc
            .login(&DiscordProvider, &claims("discord-A", None), vec![], 0)
            .unwrap();
        // User B provisions via email.
        let b = svc
            .login(&EmailProvider, &claims("x", Some("b@ex.com")), vec![], 0)
            .unwrap();

        // B tries to link A's Discord identity → collision (WSP-010), no merge.
        match svc.link(&b.user_id, &DiscordProvider, &claims("discord-A", None), 10) {
            Err(AuthError::Collision { bound_to }) => assert_eq!(bound_to, a.user_id),
            other => panic!("expected collision, got {other:?}"),
        }
    }

    #[test]
    fn linking_a_new_identity_to_self_then_logging_in_with_it_returns_same_user() {
        let repo = SqliteRepository::in_memory().unwrap();
        let key = key();
        let svc = AuthService::new(&repo, &key);

        let a = svc
            .login(&DiscordProvider, &claims("d-1", None), vec![], 0)
            .unwrap();
        // Link a Google identity to the same user.
        svc.link(
            &a.user_id,
            &GoogleProvider,
            &claims("g-1", Some("a@ex.com")),
            10,
        )
        .unwrap();
        // Logging in via Google now resolves to the same user (WSP-005).
        let via_google = svc
            .login(
                &GoogleProvider,
                &claims("g-1", Some("a@ex.com")),
                vec![],
                20,
            )
            .unwrap();
        assert_eq!(via_google.user_id, a.user_id);
    }
}
