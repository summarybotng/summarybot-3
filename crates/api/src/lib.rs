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
mod schedules;
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
        .route(
            "/workspaces/:ws/summaries",
            get(summaries::list_summaries).post(summaries::create_summary),
        )
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
        .route(
            "/workspaces/:ws/schedules",
            get(schedules::list_schedules).post(schedules::create_schedule),
        )
        .route(
            "/workspaces/:ws/schedules/:id",
            get(schedules::get_schedule).delete(schedules::delete_schedule),
        )
        .route(
            "/workspaces/:ws/schedules/:id/pause",
            post(schedules::pause),
        )
        .route(
            "/workspaces/:ws/schedules/:id/resume",
            post(schedules::resume),
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
            "/workspaces/{ws}/summaries": {
                "get": { "summary": "List summaries" },
                "post": { "summary": "Create a summary (demo: deterministic, no LLM)" }
            },
            "/workspaces/{ws}/summaries/{id}": { "get": { "summary": "Summary detail" } },
            "/workspaces/{ws}/summaries/{id}/pin": { "post": { "summary": "Pin" } },
            "/workspaces/{ws}/summaries/{id}/archive": { "post": { "summary": "Archive" } },
            "/workspaces/{ws}/summaries/{id}/tags": { "put": { "summary": "Set tags" } },
            "/workspaces/{ws}/schedules": {
                "get": { "summary": "List schedules" },
                "post": { "summary": "Create a schedule" }
            },
            "/workspaces/{ws}/schedules/{id}": {
                "get": { "summary": "Schedule detail" },
                "delete": { "summary": "Delete schedule" }
            },
            "/workspaces/{ws}/schedules/{id}/pause": { "post": { "summary": "Pause" } },
            "/workspaces/{ws}/schedules/{id}/resume": { "post": { "summary": "Resume" } }
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
    async fn create_summary_then_it_appears_in_the_list() {
        let (state, token) = seeded_state(); // already has sum_1
        let app = build_router(state);
        let created = app
            .clone()
            .oneshot(
                Request::post("/workspaces/ws-1/summaries")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"messages":["hello team","let's ship on friday","sounds good"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::OK);
        let created_json = body_json(created).await;
        // Real pipeline output: structured, via the demo client.
        assert_eq!(created_json["model"], "demo");
        assert!(!created_json["key_points"].as_array().unwrap().is_empty());

        // Now two summaries are listed.
        let listed = app
            .oneshot(
                Request::get("/workspaces/ws-1/summaries")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(body_json(listed).await.as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn create_rejects_empty_messages() {
        let (state, token) = seeded_state();
        let resp = build_router(state)
            .oneshot(
                Request::post("/workspaces/ws-1/summaries")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"messages":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn schedule_create_list_pause_delete() {
        let (state, token) = seeded_state();
        let app = build_router(state);
        let auth = format!("Bearer {token}");

        // Create a daily 09:00 UTC schedule.
        let created = app
            .clone()
            .oneshot(
                Request::post("/workspaces/ws-1/schedules")
                    .header("authorization", &auth)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"schedule_type":"daily","hour":9,"minute":0,"timezone":"UTC"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::OK);
        let cj = body_json(created).await;
        let id = cj["id"].as_str().unwrap().to_string();
        assert_eq!(cj["schedule_type"], "daily");
        assert!(cj["next_run"].as_i64().unwrap() > 0);
        assert_eq!(cj["enabled"], true);

        // Pause → disabled.
        let paused = app
            .clone()
            .oneshot(
                Request::post(format!("/workspaces/ws-1/schedules/{id}/pause"))
                    .header("authorization", &auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(body_json(paused).await["enabled"], false);

        // List shows it.
        let listed = app
            .clone()
            .oneshot(
                Request::get("/workspaces/ws-1/schedules")
                    .header("authorization", &auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(body_json(listed).await.as_array().unwrap().len(), 1);

        // Delete → 204, then 404 on re-delete.
        let deleted = app
            .clone()
            .oneshot(
                Request::delete(format!("/workspaces/ws-1/schedules/{id}"))
                    .header("authorization", &auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn schedule_create_rejects_bad_timezone() {
        let (state, token) = seeded_state();
        let resp = build_router(state)
            .oneshot(
                Request::post("/workspaces/ws-1/schedules")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"schedule_type":"daily","hour":9,"timezone":"Mars/Olympus"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
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
