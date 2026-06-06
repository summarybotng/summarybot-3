//! Tenant membership + invite endpoints (PRD §6.2/§6.3 TEN-005).
//!
//! Authentication is the access token (any valid caller); **authorization** is
//! the tenant membership: a handler that mutates members/invites requires the
//! caller to be a member of that tenant with the [`Permission`] the operation
//! needs ([`Role::allows`]). The repo `Mutex` is non-reentrant, so each handler
//! takes the lock once and does the authorization check and the operation under
//! the same guard.
//!
//! Invite tokens are opaque bearer secrets: [`InviteService`] returns the raw
//! token exactly once at issue time; only its hash is stored, and accept/revoke
//! present the raw token (never the hash) — the invitee never sees a tenant's
//! stored state.

use crate::auth::{now_secs, AuthUser};
use crate::{ApiError, AppState};
use axum::extract::{Host, Path, State};
use axum::http::StatusCode;
use axum::Json;
use domain::{
    normalize_subdomain, AcceptOutcome, Membership, Permission, Role, Tenant, TenantId, UserId,
};
use host::{resolve_tenant_by_host, InviteService};
use repository::{MembershipRepository, SqliteRepository, WorkspaceRepository};
use serde::{Deserialize, Serialize};

/// JSON shape of a membership.
#[derive(Serialize)]
pub struct MembershipDto {
    pub tenant_id: String,
    pub user_id: String,
    pub role: String,
}

impl From<Membership> for MembershipDto {
    fn from(m: Membership) -> Self {
        MembershipDto {
            tenant_id: m.tenant_id.as_str().to_string(),
            user_id: m.user_id.as_str().to_string(),
            role: m.role.as_str().to_string(),
        }
    }
}

/// JSON shape of a stored invite (never carries the raw token).
#[derive(Serialize)]
pub struct InviteDto {
    pub token_hash: String,
    pub tenant_id: String,
    pub email: String,
    pub role: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub status: String,
}

impl From<domain::Invite> for InviteDto {
    fn from(i: domain::Invite) -> Self {
        InviteDto {
            token_hash: i.token_hash,
            tenant_id: i.tenant_id.as_str().to_string(),
            email: i.email,
            role: i.role.as_str().to_string(),
            created_at: i.created_at,
            expires_at: i.expires_at,
            status: i.status.as_str().to_string(),
        }
    }
}

