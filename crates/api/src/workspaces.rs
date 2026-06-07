//! Workspace management endpoints (PRD §6, WSP-009) — tenant-scoped CRUD over
//! the [`WorkspaceRepository`].
//!
//! Workspaces are created **explicitly** (WSP-009): a tenant member with
//! `ManageSettings` names a workspace; platform sources are attached separately
//! (WSP-008, a later seam). Authorization is the caller's tenant membership
//! (reusing [`crate::tenancy::authorize`]), not the access token's workspace
//! claim — that claim grants the *data-plane* routes (summaries/schedules) and
//! is minted at login, so a freshly-created workspace needs a new token before
//! its summaries are reachable (a documented limitation of static claims).

use crate::auth::{now_secs, AuthUser};
use crate::tenancy::{authorize, parse_tenant};
use crate::{ApiError, AppState};
use axum::extract::{Path, State};
use axum::Json;
use domain::{Permission, Workspace, WorkspaceId};
use repository::WorkspaceRepository;
use serde::{Deserialize, Serialize};

/// JSON shape of a workspace.
#[derive(Serialize)]
pub struct WorkspaceDto {
    pub id: String,
    pub tenant_id: String,
    pub name: String,
    pub owner_user_id: String,
    pub created_at: i64,
}

impl From<Workspace> for WorkspaceDto {
    fn from(w: Workspace) -> Self {
        WorkspaceDto {
            id: w.id.as_str().to_string(),
            tenant_id: w.tenant_id.as_str().to_string(),
            name: w.name,
            owner_user_id: w.owner_user_id.as_str().to_string(),
            created_at: w.created_at,
        }
    }
}

/// Body for creating a workspace.
#[derive(Deserialize)]
pub struct CreateWorkspaceRequest {
    pub id: String,
    pub name: String,
}

/// `POST /tenants/:tenant/workspaces` — explicitly create a workspace (WSP-009).
/// Requires `ManageSettings`; the caller becomes the workspace owner. The id is
/// a global key, so a collision (even cross-tenant) is a 409.
pub async fn create_workspace(
    State(state): State<AppState>,
    user: AuthUser,
    Path(tenant): Path<String>,
    Json(body): Json<CreateWorkspaceRequest>,
) -> Result<Json<WorkspaceDto>, ApiError> {
    let tenant = parse_tenant(tenant)?;
    let id = WorkspaceId::parse(body.id).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let workspace = Workspace::create(
        id.clone(),
        tenant.clone(),
        body.name,
        user.0.sub.clone(),
        now_secs(),
    )
    .map_err(|e| ApiError::bad_request(e.to_string()))?;

    let repo = state.repo.lock().expect("repo mutex");
    authorize(&repo, &user.0.sub, &tenant, Permission::ManageSettings)?;
    if repo.find_workspace(&id)?.is_some() {
        return Err(ApiError::Conflict(format!(
            "workspace {} already exists",
            id.as_str()
        )));
    }
    repo.create_workspace(&workspace)?;
    Ok(Json(WorkspaceDto::from(workspace)))
}

/// `GET /tenants/:tenant/workspaces` — list a tenant's workspaces (any member).
pub async fn list_workspaces(
    State(state): State<AppState>,
    user: AuthUser,
    Path(tenant): Path<String>,
) -> Result<Json<Vec<WorkspaceDto>>, ApiError> {
    let tenant = parse_tenant(tenant)?;
    let repo = state.repo.lock().expect("repo mutex");
    authorize(&repo, &user.0.sub, &tenant, Permission::ViewSummaries)?;
    let workspaces = repo.list_workspaces(&tenant)?;
    Ok(Json(
        workspaces.into_iter().map(WorkspaceDto::from).collect(),
    ))
}

