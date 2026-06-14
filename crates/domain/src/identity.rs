//! Pluggable identity (PRD §2.4 WSP-001/005/010, §6.1; ADR-066).
//!
//! Authentication is **not** tied to Discord (WSP-001). Each auth method is an
//! [`IdentityProvider`] that turns provider-native, already-verified claims into
//! a normalized [`VerifiedIdentity`]. The provider abstraction is what lets the
//! core stay platform-agnostic: `sub` is a `user_uuid`, not a Discord id.
//!
//! This crate is pure (§12.0): the network/token I/O that *produces* the claims
//! (OAuth code→token→userinfo exchange, magic-link token verification) lives in
//! the host. Providers here only normalize the verified result, and
//! [`resolve_link`] decides — purely — what binding a verified identity implies.

use crate::{string_id, UserId, ValidationError};

/// An authentication method. Native provider vocabulary (Discord "snowflake",
/// Google "sub") stays inside the provider; the domain speaks `ProviderKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderKind {
    Discord,
    Google,
    Email,
}

impl ProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderKind::Discord => "discord",
            ProviderKind::Google => "google",
            ProviderKind::Email => "email",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, ValidationError> {
        match raw {
            "discord" => Ok(ProviderKind::Discord),
            "google" => Ok(ProviderKind::Google),
            "email" => Ok(ProviderKind::Email),
            _ => Err(ValidationError::Invalid {
                field: "provider",
                reason: "unknown identity provider",
            }),
        }
    }
}

string_id!(
    /// A provider's stable, opaque user id (Discord user id, Google `sub`, or a
    /// normalized email). Globally meaningful only when paired with a
    /// [`ProviderKind`] — `(provider, subject)` is the natural identity key.
    Subject,
    "subject",
    256
);

/// A normalized identity, the single output shape every provider produces. The
/// pair `(provider, subject)` is the identity key; `email` is advisory metadata
/// (present for SSO/email, optional for Discord) and is **never** used to merge
/// accounts — see [`resolve_link`] and WSP-010.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedIdentity {
    pub provider: ProviderKind,
    pub subject: Subject,
    pub email: Option<String>,
}

/// Provider-native claims, already verified by the host's I/O step, handed to a
/// provider for normalization. Kept deliberately minimal: the only thing the
/// domain needs is a stable subject and an optional contact email.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderClaims {
    /// The provider's stable user identifier (raw, pre-validation).
    pub subject: String,
    /// Contact email, if the provider supplies one.
    pub email: Option<String>,
}

/// An authentication method. Object-safe so providers compose behind
/// `dyn IdentityProvider` (PRD §12.2: "pluggable identity behind
/// `dyn IdentityProvider`"). The I/O that produces [`ProviderClaims`] is the
/// host's responsibility; implementors only normalize.
pub trait IdentityProvider {
    fn kind(&self) -> ProviderKind;

    /// Normalize already-verified claims into a [`VerifiedIdentity`], rejecting
    /// malformed input at this boundary (fail-fast, no silent fallback).
    fn normalize(&self, claims: &ProviderClaims) -> Result<VerifiedIdentity, ValidationError>;
}

/// Discord OAuth2 (AUTH-001). Subject is the Discord user id; email optional.
#[derive(Debug, Clone, Copy, Default)]
pub struct DiscordProvider;

impl IdentityProvider for DiscordProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Discord
    }

    fn normalize(&self, claims: &ProviderClaims) -> Result<VerifiedIdentity, ValidationError> {
        Ok(VerifiedIdentity {
            provider: ProviderKind::Discord,
            subject: Subject::parse(claims.subject.as_str())?,
            email: claims.email.clone(),
        })
    }
}

/// Google SSO (WSP-001). Subject is the OIDC `sub`; Google always returns a
/// verified email, so we require it.
#[derive(Debug, Clone, Copy, Default)]
pub struct GoogleProvider;

impl IdentityProvider for GoogleProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Google
    }

    fn normalize(&self, claims: &ProviderClaims) -> Result<VerifiedIdentity, ValidationError> {
        let email = claims.email.clone().ok_or(ValidationError::Empty {
            field: "google email",
        })?;
        Ok(VerifiedIdentity {
            provider: ProviderKind::Google,
            subject: Subject::parse(claims.subject.as_str())?,
            email: Some(normalize_email(&email)?),
        })
    }
}

/// Email magic link (WSP-001). The email *is* the identity: subject and email
/// are the same normalized address, so the same inbox always maps to the same
/// subject regardless of casing.
#[derive(Debug, Clone, Copy, Default)]
pub struct EmailProvider;

impl IdentityProvider for EmailProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Email
    }

    fn normalize(&self, claims: &ProviderClaims) -> Result<VerifiedIdentity, ValidationError> {
        // For magic-link, the verified claim is the email; ignore any separate
        // `subject` and derive the canonical subject from the address.
        let raw = claims
            .email
            .as_deref()
            .filter(|e| !e.trim().is_empty())
            .unwrap_or(claims.subject.as_str());
        let email = normalize_email(raw)?;
        Ok(VerifiedIdentity {
            provider: ProviderKind::Email,
            subject: Subject::parse(email.as_str())?,
            email: Some(email),
        })
    }
}

