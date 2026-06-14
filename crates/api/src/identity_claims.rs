//! Contested identity claim/transfer endpoints (WSP-011..014; ADR-119).
//!
//! When an identity is already bound to a different user (`resolve_link` →
//! `Collision`), a claimant opens a **claim**. The pure
//! [`domain::decide_claim_route`] policy decides who adjudicates: a same-tenant
//! dispute goes to a tenant admin, a cross-tenant one to a platform operator.
//! Approval rebinds the identity to the claimant; every action is audited.

use crate::auth::{now_secs, AuthUser};
use crate::{ApiError, AppState};
use axum::extract::{Path, State};
use axum::Json;
use domain::{
    decide_claim_route, ClaimRoute, ClaimStatus, Permission, ProviderKind, Subject, TenantId,
    UserId,
};
use repository::{
    IdentityClaim, IdentityClaimRepository, IdentityRepository, MembershipRepository,
    SqliteRepository,
};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct OpenClaimRequest {
    pub provider: String,
    pub subject: String,
}

#[derive(Serialize)]
pub struct ClaimDto {
    pub id: String,
    pub provider: String,
    pub subject: String,
    pub claimant: String,
    pub current_owner: String,
    pub route: String,
    pub status: String,
    pub created_at: i64,
}

impl From<IdentityClaim> for ClaimDto {
    fn from(c: IdentityClaim) -> Self {
        ClaimDto {
            id: c.id,
            provider: c.provider,
            subject: c.subject,
            claimant: c.claimant,
            current_owner: c.current_owner,
            route: c.route,
            status: c.status.as_str().to_string(),
            created_at: c.created_at,
        }
    }
}

fn provider_kind(raw: &str) -> Result<ProviderKind, ApiError> {
    ProviderKind::parse(raw).map_err(|_| ApiError::bad_request(format!("unknown provider: {raw}")))
}

/// Tenants both users belong to — the candidate adjudicating tenants (WSP-013).
fn shared_tenants(repo: &SqliteRepository, a: &UserId, b: &UserId) -> Vec<TenantId> {
    let a_ids: std::collections::HashSet<String> = repo
        .list_tenants_for_user(a)
        .unwrap_or_default()
        .into_iter()
        .map(|(m, _)| m.tenant_id.as_str().to_string())
        .collect();
    repo.list_tenants_for_user(b)
        .unwrap_or_default()
        .into_iter()
        .map(|(m, _)| m.tenant_id)
        .filter(|t| a_ids.contains(t.as_str()))
        .collect()
}

/// Whether `user` may approve `claim`: a platform operator for a cross-tenant
/// (operator-route) claim, or a tenant admin of a tenant shared by the claimant
/// and the current owner for a same-tenant (tenant_admin-route) claim.
fn can_approve(state: &AppState, repo: &SqliteRepository, user: &UserId, claim: &IdentityClaim) -> bool {
    match claim.route.as_str() {
        "operator" => state.is_operator(user),
        "tenant_admin" => {
            let (Ok(claimant), Ok(owner)) =
                (UserId::parse(&claim.claimant), UserId::parse(&claim.current_owner))
            else {
                return false;
            };
            shared_tenants(repo, &claimant, &owner)
                .iter()
                .any(|t| crate::tenancy::authorize(repo, user, t, Permission::ManageSettings).is_ok())
        }
        _ => false,
    }
}

/// `POST /identity/claims` — open a claim on an identity bound to someone else
/// (WSP-011). The route is decided from whether the claimant and current owner
/// share a tenant. Errors clearly if the identity is unbound or already yours.
pub async fn open_claim(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<OpenClaimRequest>,
) -> Result<Json<ClaimDto>, ApiError> {
    let provider = provider_kind(&body.provider)?;
    let subject = Subject::parse(&body.subject).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let repo = state.repo.lock().expect("repo mutex");
    let owner = repo
        .find_user(provider, &subject)?
        .ok_or_else(|| ApiError::bad_request("that identity isn't bound to anyone — just sign in"))?;
    if owner == user.0.sub {
        return Err(ApiError::bad_request("that identity is already yours"));
    }
    // Manual claim ⇒ not self-verified (that's the OAuth re-auth path, WSP-012).
    let route = decide_claim_route(false, !shared_tenants(&repo, &user.0.sub, &owner).is_empty());
    let route_str = match route {
        ClaimRoute::SelfService => "self_service",
        ClaimRoute::TenantAdmin => "tenant_admin",
        ClaimRoute::Operator => "operator",
    };
    let claim = IdentityClaim {
        id: format!("clm_{}_{}", now_secs(), &subject.as_str().chars().take(8).collect::<String>()),
        provider: provider.as_str().to_string(),
        subject: subject.as_str().to_string(),
        claimant: user.0.sub.as_str().to_string(),
        current_owner: owner.as_str().to_string(),
        route: route_str.to_string(),
        status: ClaimStatus::Pending,
        created_at: now_secs(),
    };
    repo.open_claim(&claim)?;
    crate::tenancy::audit(&repo, &user.0.sub, "identity.claim.opened", format!("{}:{}", claim.provider, claim.subject));
    Ok(Json(claim.into()))
}