/// `GET /tenants/:tenant/workspaces/:ws` — workspace detail, tenant-scoped.
pub async fn get_workspace(
    State(state): State<AppState>,
    user: AuthUser,
    Path((tenant, ws)): Path<(String, String)>,
) -> Result<Json<WorkspaceDto>, ApiError> {
    let tenant = parse_tenant(tenant)?;
    let ws = WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let repo = state.repo.lock().expect("repo mutex");
    authorize(&repo, &user.0.sub, &tenant, Permission::ViewSummaries)?;
    let workspace = repo
        .get_workspace(&tenant, &ws)?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(WorkspaceDto::from(workspace)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use domain::{DiscordProvider, ProviderClaims, Role, Secret, TenantId};
    use host::AuthService;
    use repository::{MembershipRepository, SqliteRepository};
    use tower::ServiceExt;

    /// State where `subject` is an Owner of `tenant`; returns the owner's token.
    fn state_with_owner(tenant: &str, subject: &str) -> (AppState, String) {
        let repo = SqliteRepository::in_memory().unwrap();
        let key = Secret::new(b"ws-test-key".to_vec());
        let pair = {
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
        };
        repo.upsert_membership(&domain::Membership::new(
            TenantId::parse(tenant).unwrap(),
            pair.user_id.clone(),
            Role::Owner,
        ))
        .unwrap();
        (AppState::new(repo, key), pair.access_token)
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    }

    #[tokio::test]
    async fn create_then_list_and_detail() {
        let (state, token) = state_with_owner("t1", "owner");
        let app = build_router(state);
        let auth = format!("Bearer {token}");

        let created = app
            .clone()
            .oneshot(
                Request::post("/tenants/t1/workspaces")
                    .header("authorization", &auth)
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"id":"ws-eng","name":"Engineering"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::OK);
        let cj = body_json(created).await;
        assert_eq!(cj["id"], "ws-eng");
        assert_eq!(cj["tenant_id"], "t1");
        assert!(!cj["owner_user_id"].as_str().unwrap().is_empty());

        // List shows it.
        let listed = app
            .clone()
            .oneshot(
                Request::get("/tenants/t1/workspaces")
                    .header("authorization", &auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(body_json(listed).await.as_array().unwrap().len(), 1);

        // Detail by id.
        let detail = app
            .oneshot(
                Request::get("/tenants/t1/workspaces/ws-eng")
                    .header("authorization", &auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(detail.status(), StatusCode::OK);
        assert_eq!(body_json(detail).await["name"], "Engineering");
    }

    #[tokio::test]
    async fn create_rejects_duplicate_id() {
        let (state, token) = state_with_owner("t1", "owner");
        let app = build_router(state);
        let auth = format!("Bearer {token}");
        let mk = || {
            Request::post("/tenants/t1/workspaces")
                .header("authorization", &auth)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"id":"ws-dup","name":"X"}"#))
                .unwrap()
        };
        assert_eq!(
            app.clone().oneshot(mk()).await.unwrap().status(),
            StatusCode::OK
        );
        assert_eq!(
            app.oneshot(mk()).await.unwrap().status(),
            StatusCode::CONFLICT
        );
    }

    #[tokio::test]
    async fn non_member_cannot_create_or_list() {
        let (state, _owner) = state_with_owner("t1", "owner");
        // A second user with no membership in t1.
        let outsider = {
            let repo = state.repo.lock().unwrap();
            let svc = AuthService::new(&*repo, &state.signing_key);
            svc.login(
                &DiscordProvider,
                &ProviderClaims {
                    subject: "outsider".into(),
                    email: None,
                },
                vec![],
                now_secs(),
            )
            .unwrap()
            .access_token
        };
        let app = build_router(state);

        let create = app
            .clone()
            .oneshot(
                Request::post("/tenants/t1/workspaces")
                    .header("authorization", format!("Bearer {outsider}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"id":"ws-x","name":"X"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create.status(), StatusCode::FORBIDDEN);

        let list = app
            .oneshot(
                Request::get("/tenants/t1/workspaces")
                    .header("authorization", format!("Bearer {outsider}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn detail_404_for_unknown_workspace() {
        let (state, token) = state_with_owner("t1", "owner");
        let resp = build_router(state)
            .oneshot(
                Request::get("/tenants/t1/workspaces/ghost")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