/// Authorize the caller for `permission` on `tenant`, under an already-held
/// repo guard (the `Mutex` is non-reentrant). 403 unless the caller is a member
/// of the tenant with sufficient role.
fn authorize(
    repo: &SqliteRepository,
    actor: &UserId,
    tenant: &TenantId,
    permission: Permission,
) -> Result<(), ApiError> {
    let membership = repo
        .get_membership(tenant, actor)?
        .ok_or(ApiError::Forbidden)?;
    if membership.can(permission) {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

/// Parse a tenant id from the path, mapping a malformed one to 400.
fn parse_tenant(raw: String) -> Result<TenantId, ApiError> {
    TenantId::parse(raw).map_err(|e| ApiError::bad_request(e.to_string()))
}

/// `GET /tenants/:tenant/members` — list members (any member may view).
pub async fn list_members(
    State(state): State<AppState>,
    user: AuthUser,
    Path(tenant): Path<String>,
) -> Result<Json<Vec<MembershipDto>>, ApiError> {
    let tenant = parse_tenant(tenant)?;
    let repo = state.repo.lock().expect("repo mutex");
    authorize(&repo, &user.0.sub, &tenant, Permission::ViewSummaries)?;
    let members = repo.list_members(&tenant)?;
    Ok(Json(members.into_iter().map(MembershipDto::from).collect()))
}

/// Body for setting a member's role.
#[derive(Deserialize)]
pub struct SetRoleRequest {
    pub role: String,
}

/// `PUT /tenants/:tenant/members/:user` — set (or add) a member's role.
pub async fn set_member_role(
    State(state): State<AppState>,
    user: AuthUser,
    Path((tenant, target)): Path<(String, String)>,
    Json(body): Json<SetRoleRequest>,
) -> Result<Json<MembershipDto>, ApiError> {
    let tenant = parse_tenant(tenant)?;
    let target = UserId::parse(target).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let role = Role::parse(&body.role)
        .ok_or_else(|| ApiError::bad_request(format!("unknown role: {}", body.role)))?;
    let repo = state.repo.lock().expect("repo mutex");
    authorize(&repo, &user.0.sub, &tenant, Permission::ManageMembers)?;
    let membership = Membership::new(tenant, target, role);
    repo.upsert_membership(&membership)?;
    Ok(Json(MembershipDto::from(membership)))
}

/// `DELETE /tenants/:tenant/members/:user` — remove a member (204, or 404).
pub async fn remove_member(
    State(state): State<AppState>,
    user: AuthUser,
    Path((tenant, target)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let tenant = parse_tenant(tenant)?;
    let target = UserId::parse(target).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let repo = state.repo.lock().expect("repo mutex");
    authorize(&repo, &user.0.sub, &tenant, Permission::ManageMembers)?;
    if repo.remove_membership(&tenant, &target)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

/// Body for issuing an invite.
#[derive(Deserialize)]
pub struct CreateInviteRequest {
    pub email: String,
    pub role: String,
    /// Optional lifetime override (seconds); defaults to the service default.
    #[serde(default)]
    pub ttl_secs: Option<i64>,
}

/// What issuing an invite returns — the raw `token` is shown **once**.
#[derive(Serialize)]
pub struct IssuedInviteDto {
    pub token: String,
    pub token_hash: String,
    pub email: String,
    pub role: String,
    pub expires_at: i64,
}

/// `POST /tenants/:tenant/invites` — issue an invite; returns the raw token once.
pub async fn create_invite(
    State(state): State<AppState>,
    user: AuthUser,
    Path(tenant): Path<String>,
    Json(body): Json<CreateInviteRequest>,
) -> Result<Json<IssuedInviteDto>, ApiError> {
    let tenant = parse_tenant(tenant)?;
    let role = Role::parse(&body.role)
        .ok_or_else(|| ApiError::bad_request(format!("unknown role: {}", body.role)))?;
    let repo = state.repo.lock().expect("repo mutex");
    authorize(&repo, &user.0.sub, &tenant, Permission::ManageMembers)?;
    let mut svc = InviteService::new(&*repo);
    if let Some(ttl) = body.ttl_secs {
        svc = svc.with_ttl(ttl);
    }
    let issued = svc
        .issue(tenant, body.email, role, now_secs())
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(Json(IssuedInviteDto {
        token: issued.raw_token,
        token_hash: issued.invite.token_hash,
        email: issued.invite.email,
        role: issued.invite.role.as_str().to_string(),
        expires_at: issued.invite.expires_at,
    }))
}

/// `GET /tenants/:tenant/invites` — list a tenant's invites (no raw tokens).
pub async fn list_invites(
    State(state): State<AppState>,
    user: AuthUser,
    Path(tenant): Path<String>,
) -> Result<Json<Vec<InviteDto>>, ApiError> {
    let tenant = parse_tenant(tenant)?;
    let repo = state.repo.lock().expect("repo mutex");
    authorize(&repo, &user.0.sub, &tenant, Permission::ManageMembers)?;
    let invites = repo.list_invites(&tenant)?;
    Ok(Json(invites.into_iter().map(InviteDto::from).collect()))
}

/// Body carrying a raw invite token.
#[derive(Deserialize)]
pub struct TokenRequest {
    pub token: String,
}

/// `POST /tenants/:tenant/invites/revoke` — revoke a pending invite (204/404).
pub async fn revoke_invite(
    State(state): State<AppState>,
    user: AuthUser,
    Path(tenant): Path<String>,
    Json(body): Json<TokenRequest>,
) -> Result<StatusCode, ApiError> {
    let tenant = parse_tenant(tenant)?;
    let repo = state.repo.lock().expect("repo mutex");
    authorize(&repo, &user.0.sub, &tenant, Permission::ManageMembers)?;
    if InviteService::new(&*repo)
        .revoke(&body.token)
        .map_err(|e| ApiError::bad_request(e.to_string()))?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

/// Outcome of accepting an invite.
#[derive(Serialize)]
pub struct AcceptResponse {
    pub accepted: bool,
    /// The granted role, present only on success.
    pub role: Option<String>,
    /// Why it was refused, present only on rejection (`not_pending` / `expired`).
    pub reason: Option<String>,
}

/// `POST /invites/accept` — accept an invite for the calling user. Not
/// tenant-scoped in the path: the token resolves the tenant, and an invitee is
/// not yet a member. A refusal is a clean 200 describing why (the invite state
/// machine is not an error).
pub async fn accept_invite(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<TokenRequest>,
) -> Result<Json<AcceptResponse>, ApiError> {
    let repo = state.repo.lock().expect("repo mutex");
    let outcome = InviteService::new(&*repo)
        .accept(&body.token, &user.0.sub, now_secs())
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(Json(match outcome {
        AcceptOutcome::Accept(role) => AcceptResponse {
            accepted: true,
            role: Some(role.as_str().to_string()),
            reason: None,
        },
        AcceptOutcome::Reject(reason) => AcceptResponse {
            accepted: false,
            role: None,
            reason: Some(
                match reason {
                    domain::AcceptReject::NotPending => "not_pending",
                    domain::AcceptReject::Expired => "expired",
                }
                .to_string(),
            ),
        },
    }))
}

/// Body for provisioning a tenant.
#[derive(Deserialize)]
pub struct CreateTenantRequest {
    pub id: String,
    pub name: String,
    /// Optional subdomain (TEN-001); validated/normalized if present.
    #[serde(default)]
    pub subdomain: Option<String>,
    /// Optional custom domain (TEN-002).
    #[serde(default)]
    pub custom_domain: Option<String>,
}

/// Normalize a custom domain: trim + lowercase, reject obviously-invalid forms.
/// Full hostname validation (public-suffix, DNS) is a later concern.
fn normalize_custom_domain(raw: &str) -> Option<String> {
    let s = raw.trim().to_ascii_lowercase();
    if s.is_empty() || !s.contains('.') || s.contains([' ', ':', '/', '@']) {
        return None;
    }
    Some(s)
}

/// `POST /tenants` — provision a tenant and make the caller its Owner (TEN-001).
/// This is the bootstrap: every other tenant operation needs an existing member,
/// so creation grants the creator `Owner`. The id, subdomain, and custom domain
/// must each be free (409 on conflict). Authenticated; no prior membership
/// required.
pub async fn provision_tenant(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<CreateTenantRequest>,
) -> Result<Json<TenantDto>, ApiError> {
    let id = TenantId::parse(body.id).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let subdomain = match body
        .subdomain
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(raw) => Some(
            normalize_subdomain(raw)
                .ok_or_else(|| ApiError::bad_request(format!("invalid subdomain: {raw}")))?,
        ),
        None => None,
    };
    let custom_domain = match body
        .custom_domain
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(raw) => Some(
            normalize_custom_domain(raw)
                .ok_or_else(|| ApiError::bad_request(format!("invalid custom domain: {raw}")))?,
        ),
        None => None,
    };
    let tenant = Tenant::new(
        id.clone(),
        body.name,
        subdomain.clone(),
        custom_domain.clone(),
    )
    .map_err(|e| ApiError::bad_request(e.to_string()))?;

    let repo = state.repo.lock().expect("repo mutex");
    // Pre-check uniqueness for clean 409s (the partial-unique indexes are the
    // race-proof backstop). The repo is single-writer under the mutex.
    if repo.get_tenant(&id)?.is_some() {
        return Err(ApiError::Conflict(format!(
            "tenant {} already exists",
            id.as_str()
        )));
    }
    if let Some(sub) = &subdomain {
        if repo.find_tenant_by_subdomain(sub)?.is_some() {
            return Err(ApiError::Conflict(format!("subdomain {sub} is taken")));
        }
    }
    if let Some(dom) = &custom_domain {
        if repo.find_tenant_by_custom_domain(dom)?.is_some() {
            return Err(ApiError::Conflict(format!("custom domain {dom} is taken")));
        }
    }
    repo.create_tenant(&tenant)?;
    repo.upsert_membership(&Membership::new(id, user.0.sub.clone(), Role::Owner))?;
    Ok(Json(TenantDto::from(tenant)))
}

/// Public tenant identity resolved from the request host (TEN-006).
#[derive(Serialize)]
pub struct TenantDto {
    pub id: String,
    pub name: String,
    pub subdomain: Option<String>,
    pub custom_domain: Option<String>,
}

impl From<domain::Tenant> for TenantDto {
    fn from(t: domain::Tenant) -> Self {
        TenantDto {
            id: t.id.as_str().to_string(),
            name: t.name,
            subdomain: t.subdomain,
            custom_domain: t.custom_domain,
        }
    }
}

/// `GET /tenant` — resolve the tenant for the request's `Host` header (TEN-006:
/// subdomain under the base domain, or a tenant's custom domain). Unauthenticated
/// on purpose: the login/branding surface needs the tenant *before* auth. The
/// apex/marketing host or an unknown host is a 404 (no tenant context).
pub async fn resolve_tenant(
    State(state): State<AppState>,
    Host(host): Host,
) -> Result<Json<TenantDto>, ApiError> {
    let tenant = {
        let repo = state.repo.lock().expect("repo mutex");
        resolve_tenant_by_host(&*repo, &host, &state.base_domain)?
    };
    tenant
        .map(|t| Json(TenantDto::from(t)))
        .ok_or(ApiError::NotFound)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_router;
    use axum::body::Body;
    use axum::http::Request;
    use domain::{DiscordProvider, ProviderClaims, Secret};
    use host::AuthService;
    use repository::SqliteRepository;
    use tower::ServiceExt;

    /// State seeded so that `owner_user` is an Owner of `tenant`; returns the
    /// owner's access token and the owner's user id.
    fn state_with_owner(tenant: &str) -> (AppState, String, String) {
        let repo = SqliteRepository::in_memory().unwrap();
        let key = Secret::new(b"tenancy-test-key".to_vec());
        let pair = {
            let svc = AuthService::new(&repo, &key);
            svc.login(
                &DiscordProvider,
                &ProviderClaims {
                    subject: "owner".into(),
                    email: None,
                },
                vec![],
                now_secs(),
            )
            .unwrap()
        };
        repo.upsert_membership(&Membership::new(
            TenantId::parse(tenant).unwrap(),
            pair.user_id.clone(),
            Role::Owner,
        ))
        .unwrap();
        (
            AppState::new(repo, key),
            pair.access_token,
            pair.user_id.as_str().to_string(),
        )
    }

    /// A second logged-in user (no membership); returns (token, user_id).
    fn login_outsider(state: &AppState, subject: &str) -> (String, String) {
        let repo = state.repo.lock().unwrap();
        let svc = AuthService::new(&*repo, &state.signing_key);
        let pair = svc
            .login(
                &DiscordProvider,
                &ProviderClaims {
                    subject: subject.into(),
                    email: None,
                },
                vec![],
                now_secs(),
            )
            .unwrap();
        (pair.access_token, pair.user_id.as_str().to_string())
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    }

    #[tokio::test]
    async fn resolves_tenant_from_subdomain_host() {
        use repository::WorkspaceRepository;
        let repo = SqliteRepository::in_memory().unwrap();
        repo.create_tenant(
            &domain::Tenant::new(
                TenantId::parse("t-acme").unwrap(),
                "Acme",
                Some("acme".into()),
                None,
            )
            .unwrap(),
        )
        .unwrap();
        // Default base domain is summarybot.app.
        let state = AppState::new(repo, Secret::new(b"k".to_vec()));

        let resp = build_router(state)
            .oneshot(
                Request::get("/tenant")
                    .header("host", "acme.summarybot.app")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["id"], "t-acme");
        assert_eq!(json["subdomain"], "acme");
    }

    #[tokio::test]
    async fn apex_host_has_no_tenant() {
        let (state, _, _) = state_with_owner("t1");
        let resp = build_router(state)
            .oneshot(
                Request::get("/tenant")
                    .header("host", "www.summarybot.app")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// A fresh state with one logged-in user holding no membership.
    fn state_with_user(subject: &str) -> (AppState, String) {
        let repo = SqliteRepository::in_memory().unwrap();
        let key = Secret::new(b"provision-test-key".to_vec());
        let token = {
            let svc = AuthService::new(&repo, &key);
            svc.login(
                &DiscordProvider,
                &ProviderClaims {
                    subject: subject.into(),
                    email: None,
                },
                vec![],
                now_secs(),
            )
            .unwrap()
            .access_token
        };
        (AppState::new(repo, key), token)
    }

    #[tokio::test]
    async fn provision_tenant_makes_caller_owner() {
        let (state, token) = state_with_user("founder");
        let app = build_router(state);

        let created = app
            .clone()
            .oneshot(
                Request::post("/tenants")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"id":"acme","name":"Acme Inc","subdomain":"Acme"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::OK);
        let cj = body_json(created).await;
        assert_eq!(cj["id"], "acme");
        assert_eq!(cj["subdomain"], "acme"); // normalized to lowercase

        // The creator is now Owner → can list members (a non-member would 403).
        let listed = app
            .oneshot(
                Request::get("/tenants/acme/members")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(listed.status(), StatusCode::OK);
        let lj = body_json(listed).await;
        assert_eq!(lj.as_array().unwrap().len(), 1);
        assert_eq!(lj[0]["role"], "owner");
    }

    #[tokio::test]
    async fn provision_rejects_duplicate_subdomain() {
        let (state, token) = state_with_user("founder");
        let app = build_router(state);
        let mk = |id: &str, sub: &str| {
            Request::post("/tenants")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"id":"{id}","name":"X","subdomain":"{sub}"}}"#
                )))
                .unwrap()
        };
        let first = app.clone().oneshot(mk("t-a", "shared")).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        // Different id, same subdomain → 409.
        let second = app.oneshot(mk("t-b", "shared")).await.unwrap();
        assert_eq!(second.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn provision_rejects_invalid_subdomain() {
        let (state, token) = state_with_user("founder");
        let resp = build_router(state)
            .oneshot(
                Request::post("/tenants")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"id":"t-bad","name":"X","subdomain":"not valid"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn provision_requires_auth() {
        let (state, _) = state_with_user("founder");
        let resp = build_router(state)
            .oneshot(
                Request::post("/tenants")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"id":"t-x","name":"X"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn owner_lists_members() {
        let (state, token, owner_id) = state_with_owner("t1");
        let resp = build_router(state)
            .oneshot(
                Request::get("/tenants/t1/members")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json.as_array().unwrap().len(), 1);
        assert_eq!(json[0]["user_id"], owner_id);
        assert_eq!(json[0]["role"], "owner");
    }

    #[tokio::test]
    async fn non_member_is_forbidden() {
        let (state, _owner_token, _) = state_with_owner("t1");
        let (outsider, _) = login_outsider(&state, "outsider");
        let resp = build_router(state)
            .oneshot(
                Request::get("/tenants/t1/members")
                    .header("authorization", format!("Bearer {outsider}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn owner_invites_and_outsider_accepts_then_is_a_member() {
        let (state, owner_token, _) = state_with_owner("t1");
        let (newcomer, newcomer_id) = login_outsider(&state, "newcomer");
        let app = build_router(state);

        // Owner issues a member invite; the raw token comes back once.
        let issued = app
            .clone()
            .oneshot(
                Request::post("/tenants/t1/invites")
                    .header("authorization", format!("Bearer {owner_token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"email":"new@example.com","role":"member"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(issued.status(), StatusCode::OK);
        let ij = body_json(issued).await;
        let raw = ij["token"].as_str().unwrap().to_string();
        assert!(!raw.is_empty());

        // Newcomer accepts with the raw token → granted "member".
        let accepted = app
            .clone()
            .oneshot(
                Request::post("/invites/accept")
                    .header("authorization", format!("Bearer {newcomer}"))
                    .header("content-type", "application/json")
                    .body(Body::from(format!(r#"{{"token":"{raw}"}}"#)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::OK);
        let aj = body_json(accepted).await;
        assert_eq!(aj["accepted"], true);
        assert_eq!(aj["role"], "member");

        // Newcomer now appears in the member list.
        let listed = app
            .oneshot(
                Request::get("/tenants/t1/members")
                    .header("authorization", format!("Bearer {owner_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let lj = body_json(listed).await;
        let ids: Vec<&str> = lj
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["user_id"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&newcomer_id.as_str()));
    }

    #[tokio::test]
    async fn accepting_a_bogus_token_is_a_clean_rejection() {
        let (state, _owner, _) = state_with_owner("t1");
        let (newcomer, _) = login_outsider(&state, "newcomer");
        let resp = build_router(state)
            .oneshot(
                Request::post("/invites/accept")
                    .header("authorization", format!("Bearer {newcomer}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"token":"nope"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["accepted"], false);
        assert_eq!(json["reason"], "not_pending");
    }

    #[tokio::test]
    async fn revoked_invite_cannot_be_accepted() {
        let (state, owner_token, _) = state_with_owner("t1");
        let (newcomer, _) = login_outsider(&state, "newcomer");
        let app = build_router(state);

        let issued = app
            .clone()
            .oneshot(
                Request::post("/tenants/t1/invites")
                    .header("authorization", format!("Bearer {owner_token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"email":"new@example.com","role":"admin"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let raw = body_json(issued).await["token"]
            .as_str()
            .unwrap()
            .to_string();

        // Owner revokes it.
        let revoked = app
            .clone()
            .oneshot(
                Request::post("/tenants/t1/invites/revoke")
                    .header("authorization", format!("Bearer {owner_token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(format!(r#"{{"token":"{raw}"}}"#)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(revoked.status(), StatusCode::NO_CONTENT);

        // Accept now refused.
        let accepted = app
            .oneshot(
                Request::post("/invites/accept")
                    .header("authorization", format!("Bearer {newcomer}"))
                    .header("content-type", "application/json")
                    .body(Body::from(format!(r#"{{"token":"{raw}"}}"#)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(body_json(accepted).await["accepted"], false);
    }

    #[tokio::test]
    async fn set_and_remove_member_role() {
        let (state, owner_token, _) = state_with_owner("t1");
        let app = build_router(state);
        let auth = format!("Bearer {owner_token}");

        // Add u-target as admin.
        let set = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/tenants/t1/members/u-target")
                    .header("authorization", &auth)
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"role":"admin"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(set.status(), StatusCode::OK);
        assert_eq!(body_json(set).await["role"], "admin");

        // Remove them → 204, then 404 on repeat.
        let removed = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/tenants/t1/members/u-target")
                    .header("authorization", &auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(removed.status(), StatusCode::NO_CONTENT);

        let again = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/tenants/t1/members/u-target")
                    .header("authorization", &auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(again.status(), StatusCode::NOT_FOUND);
    }
}
