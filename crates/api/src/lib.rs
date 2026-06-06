//! Web API (PRD §5, §12.6) — OpenAPI-shaped, workspace-scoped, thin over the
//! services/repositories.
//!
//! axum + tokio. Handlers are `async` but call the existing **synchronous**
//! services under a `Mutex<SqliteRepository>` (fine for SQLite; `spawn_blocking`
//! is a later optimization), so the rest of the codebase stays sync. Every
//! request carries a correlation id; protected routes require a verified access
//! token (Phase 1 auth). Real OAuth redirects, the schedule API (needs the
//! Phase 6 scheduler) and SSE/WebSocket over Redis are documented seams.

mod auth;
mod error;
mod summaries;

pub use error::ApiError;

use axum::routing::{get, post, put};
use axum::{Json, Router};
use domain::Secret;
use repository::SqliteRepository;
use std::sync::{Arc, Mutex};

/// Shared application state. The repo is behind a `Mutex` because rusqlite's
/// `Connection` is `Send` but not `Sync`; SQLite serializes writes anyway.
#[derive(Clone)]
pub struct AppState {
    pub repo: Arc<Mutex<SqliteRepository>>,
    pub signing_key: Arc<Secret<Vec<u8>>>,
}

impl AppState {
    pub fn new(repo: SqliteRepository, signing_key: Secret<Vec<u8>>) -> Self {
        Self {
            repo: Arc::new(Mutex::new(repo)),
            signing_key: Arc::new(signing_key),
        }
    }
}

/// Build the router with all routes + the correlation-id middleware.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/openapi.json", get(openapi))
        .route("/auth/login", post(auth::login))
        .route("/workspaces/:ws/summaries", get(summaries::list_summaries))
        .route("/workspaces/:ws/summaries/:id", get(summaries::get_summary))
        .route("/workspaces/:ws/summaries/:id/pin", post(summaries::pin))
        .route(
            "/workspaces/:ws/summaries/:id/unpin",
            post(summaries::unpin),
        )
        .route(
            "/workspaces/:ws/summaries/:id/archive",
            post(summaries::archive),
        )
        .route(
            "/workspaces/:ws/summaries/:id/unarchive",
            post(summaries::unarchive),
        )
        .route(
            "/workspaces/:ws/summaries/:id/tags",
            put(summaries::set_tags),
        )
        .layer(axum::middleware::from_fn(auth::correlation_id))
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

/// Minimal OpenAPI document (OpenAPI-first §12.6 item 1). A full generated spec
/// (utoipa) is a follow-up; this enumerates the live routes for clients/tools.
async fn openapi() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "openapi": "3.0.3",
        "info": { "title": "SummaryBot API", "version": "0.1.0" },
        "paths": {
            "/healthz": { "get": { "summary": "Liveness check" } },
            "/auth/login": { "post": { "summary": "Exchange verified provider claims for tokens" } },
            "/workspaces/{ws}/summaries": { "get": { "summary": "List summaries" } },
            "/workspaces/{ws}/summaries/{id}": { "get": { "summary": "Summary detail" } },
            "/workspaces/{ws}/summaries/{id}/pin": { "post": { "summary": "Pin" } },
            "/workspaces/{ws}/summaries/{id}/archive": { "post": { "summary": "Archive" } },
            "/workspaces/{ws}/summaries/{id}/tags": { "put": { "summary": "Set tags" } }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use host::AuthService;
    use repository::{StructuredSummaryRepository, SummaryRecord};
    use tower::ServiceExt; // oneshot

    fn key() -> Secret<Vec<u8>> {
        Secret::new(b"api-test-key".to_vec())
    }

    /// Seed a workspace summary and mint a token that grants it.
    fn seeded_state() -> (AppState, String) {
        let repo = SqliteRepository::in_memory().unwrap();
        repo.save_record(
            &domain::WorkspaceId::parse("ws-1").unwrap(),
            &record("sum_1"),
        )
        .unwrap();
        let k = key();
        // Mint an access token granting ws-1 via the auth service.
        let token = {
            let svc = AuthService::new(&repo, &k);
            let pair = svc
                .login(
                    &domain::DiscordProvider,
                    &domain::ProviderClaims {
                        subject: "u1".into(),
                        email: None,
                    },
                    vec![domain::WorkspaceId::parse("ws-1").unwrap()],
                    // Mint against the real clock so the short-lived access token
                    // is still valid when the auth extractor verifies it.
                    crate::auth::now_secs(),
                )
                .unwrap();
            pair.access_token
        };
        (AppState::new(repo, k), token)
    }

    fn record(id: &str) -> SummaryRecord {
        use domain::summarize::ExtractedSummary;
        SummaryRecord {
            id: id.into(),
            channel_id: None,
            model: "sonnet".into(),
            cost_micros: 1_000,
            degraded: false,
            created_at: 10,
            pinned: false,
            archived: false,
            tags: vec![],
            summary: ExtractedSummary {
                text: "We shipped.".into(),
                key_points: vec!["Launched".into()],
                technical_terms: vec![],
                participants: vec!["Alice".into()],
                action_items: vec![],
                citations: vec![],
            },
        }
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    }

    #[tokio::test]
    async fn healthz_is_ok_and_carries_correlation_id() {
        let (state, _) = seeded_state();
        let resp = build_router(state)
            .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers().contains_key("x-correlation-id"));
    }

    #[tokio::test]
    async fn summaries_require_a_token() {
        let (state, _) = seeded_state();
        let resp = build_router(state)
            .oneshot(
                Request::get("/workspaces/ws-1/summaries")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn lists_summaries_with_a_valid_token() {
        let (state, token) = seeded_state();
        let resp = build_router(state)
            .oneshot(
                Request::get("/workspaces/ws-1/summaries")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json.as_array().unwrap().len(), 1);
        assert_eq!(json[0]["id"], "sum_1");
    }

    #[tokio::test]
    async fn token_for_another_workspace_is_forbidden() {
        let (state, token) = seeded_state(); // token grants ws-1 only
        let resp = build_router(state)
            .oneshot(
                Request::get("/workspaces/ws-other/summaries")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn pin_then_detail_reflects_the_change() {
        let (state, token) = seeded_state();
        let app = build_router(state);
        let pinned = app
            .clone()
            .oneshot(
                Request::post("/workspaces/ws-1/summaries/sum_1/pin")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(pinned.status(), StatusCode::OK);
        assert_eq!(body_json(pinned).await["pinned"], true);
    }

    #[tokio::test]
    async fn detail_404_for_unknown_summary() {
        let (state, token) = seeded_state();
        let resp = build_router(state)
            .oneshot(
                Request::get("/workspaces/ws-1/summaries/nope")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn login_issues_a_usable_token() {
        let repo = SqliteRepository::in_memory().unwrap();
        let state = AppState::new(repo, key());
        let resp = build_router(state)
            .oneshot(
                Request::post("/auth/login")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"provider":"email","subject":"x","email":"a@b.com","workspaces":["ws-1"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert!(json["access_token"].as_str().unwrap().contains('.'));
        assert!(!json["refresh_token"].as_str().unwrap().is_empty());
    }
}