/// Minimal, boundary-level email normalization: trim, lowercase, and require a
/// single interior `@`. Not RFC-5322 validation — just enough to canonicalize
/// the identity key and reject obvious garbage (deliverability is proven by the
/// magic link itself).
fn normalize_email(raw: &str) -> Result<String, ValidationError> {
    let trimmed = raw.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        return Err(ValidationError::Empty { field: "email" });
    }
    let at = trimmed.find('@');
    let valid = matches!(at, Some(i) if i > 0 && i < trimmed.len() - 1)
        && trimmed.matches('@').count() == 1;
    if !valid {
        return Err(ValidationError::Invalid {
            field: "email",
            reason: "must contain a single interior @",
        });
    }
    Ok(trimmed)
}

/// A binding of a verified identity to an application user (`user_uuid`). The
/// `(provider, subject)` pair is unique across the system (WSP-010): one
/// platform account binds to exactly one user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityLink {
    pub provider: ProviderKind,
    pub subject: Subject,
    pub user_id: UserId,
}

impl IdentityLink {
    pub fn new(identity: &VerifiedIdentity, user_id: UserId) -> Self {
        Self {
            provider: identity.provider,
            subject: identity.subject.clone(),
            user_id,
        }
    }
}

/// What the caller intends to do with a freshly verified identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkIntent {
    /// Anonymous sign-in: there is no acting user yet.
    Login,
    /// An already-authenticated user is attaching another identity to their
    /// account (WSP-005, unified identity across linked platforms).
    Link { acting_user: UserId },
}

/// The decision [`resolve_link`] reaches for a verified identity, given who (if
/// anyone) it is currently bound to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkOutcome {
    /// Identity is bound: sign that user in.
    SignIn(UserId),
    /// Unbound, anonymous: provision a new user, then bind.
    Provision,
    /// Unbound, acting user present: bind it to that user.
    Link(UserId),
    /// Already bound to the acting user — idempotent, nothing to do.
    AlreadyLinked(UserId),
    /// Bound to a **different** user. REJECTED: no auto-merge, no silent
    /// reassign (WSP-010, open-question #1). Resolution must go through the
    /// claim/transfer workflow — self-service re-auth (WSP-012) or an
    /// escalated human approver (WSP-013).
    Collision {
        bound_to: UserId,
        attempted_by: UserId,
    },
}

/// Pure WSP-010 policy (open-question #1, RESOLVED: reject + claim/transfer).
/// `existing` is the user currently bound to the identity, if any. This is the
/// single place the collision rule lives, so it can be exhaustively tested and
/// can never be bypassed by a forgetful call site.
pub fn resolve_link(intent: &LinkIntent, existing: Option<&UserId>) -> LinkOutcome {
    match (intent, existing) {
        (LinkIntent::Login, Some(user)) => LinkOutcome::SignIn(user.clone()),
        (LinkIntent::Login, None) => LinkOutcome::Provision,
        (LinkIntent::Link { acting_user }, None) => LinkOutcome::Link(acting_user.clone()),
        (LinkIntent::Link { acting_user }, Some(bound)) if bound == acting_user => {
            LinkOutcome::AlreadyLinked(bound.clone())
        }
        (LinkIntent::Link { acting_user }, Some(bound)) => LinkOutcome::Collision {
            bound_to: bound.clone(),
            attempted_by: acting_user.clone(),
        },
    }
}

/// Who must approve a contested identity claim (WSP-011..014; ADR-119). A
/// [`LinkOutcome::Collision`] can't be auto-resolved (WSP-010); this decides the
/// adjudication route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimRoute {
    /// The claimant proved control by completing the provider's own OAuth
    /// (WSP-012) — auto-approve, no human needed.
    SelfService,
    /// Claimant + current owner share a tenant: the **tenant admin** adjudicates
    /// (WSP-013), the obvious in-tenant authority.
    TenantAdmin,
    /// A **cross-tenant** collision: no tenant admin has authority over both, so
    /// it escalates to a **platform operator** (WSP-013/ADR-119).
    Operator,
}

/// Decide who must approve a contested claim (ADR-119). Self-verification wins
/// outright (WSP-012); otherwise a shared tenant routes to its admin and a
/// cross-tenant dispute routes to a platform operator.
pub fn decide_claim_route(can_self_verify: bool, share_a_tenant: bool) -> ClaimRoute {
    if can_self_verify {
        ClaimRoute::SelfService
    } else if share_a_tenant {
        ClaimRoute::TenantAdmin
    } else {
        ClaimRoute::Operator
    }
}

/// State of an identity claim's lifecycle (WSP-011..014).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimStatus {
    Pending,
    Approved,
    Denied,
}