/// `GET /identity/claims` — pending claims the caller is authorized to adjudicate.
pub async fn list_claims(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<ClaimDto>>, ApiError> {
    let repo = state.repo.lock().expect("repo mutex");
    let out = repo
        .list_pending_claims()?
        .into_iter()
        .filter(|c| can_approve(&state, &repo, &user.0.sub, c))
        .map(ClaimDto::from)
        .collect();
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct ResolveRequest {
    /// `true` to approve (transfer the identity), `false` to deny.
    pub approve: bool,
}

/// `POST /identity/claims/:id/resolve` `{approve}` — adjudicate a claim. Approval
/// rebinds the identity to the claimant (WSP-013); both outcomes are audited.
pub async fn resolve_claim(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(body): Json<ResolveRequest>,
) -> Result<Json<ClaimDto>, ApiError> {
    let repo = state.repo.lock().expect("repo mutex");
    let claim = repo.get_claim(&id)?.ok_or(ApiError::NotFound)?;
    if claim.status != ClaimStatus::Pending {
        return Err(ApiError::bad_request("claim is already resolved"));
    }
    // Authority is invisible to the unauthorized: a non-adjudicator gets 404.
    if !can_approve(&state, &repo, &user.0.sub, &claim) {
        return Err(ApiError::NotFound);
    }
    let status = if body.approve {
        let provider = provider_kind(&claim.provider)?;
        let subject = Subject::parse(&claim.subject).map_err(|e| ApiError::bad_request(e.to_string()))?;
        let claimant = UserId::parse(&claim.claimant).map_err(|e| ApiError::bad_request(e.to_string()))?;
        repo.rebind_identity(provider, &subject, &claimant, now_secs())?;
        ClaimStatus::Approved
    } else {
        ClaimStatus::Denied
    };
    repo.set_claim_status(&id, status)?;
    crate::tenancy::audit(
        &repo,
        &user.0.sub,
        if body.approve { "identity.claim.approved" } else { "identity.claim.denied" },
        format!("{}:{} → {}", claim.provider, claim.subject, claim.claimant),
    );
    let mut updated = claim;
    updated.status = status;
    Ok(Json(updated.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{build_router, AppState};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use domain::{DiscordProvider, IdentityLink, ProviderClaims, Secret};
    use host::AuthService;
    use repository::SqliteRepository;
    use tower::ServiceExt;

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    }

    #[tokio::test]
    async fn cross_tenant_claim_is_approved_by_operator_and_rebinds_the_identity() {
        let repo = SqliteRepository::in_memory().unwrap();
        let key = Secret::new(b"claim-test-key".to_vec());
        let login = |subject: &str| {
            AuthService::new(&repo, &key)
                .login(
                    &DiscordProvider,
                    &ProviderClaims { subject: subject.into(), email: None },
                    vec![],
                    now_secs(),
                )
                .unwrap()
        };
        let owner = login("owner-user");
        let claimant = login("claimant-user");
        let op = login("op-user");
        // The contested identity is currently bound to the owner.
        repo.link_identity(
            &IdentityLink {
                provider: ProviderKind::Discord,
                subject: Subject::parse("contested#1").unwrap(),
                user_id: UserId::parse(owner.user_id.as_str()).unwrap(),
            },
            1,
        )
        .unwrap();
        let state = AppState::new(repo, key).with_operators([op.user_id.as_str().to_string()]);
        let app = build_router(state.clone());

        // Claimant opens a claim → cross-tenant ⇒ operator route.
        let resp = app
            .clone()
            .oneshot(
                Request::post("/identity/claims")
                    .header("authorization", format!("Bearer {}", claimant.access_token))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"provider":"discord","subject":"contested#1"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let j = body_json(resp).await;
        assert_eq!(j["route"], "operator");
        let claim_id = j["id"].as_str().unwrap().to_string();

        // A non-operator can't see or resolve it (404).
        let resp = app
            .clone()
            .oneshot(
                Request::post(format!("/identity/claims/{claim_id}/resolve"))
                    .header("authorization", format!("Bearer {}", claimant.access_token))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"approve":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        // The operator approves → the identity rebinds to the claimant.
        let resp = app
            .clone()
            .oneshot(
                Request::post(format!("/identity/claims/{claim_id}/resolve"))
                    .header("authorization", format!("Bearer {}", op.access_token))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"approve":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await["status"], "approved");

        let repo = state.repo.lock().unwrap();
        let bound = repo
            .find_user(ProviderKind::Discord, &Subject::parse("contested#1").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(bound.as_str(), claimant.user_id.as_str());
    }
}
