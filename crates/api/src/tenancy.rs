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
use axum::extract::{Host, Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use domain::{current_window, remaining_micros, Budget, Spend};
use domain::{
    normalize_subdomain, AcceptOutcome, Membership, Permission, Role, Tenant, TenantId, UserId,
};
use host::{resolve_tenant_by_host, InviteService};
use repository::{
    AuditEntry, BudgetRepository, BudgetRow, IdentityRepository, LlmConfigRepository,
    MembershipRepository, SqliteRepository, TenantLlmConfig, WorkspaceRepository,
};
use serde::{Deserialize, Deserializer, Serialize};

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
/// of the tenant with sufficient role. Shared with the workspace endpoints.
pub(crate) fn authorize(
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

/// Parse a tenant id from the path, mapping a malformed one to 400. Shared with
/// the workspace endpoints.
pub(crate) fn parse_tenant(raw: String) -> Result<TenantId, ApiError> {
    TenantId::parse(raw).map_err(|e| ApiError::bad_request(e.to_string()))
}

/// Append an audit entry for a tenant admin action (WSP-014). Best-effort: a
/// logging failure must never fail the action it records. Call under the held
/// repo guard, with the acting user as `actor` (so it shows in their tenant's
/// audit view, which is scoped by member ids).
pub(crate) fn audit(repo: &SqliteRepository, actor: &UserId, action: &str, detail: String) {
    let _ = repo.append_audit(&AuditEntry {
        ts: now_secs(),
        actor: Some(actor.clone()),
        action: action.to_string(),
        detail,
    });
}

/// JSON shape of an audit-ledger entry.
#[derive(Serialize)]
pub struct AuditEntryDto {
    pub ts: i64,
    pub actor: Option<String>,
    pub action: String,
    pub detail: String,
}

impl From<AuditEntry> for AuditEntryDto {
    fn from(e: AuditEntry) -> Self {
        AuditEntryDto {
            ts: e.ts,
            actor: e.actor.map(|a| a.as_str().to_string()),
            action: e.action,
            detail: e.detail,
        }
    }
}

/// `?limit=50&offset=0` for the audit listing.
#[derive(Deserialize)]
pub struct AuditQuery {
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// `GET /workspaces/:ws/audit` — security/admin events, newest first (WSP-014).
/// Workspace-scoped to fit the dashboard: resolves the workspace's tenant and
/// requires `ManageSettings` (Admin+) on it. The audit ledger is process-global
/// with no tenant column, so it's scoped to that tenant's **members** (events by
/// your members); system/anonymous events aren't attributed to a tenant. A
/// workspace with no tenant row (e.g. a dev workspace) yields an empty list.
pub async fn list_audit(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
    Query(q): Query<AuditQuery>,
) -> Result<Json<Vec<AuditEntryDto>>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let repo = state.repo.lock().expect("repo mutex");
    let Some(ws_row) = repo.find_workspace(&workspace)? else {
        return Ok(Json(vec![])); // unprovisioned workspace → no tenant, nothing to show
    };
    let tenant = ws_row.tenant_id;
    authorize(&repo, &user.0.sub, &tenant, Permission::ManageSettings)?;
    let actors: Vec<String> = repo
        .list_members(&tenant)?
        .into_iter()
        .map(|m| m.user_id.as_str().to_string())
        .collect();
    let limit = q.limit.unwrap_or(50).min(200);
    let entries = repo.list_audit_by_actors(&actors, limit, q.offset.unwrap_or(0))?;
    Ok(Json(entries.into_iter().map(AuditEntryDto::from).collect()))
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
    let membership = Membership::new(tenant, target.clone(), role);
    repo.upsert_membership(&membership)?;
    audit(
        &repo,
        &user.0.sub,
        "member.role_set",
        format!("{} -> {}", target.as_str(), role.as_str()),
    );
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
        audit(
            &repo,
            &user.0.sub,
            "member.removed",
            target.as_str().to_string(),
        );
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
    audit(
        &repo,
        &user.0.sub,
        "invite.issued",
        format!("{} ({})", issued.invite.email, issued.invite.role.as_str()),
    );
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
    /// The raw invite token (used by the invitee). Admins revoking from the
    /// invite list pass `token_hash` instead (the raw token is shown only once).
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub token_hash: Option<String>,
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
    let svc = InviteService::new(&*repo);
    // Admins revoke by `token_hash` (from the invite list); the raw `token` path
    // remains for completeness.
    let revoked = match (&body.token_hash, &body.token) {
        (Some(hash), _) => svc.revoke_by_hash(hash),
        (None, Some(raw)) => svc.revoke(raw),
        (None, None) => return Err(ApiError::bad_request("token or token_hash required")),
    }
    .map_err(|e| ApiError::bad_request(e.to_string()))?;
    if revoked {
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
    let raw = body
        .token
        .as_deref()
        .ok_or_else(|| ApiError::bad_request("token required"))?;
    let repo = state.repo.lock().expect("repo mutex");
    let outcome = InviteService::new(&*repo)
        .accept(raw, &user.0.sub, now_secs())
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

/// Deserialize a field as a "double option" so a PATCH can distinguish three
/// cases: **absent** (field missing → `None`, leave unchanged), **`null`**
/// (`Some(None)`, clear it), and **a value** (`Some(Some(v))`, set it). Pair
/// with `#[serde(default)]` so an absent field becomes the outer `None`.
fn double_option<'de, T, D>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(de).map(Some)
}

/// Body for updating a tenant's settings (TEN-001/TEN-002). Every field is
/// optional with PATCH semantics: omit to keep, send `null` to clear, send a
/// value to set. `name` can't be cleared (a tenant must be named), so it's a
/// plain optional.
#[derive(Deserialize)]
pub struct UpdateTenantRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    pub subdomain: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub custom_domain: Option<Option<String>>,
}

/// `PUT /tenants/:tenant` — update tenant settings (TEN-001/TEN-002). Requires
/// `ManageSettings` (Admin+). Subdomain/custom domain are validated when set and
/// must be free (409), ignoring the tenant's own current value.
pub async fn update_tenant(
    State(state): State<AppState>,
    user: AuthUser,
    Path(tenant): Path<String>,
    Json(body): Json<UpdateTenantRequest>,
) -> Result<Json<TenantDto>, ApiError> {
    let tenant_id = parse_tenant(tenant)?;
    let repo = state.repo.lock().expect("repo mutex");
    authorize(&repo, &user.0.sub, &tenant_id, Permission::ManageSettings)?;
    let current = repo.get_tenant(&tenant_id)?.ok_or(ApiError::NotFound)?;

    // Name: keep current unless a non-empty replacement is given.
    let name = match body.name.as_deref().map(str::trim) {
        Some(n) if !n.is_empty() => n.to_string(),
        Some(_) => return Err(ApiError::bad_request("name must not be empty")),
        None => current.name.clone(),
    };

    // Subdomain / custom domain: absent → keep; null → clear; value → validate.
    let subdomain = resolve_update(body.subdomain, current.subdomain.clone(), |raw| {
        normalize_subdomain(raw).ok_or_else(|| format!("invalid subdomain: {raw}"))
    })?;
    let custom_domain = resolve_update(body.custom_domain, current.custom_domain.clone(), |raw| {
        normalize_custom_domain(raw).ok_or_else(|| format!("invalid custom domain: {raw}"))
    })?;

    // Uniqueness, ignoring this tenant's own current values.
    if let Some(sub) = &subdomain {
        if let Some(other) = repo.find_tenant_by_subdomain(sub)? {
            if other.id != tenant_id {
                return Err(ApiError::Conflict(format!("subdomain {sub} is taken")));
            }
        }
    }
    if let Some(dom) = &custom_domain {
        if let Some(other) = repo.find_tenant_by_custom_domain(dom)? {
            if other.id != tenant_id {
                return Err(ApiError::Conflict(format!("custom domain {dom} is taken")));
            }
        }
    }

    let updated = Tenant::new(tenant_id, name, subdomain, custom_domain)
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    repo.update_tenant(&updated)?;
    Ok(Json(TenantDto::from(updated)))
}

/// Apply PATCH semantics to one optional field: `None` keeps `current`,
/// `Some(None)` clears it, `Some(Some(raw))` validates+normalizes via `validate`.
fn resolve_update(
    field: Option<Option<String>>,
    current: Option<String>,
    validate: impl Fn(&str) -> Result<String, String>,
) -> Result<Option<String>, ApiError> {
    match field {
        None => Ok(current),
        Some(None) => Ok(None),
        Some(Some(raw)) => {
            let raw = raw.trim();
            if raw.is_empty() {
                // An empty string is treated as a clear, like null.
                Ok(None)
            } else {
                validate(raw).map(Some).map_err(ApiError::bad_request)
            }
        }
    }
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

/// Response shape of a tenant's LLM config (ADR-125). The BYO key is **never**
/// returned — only whether one is set.
#[derive(Serialize)]
pub struct LlmConfigDto {
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub has_key: bool,
}

fn config_dto(cfg: &TenantLlmConfig) -> LlmConfigDto {
    LlmConfigDto {
        base_url: cfg.base_url.clone(),
        model: cfg.model.clone(),
        has_key: cfg.api_key_enc.is_some(),
    }
}

/// Request body for setting LLM config. `base_url`/`model` are full replacements;
/// `api_key` is tri-state (omit = keep, null/"" = clear, value = set + encrypt).
#[derive(Deserialize)]
pub struct SetLlmConfigRequest {
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    pub api_key: Option<Option<String>>,
}

/// `GET /tenants/:tenant/llm-config` — the tenant's LLM override (ManageSettings).
pub async fn get_llm_config(
    State(state): State<AppState>,
    user: AuthUser,
    Path(tenant): Path<String>,
) -> Result<Json<LlmConfigDto>, ApiError> {
    let tenant = parse_tenant(tenant)?;
    let repo = state.repo.lock().expect("repo mutex");
    authorize(&repo, &user.0.sub, &tenant, Permission::ManageSettings)?;
    let cfg = repo.get_llm_config(&tenant)?.unwrap_or_default();
    Ok(Json(config_dto(&cfg)))
}

/// `PUT /tenants/:tenant/llm-config` — set the tenant's LLM override.
/// `base_url` (validated http(s)) and `model` replace; `api_key` is tri-state
/// (omit = keep, null/empty = clear, value = encrypt + store, ADR-125 2b).
/// Setting a key requires the server's master key (`LLM_CONFIG_KEY`).
/// Requires ManageSettings.
pub async fn set_llm_config(
    State(state): State<AppState>,
    user: AuthUser,
    Path(tenant): Path<String>,
    Json(body): Json<SetLlmConfigRequest>,
) -> Result<Json<LlmConfigDto>, ApiError> {
    let tenant = parse_tenant(tenant)?;
    let base_url = match body
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(u) => {
            if !(u.starts_with("http://") || u.starts_with("https://"))
                || u.contains(char::is_whitespace)
            {
                return Err(ApiError::bad_request("base_url must be an http(s) URL"));
            }
            Some(u.to_string())
        }
        None => None,
    };
    let model = body
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let repo = state.repo.lock().expect("repo mutex");
    authorize(&repo, &user.0.sub, &tenant, Permission::ManageSettings)?;
    // Tri-state key: keep current, clear, or encrypt + set.
    let current = repo.get_llm_config(&tenant)?.unwrap_or_default();
    let api_key_enc = match body.api_key {
        None => current.api_key_enc,
        Some(None) => None,
        Some(Some(k)) if k.trim().is_empty() => None,
        Some(Some(k)) => {
            let master = state.master_key().ok_or_else(|| {
                ApiError::bad_request(
                    "key encryption not configured on the server (set LLM_CONFIG_KEY)",
                )
            })?;
            Some(
                host::encrypt_secret(master, k.trim())
                    .map_err(|e| ApiError::Internal(e.to_string()))?,
            )
        }
    };
    let cfg = TenantLlmConfig {
        base_url,
        model,
        api_key_enc,
    };
    repo.set_llm_config(&tenant, &cfg)?;
    Ok(Json(config_dto(&cfg)))
}

/// `DELETE /tenants/:tenant/llm-config` — clear the override (revert to process
/// defaults). Idempotent 204. Requires ManageSettings.
pub async fn clear_llm_config(
    State(state): State<AppState>,
    user: AuthUser,
    Path(tenant): Path<String>,
) -> Result<StatusCode, ApiError> {
    let tenant = parse_tenant(tenant)?;
    let repo = state.repo.lock().expect("repo mutex");
    authorize(&repo, &user.0.sub, &tenant, Permission::ManageSettings)?;
    repo.clear_llm_config(&tenant)?;
    Ok(StatusCode::NO_CONTENT)
}

/// JSON shape of a tenant's LLM budget (ADR-125 Phase 3), reflecting the current
/// (rolled) window.
#[derive(Serialize)]
pub struct BudgetDto {
    pub configured: bool,
    pub limit_micros: i64,
    pub period_secs: i64,
    pub spent_micros: i64,
    pub remaining_micros: i64,
    pub period_start: i64,
}

/// Body for granting/updating a budget.
#[derive(Deserialize)]
pub struct SetBudgetRequest {
    pub limit_micros: i64,
    pub period_secs: i64,
}

/// `GET /tenants/:tenant/budget` — current budget + spend (ManageBilling/Owner).
pub async fn get_budget(
    State(state): State<AppState>,
    user: AuthUser,
    Path(tenant): Path<String>,
) -> Result<Json<BudgetDto>, ApiError> {
    let tenant = parse_tenant(tenant)?;
    let repo = state.repo.lock().expect("repo mutex");
    authorize(&repo, &user.0.sub, &tenant, Permission::ManageBilling)?;
    Ok(Json(match repo.get_budget(&tenant)? {
        Some(row) => {
            let budget = Budget {
                limit_micros: row.limit_micros,
                period_secs: row.period_secs,
            };
            let window = current_window(
                Spend {
                    period_start: row.period_start,
                    spent_micros: row.spent_micros,
                },
                budget,
                now_secs(),
            );
            BudgetDto {
                configured: true,
                limit_micros: row.limit_micros,
                period_secs: row.period_secs,
                spent_micros: window.spent_micros,
                remaining_micros: remaining_micros(window, budget),
                period_start: window.period_start,
            }
        }
        None => BudgetDto {
            configured: false,
            limit_micros: 0,
            period_secs: 0,
            spent_micros: 0,
            remaining_micros: 0,
            period_start: 0,
        },
    }))
}

/// `PUT /tenants/:tenant/budget` — grant/update a budget (ManageBilling/Owner).
/// Changing the limit/period preserves the current window's accrued spend.
pub async fn set_budget(
    State(state): State<AppState>,
    user: AuthUser,
    Path(tenant): Path<String>,
    Json(body): Json<SetBudgetRequest>,
) -> Result<Json<BudgetDto>, ApiError> {
    let tenant = parse_tenant(tenant)?;
    if body.limit_micros < 0 || body.period_secs < 0 {
        return Err(ApiError::bad_request(
            "limit_micros and period_secs must be >= 0",
        ));
    }
    let repo = state.repo.lock().expect("repo mutex");
    authorize(&repo, &user.0.sub, &tenant, Permission::ManageBilling)?;
    let existing = repo.get_budget(&tenant)?;
    let row = BudgetRow {
        limit_micros: body.limit_micros,
        period_secs: body.period_secs,
        period_start: existing.map(|e| e.period_start).unwrap_or_else(now_secs),
        spent_micros: existing.map(|e| e.spent_micros).unwrap_or(0),
    };
    repo.upsert_budget(&tenant, &row)?;
    let budget = Budget {
        limit_micros: row.limit_micros,
        period_secs: row.period_secs,
    };
    let window = current_window(
        Spend {
            period_start: row.period_start,
            spent_micros: row.spent_micros,
        },
        budget,
        now_secs(),
    );
    Ok(Json(BudgetDto {
        configured: true,
        limit_micros: row.limit_micros,
        period_secs: row.period_secs,
        spent_micros: window.spent_micros,
        remaining_micros: remaining_micros(window, budget),
        period_start: window.period_start,
    }))
}

/// `DELETE /tenants/:tenant/budget` — remove the grant (ManageBilling/Owner).
pub async fn clear_budget(
    State(state): State<AppState>,
    user: AuthUser,
    Path(tenant): Path<String>,
) -> Result<StatusCode, ApiError> {
    let tenant = parse_tenant(tenant)?;
    let repo = state.repo.lock().expect("repo mutex");
    authorize(&repo, &user.0.sub, &tenant, Permission::ManageBilling)?;
    repo.clear_budget(&tenant)?;
    Ok(StatusCode::NO_CONTENT)
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

    /// Provision a tenant and return (state, owner token, tenant id).
    async fn provisioned(id: &str, subdomain: &str) -> (AppState, String) {
        let (state, token) = state_with_user("founder");
        let app = build_router(state.clone());
        let resp = app
            .oneshot(
                Request::post("/tenants")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"id":"{id}","name":"Acme","subdomain":"{subdomain}"}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        (state, token)
    }

    #[tokio::test]
    async fn update_tenant_renames_and_moves_subdomain() {
        let (state, token) = provisioned("acme", "acme").await;
        let resp = build_router(state)
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/tenants/acme")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name":"Acme Corp","subdomain":"acmecorp"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let j = body_json(resp).await;
        assert_eq!(j["name"], "Acme Corp");
        assert_eq!(j["subdomain"], "acmecorp");
    }

    #[tokio::test]
    async fn update_tenant_null_clears_subdomain() {
        let (state, token) = provisioned("acme", "acme").await;
        let resp = build_router(state)
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/tenants/acme")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"subdomain":null}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        // subdomain cleared; name untouched (absent field = keep).
        let j = body_json(resp).await;
        assert!(j["subdomain"].is_null());
        assert_eq!(j["name"], "Acme");
    }

    #[tokio::test]
    async fn update_tenant_requires_manage_settings() {
        // An outsider (no membership) can't update settings → 403.
        let (state, _owner) = provisioned("acme", "acme").await;
        let (outsider, _) = login_outsider(&state, "outsider");
        let resp = build_router(state)
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/tenants/acme")
                    .header("authorization", format!("Bearer {outsider}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name":"Hacked"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn update_tenant_rejects_taken_subdomain() {
        // Two tenants under one owner; second can't steal the first's subdomain.
        let (state, token) = provisioned("t-a", "alpha").await;
        let app = build_router(state);
        app.clone()
            .oneshot(
                Request::post("/tenants")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"id":"t-b","name":"B","subdomain":"beta"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/tenants/t-b")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"subdomain":"alpha"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn update_tenant_keeping_own_subdomain_is_allowed() {
        // Re-asserting the tenant's *own* subdomain must not 409 against itself.
        let (state, token) = provisioned("acme", "acme").await;
        let resp = build_router(state)
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/tenants/acme")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name":"Acme 2","subdomain":"acme"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await["subdomain"], "acme");
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
    async fn llm_config_set_get_clear_round_trip() {
        let (state, token, _) = state_with_owner("t1");
        let app = build_router(state);
        let auth = format!("Bearer {token}");

        // Initially empty.
        let g0 = app
            .clone()
            .oneshot(
                Request::get("/tenants/t1/llm-config")
                    .header("authorization", &auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(g0.status(), StatusCode::OK);
        let j0 = body_json(g0).await;
        assert!(j0["base_url"].is_null() && j0["model"].is_null());

        // Set a tenant endpoint + model.
        let set = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/tenants/t1/llm-config")
                    .header("authorization", &auth)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"base_url":"http://mac-mini.local:11434/v1","model":"llama3.1"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(set.status(), StatusCode::OK);
        let js = body_json(set).await;
        assert_eq!(js["base_url"], "http://mac-mini.local:11434/v1");
        assert_eq!(js["model"], "llama3.1");

        // Read back.
        let g1 = app
            .clone()
            .oneshot(
                Request::get("/tenants/t1/llm-config")
                    .header("authorization", &auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(body_json(g1).await["model"], "llama3.1");

        // Clear → 204, then empty again.
        let del = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/tenants/t1/llm-config")
                    .header("authorization", &auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(del.status(), StatusCode::NO_CONTENT);
        let g2 = app
            .oneshot(
                Request::get("/tenants/t1/llm-config")
                    .header("authorization", &auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(body_json(g2).await["base_url"].is_null());
    }

    #[tokio::test]
    async fn llm_config_rejects_bad_url_and_non_member() {
        let (state, token, _) = state_with_owner("t1");
        let (outsider, _) = login_outsider(&state, "outsider");
        let app = build_router(state);

        // Invalid base_url → 400.
        let bad = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/tenants/t1/llm-config")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"base_url":"not a url"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

        // Non-member → 403.
        let forbidden = app
            .oneshot(
                Request::get("/tenants/t1/llm-config")
                    .header("authorization", format!("Bearer {outsider}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    }

    /// State where `owner` is Owner of `tenant` AND the server has a master key.
    fn state_with_owner_and_master(tenant: &str) -> (AppState, String) {
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
            AppState::new(repo, key).with_config_key([5u8; 32]),
            pair.access_token,
        )
    }

    #[tokio::test]
    async fn llm_config_byo_key_encrypts_and_is_never_returned() {
        let (state, token) = state_with_owner_and_master("t1");
        let app = build_router(state);
        let auth = format!("Bearer {token}");

        // Set a BYO key + model.
        let set = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/tenants/t1/llm-config")
                    .header("authorization", &auth)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"gpt-4o-mini","api_key":"sk-secret-123"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(set.status(), StatusCode::OK);
        let js = body_json(set).await;
        assert_eq!(js["has_key"], true);
        assert_eq!(js["model"], "gpt-4o-mini");
        assert!(js["api_key"].is_null()); // never echoed back

        // GET reports has_key but no key material.
        let g = app
            .clone()
            .oneshot(
                Request::get("/tenants/t1/llm-config")
                    .header("authorization", &auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let gj = body_json(g).await;
        assert_eq!(gj["has_key"], true);
        assert!(gj["api_key"].is_null());

        // Omitting api_key on a later save keeps the key (tri-state).
        let keep = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/tenants/t1/llm-config")
                    .header("authorization", &auth)
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"gpt-4o"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(body_json(keep).await["has_key"], true);

        // null clears it.
        let clear = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/tenants/t1/llm-config")
                    .header("authorization", &auth)
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"api_key":null}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(body_json(clear).await["has_key"], false);
    }

    #[tokio::test]
    async fn llm_config_key_requires_server_master_key() {
        // state_with_owner builds state WITHOUT a master key.
        let (state, token, _) = state_with_owner("t1");
        let resp = build_router(state)
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/tenants/t1/llm-config")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"api_key":"sk-x"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn budget_set_get_clear_round_trip() {
        let (state, token, _) = state_with_owner("t1");
        let app = build_router(state);
        let auth = format!("Bearer {token}");

        // Unset initially.
        let g0 = app
            .clone()
            .oneshot(
                Request::get("/tenants/t1/budget")
                    .header("authorization", &auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(body_json(g0).await["configured"], false);

        // Grant a budget.
        let set = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/tenants/t1/budget")
                    .header("authorization", &auth)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"limit_micros":1000000,"period_secs":2592000}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(set.status(), StatusCode::OK);
        let sj = body_json(set).await;
        assert_eq!(sj["configured"], true);
        assert_eq!(sj["limit_micros"], 1_000_000);
        assert_eq!(sj["remaining_micros"], 1_000_000);

        // Clear → unset again.
        let del = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/tenants/t1/budget")
                    .header("authorization", &auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(del.status(), StatusCode::NO_CONTENT);
        let g1 = app
            .oneshot(
                Request::get("/tenants/t1/budget")
                    .header("authorization", &auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(body_json(g1).await["configured"], false);
    }

    #[tokio::test]
    async fn budget_requires_owner() {
        let (state, _, _) = state_with_owner("t1");
        let (outsider, _) = login_outsider(&state, "outsider");
        let resp = build_router(state)
            .oneshot(
                Request::get("/tenants/t1/budget")
                    .header("authorization", format!("Bearer {outsider}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
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