impl ClaimStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ClaimStatus::Pending => "pending",
            ClaimStatus::Approved => "approved",
            ClaimStatus::Denied => "denied",
        }
    }
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "pending" => Some(ClaimStatus::Pending),
            "approved" => Some(ClaimStatus::Approved),
            "denied" => Some(ClaimStatus::Denied),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims(subject: &str, email: Option<&str>) -> ProviderClaims {
        ProviderClaims {
            subject: subject.to_string(),
            email: email.map(str::to_string),
        }
    }

    #[test]
    fn claim_route_self_verify_beats_everything_else() {
        // WSP-012: completing the provider OAuth auto-approves regardless of tenancy.
        assert_eq!(decide_claim_route(true, true), ClaimRoute::SelfService);
        assert_eq!(decide_claim_route(true, false), ClaimRoute::SelfService);
        // Same tenant → tenant admin; cross-tenant → platform operator (ADR-119).
        assert_eq!(decide_claim_route(false, true), ClaimRoute::TenantAdmin);
        assert_eq!(decide_claim_route(false, false), ClaimRoute::Operator);
    }

    #[test]
    fn provider_kind_roundtrips() {
        for k in [
            ProviderKind::Discord,
            ProviderKind::Google,
            ProviderKind::Email,
        ] {
            assert_eq!(ProviderKind::parse(k.as_str()), Ok(k));
        }
        assert!(ProviderKind::parse("facebook").is_err());
    }

    #[test]
    fn discord_normalizes_subject_keeps_optional_email() {
        let id = DiscordProvider
            .normalize(&claims("100200300", None))
            .unwrap();
        assert_eq!(id.provider, ProviderKind::Discord);
        assert_eq!(id.subject.as_str(), "100200300");
        assert_eq!(id.email, None);
    }

    #[test]
    fn google_requires_email_and_lowercases_it() {
        assert!(GoogleProvider.normalize(&claims("sub-1", None)).is_err());
        let id = GoogleProvider
            .normalize(&claims("sub-1", Some("Person@Example.COM")))
            .unwrap();
        assert_eq!(id.email.as_deref(), Some("person@example.com"));
        assert_eq!(id.subject.as_str(), "sub-1");
    }

    #[test]
    fn email_identity_is_the_normalized_address() {
        // Subject is derived from the email, so casing can't fork the identity.
        let lower = EmailProvider
            .normalize(&claims("ignored", Some("a@b.com")))
            .unwrap();
        let upper = EmailProvider
            .normalize(&claims("ignored", Some("  A@B.COM  ")))
            .unwrap();
        assert_eq!(lower.subject, upper.subject);
        assert_eq!(lower.subject.as_str(), "a@b.com");
    }

    #[test]
    fn email_provider_rejects_malformed_addresses() {
        for bad in ["", "no-at-sign", "@leading", "trailing@", "a@@b.com"] {
            assert!(
                EmailProvider.normalize(&claims(bad, Some(bad))).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn providers_compose_behind_dyn_trait() {
        // Proves the abstraction: heterogeneous providers, one interface.
        let providers: Vec<Box<dyn IdentityProvider>> = vec![
            Box::new(DiscordProvider),
            Box::new(GoogleProvider),
            Box::new(EmailProvider),
        ];
        let kinds: Vec<ProviderKind> = providers.iter().map(|p| p.kind()).collect();
        assert_eq!(
            kinds,
            vec![
                ProviderKind::Discord,
                ProviderKind::Google,
                ProviderKind::Email
            ]
        );
    }

    fn user(id: &str) -> UserId {
        UserId::parse(id).unwrap()
    }

    #[test]
    fn login_signs_in_bound_user_or_provisions() {
        let u = user("u1");
        assert_eq!(
            resolve_link(&LinkIntent::Login, Some(&u)),
            LinkOutcome::SignIn(u.clone())
        );
        assert_eq!(
            resolve_link(&LinkIntent::Login, None),
            LinkOutcome::Provision
        );
    }

    #[test]
    fn linking_unbound_identity_binds_to_acting_user() {
        let acting = user("u1");
        assert_eq!(
            resolve_link(
                &LinkIntent::Link {
                    acting_user: acting.clone()
                },
                None
            ),
            LinkOutcome::Link(acting)
        );
    }

    #[test]
    fn relinking_own_identity_is_idempotent() {
        let acting = user("u1");
        assert_eq!(
            resolve_link(
                &LinkIntent::Link {
                    acting_user: acting.clone()
                },
                Some(&acting)
            ),
            LinkOutcome::AlreadyLinked(acting)
        );
    }

    #[test]
    fn linking_someone_elses_identity_is_a_collision_not_a_merge() {
        // WSP-010 / Q#1: reject, never auto-merge or silently reassign.
        let acting = user("intruder");
        let owner = user("owner");
        assert_eq!(
            resolve_link(
                &LinkIntent::Link {
                    acting_user: acting.clone()
                },
                Some(&owner)
            ),
            LinkOutcome::Collision {
                bound_to: owner,
                attempted_by: acting,
            }
        );
    }
}
