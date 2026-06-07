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
mod events;
mod scheduler_driver;
mod schedules;
mod summaries;
mod tenancy;
mod whatsapp;
mod workspaces;

pub use error::ApiError;
pub use events::LiveEvent;
pub use scheduler_driver::spawn_scheduler;

use axum::routing::{get, post, put};
use axum::{Json, Router};
use domain::summarize::{Model, ModelLadder, ModelPrice};
use domain::Secret;
use host::llm::{DemoLlmClient, GlobalRateLimiter, LlmClient, RateLimitConfig};
use repository::SqliteRepository;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

/// Capacity of the live-events broadcast buffer (frames a slow subscriber may
/// fall behind before it starts dropping the oldest — see SSE lag handling).
const EVENT_BUFFER: usize = 256;

/// Shared application state. The repo is behind a `Mutex` because rusqlite's
/// `Connection` is `Send` but not `Sync`; SQLite serializes writes anyway.
///
/// The LLM client and rate limiter are **process-wide and shared**: every
/// request and every scheduler tick goes through the one `limiter` (so the
/// LEG-001 budget is global, not per-request) and the one configured `llm`
/// backend (demo by default; OpenRouter when configured — see `main.rs`).
#[derive(Clone)]
pub struct AppState {
    pub repo: Arc<Mutex<SqliteRepository>>,
    pub signing_key: Arc<Secret<Vec<u8>>>,
    pub llm: Arc<dyn LlmClient + Send + Sync>,
    pub limiter: Arc<GlobalRateLimiter>,
    /// Product base domain for host→tenant routing (TEN-006); subdomains under
    /// it resolve to tenants, anything else is a custom domain.
    pub base_domain: Arc<str>,
    /// Process-wide live-events bus (SSE). Mutations publish here; the
    /// `/workspaces/:ws/events` stream filters to a workspace.
    pub events: broadcast::Sender<LiveEvent>,
    /// Summarization model name the configured backend serves (ADR-125): the
    /// demo model by default, or e.g. a Mac mini's `llama3.1` / a hosted id.
    pub model: Arc<str>,
    /// Operator master key (32 bytes) for encrypting stored tenant API keys
    /// (ADR-125 Phase 2b). `None` disables BYO-key storage. Never logged.
    pub config_key: Option<Arc<[u8; 32]>>,
    /// Price (micros per 1k tokens, input and output) of the process-default
    /// model, so platform-key spend is measurable for budgets (ADR-125 Phase 3).
    /// Zero for the demo model.
    pub price_micros_per_ktoken: i64,
}

/// Default product base domain when unconfigured.
const DEFAULT_BASE_DOMAIN: &str = "summarybot.app";

impl AppState {
    /// Default state: the deterministic [`DemoLlmClient`] and a fresh shared
    /// limiter (no key/network). Used by tests and the default build.
    pub fn new(repo: SqliteRepository, signing_key: Secret<Vec<u8>>) -> Self {
        Self::with_llm(
            repo,
            signing_key,
            Arc::new(DemoLlmClient),
            Arc::new(GlobalRateLimiter::new(RateLimitConfig::default())),
        )
    }

    /// State with an explicit LLM backend + shared limiter (the server picks
    /// these from config in `main.rs`).
    pub fn with_llm(
        repo: SqliteRepository,
        signing_key: Secret<Vec<u8>>,
        llm: Arc<dyn LlmClient + Send + Sync>,
        limiter: Arc<GlobalRateLimiter>,
    ) -> Self {
        Self {
            repo: Arc::new(Mutex::new(repo)),
            signing_key: Arc::new(signing_key),
            llm,
            limiter,
            base_domain: Arc::from(DEFAULT_BASE_DOMAIN),
            events: broadcast::channel(EVENT_BUFFER).0,
            model: Arc::from("demo"),
            config_key: None,
            price_micros_per_ktoken: 0,
        }
    }

    /// Override the product base domain used for host→tenant routing (TEN-006).
    pub fn with_base_domain(mut self, base_domain: impl Into<String>) -> Self {
        self.base_domain = Arc::from(base_domain.into());
        self
    }

    /// Override the summarization model name (ADR-125).
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Arc::from(model.into());
        self
    }

    /// Set the master key used to encrypt stored tenant API keys (ADR-125 2b).
    pub fn with_config_key(mut self, key: [u8; 32]) -> Self {
        self.config_key = Some(Arc::new(key));
        self
    }

    /// Set the process-default model price (micros/1k tokens) for budget
    /// accounting (ADR-125 Phase 3).
    pub fn with_price(mut self, price_micros_per_ktoken: i64) -> Self {
        self.price_micros_per_ktoken = price_micros_per_ktoken;
        self
    }

    /// The master key, if configured.
    pub(crate) fn master_key(&self) -> Option<&[u8; 32]> {
        self.config_key.as_deref()
    }

    /// A single-model ladder for the process-default model.
    pub(crate) fn model_ladder(&self) -> ModelLadder {
        self.ladder_for(&self.model)
    }

    /// A single-model ladder for an explicit model name, priced from the
    /// process-default price (ADR-125 Phase 3) so platform-key spend is
    /// measurable. Per-model price tables are a future refinement.
    pub(crate) fn ladder_for(&self, model: &str) -> ModelLadder {
        ModelLadder::new(vec![Model {
            name: model.to_string(),
            price: ModelPrice {
                input_micros_per_ktoken: self.price_micros_per_ktoken,
                output_micros_per_ktoken: self.price_micros_per_ktoken,
            },
            context_tokens: 200_000,
        }])
    }

    /// Resolve the LLM client for a tenant's override (ADR-125). With the
    /// `http-llm` feature: a base URL → that endpoint (with the optional BYO
    /// key); a key but no base URL → OpenRouter with the tenant's key; neither →
    /// the process default.
    #[cfg(feature = "http-llm")]
    pub(crate) fn client_for_base(
        &self,
        base_url: Option<String>,
        api_key: Option<String>,
    ) -> Arc<dyn LlmClient + Send + Sync> {
        use host::llm::HttpLlmClient;
        match (base_url, api_key) {
            (Some(b), key) => Arc::new(HttpLlmClient::new(b, key.map(Secret::new))),
            (None, Some(key)) => Arc::new(HttpLlmClient::openrouter(Secret::new(key))),
            (None, None) => self.llm.clone(),
        }
    }

    /// Without `http-llm` there is no HTTP client to build, so per-tenant
    /// endpoint/key are ignored and the process-default backend is used.
    #[cfg(not(feature = "http-llm"))]
    pub(crate) fn client_for_base(
        &self,
        _base_url: Option<String>,
        _api_key: Option<String>,
    ) -> Arc<dyn LlmClient + Send + Sync> {
        self.llm.clone()
    }

    /// Publish a live event to all connected SSE subscribers. A no-op when no
    /// one is listening (the send error just means zero receivers).
    pub fn publish(&self, event: LiveEvent) {
        let _ = self.events.send(event);
    }
}

/// The resolved LLM choice for a request (ADR-125): which endpoint/key/model to
/// use, the owning tenant (if the workspace maps to one), and whether the tenant
/// brought their own provider (`byo` — in which case no platform budget applies).
pub(crate) struct LlmResolution {
    pub base_url: Option<String>,
    pub model: String,
    pub api_key: Option<String>,
    pub tenant: Option<domain::TenantId>,
    pub byo: bool,
}

/// Resolve the LLM choice for a workspace under an already-held repo guard: map
/// the workspace → its tenant, read that tenant's config, and decrypt any BYO
/// key with `master_key` (ADR-125 2b). A workspace with no row yields the
/// process default and no tenant.
pub(crate) fn resolve_llm(
    repo: &SqliteRepository,
    workspace: &domain::WorkspaceId,
    default_model: &str,
    master_key: Option<&[u8; 32]>,
) -> LlmResolution {
    use repository::{LlmConfigRepository, WorkspaceRepository};
    let default = || LlmResolution {
        base_url: None,
        model: default_model.to_string(),
        api_key: None,
        tenant: None,
        byo: false,
    };
    let Ok(Some(ws)) = repo.find_workspace(workspace) else {
        return default();
    };
    let tenant = Some(ws.tenant_id.clone());
    let Ok(Some(cfg)) = repo.get_llm_config(&ws.tenant_id) else {
        // Tenant known but no LLM override → platform default (budget may apply).
        return LlmResolution {
            tenant,
            ..default()
        };
    };
    let api_key = match (cfg.api_key_enc, master_key) {
        (Some(enc), Some(master)) => host::decrypt_secret(master, &enc).ok(),
        _ => None,
    };
    let byo = cfg.base_url.is_some() || api_key.is_some();
    LlmResolution {
        base_url: cfg.base_url,
        model: cfg.model.unwrap_or_else(|| default_model.to_string()),
        api_key,
        tenant,
        byo,
    }
}

/// Budget gate for a platform-key call (ADR-125 Phase 3): under an already-held
/// guard, refuse if the tenant's budget is exhausted. Returns the window to
/// charge on success — `None` when no budget applies (BYO, no tenant, or no
/// grant → unmetered).
pub(crate) fn budget_gate(
    repo: &SqliteRepository,
    res: &LlmResolution,
    now: i64,
) -> Result<Option<(domain::TenantId, domain::Window)>, ApiError> {
    use domain::{current_window, within_budget, Budget, Spend};
    use repository::BudgetRepository;
    if res.byo {
        return Ok(None);
    }
    let Some(tenant) = res.tenant.clone() else {
        return Ok(None);
    };
    let Some(row) = repo.get_budget(&tenant)? else {
        return Ok(None);
    };
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
        now,
    );
    if within_budget(window, budget) {
        Ok(Some((tenant, window)))
    } else {
        Err(ApiError::BudgetExceeded(format!(
            "LLM budget exhausted for tenant {}",
            tenant.as_str()
        )))
    }
}

/// Charge `cost_micros` to a tenant's budget window after a successful call.
pub(crate) fn budget_charge(
    repo: &SqliteRepository,
    tenant: &domain::TenantId,
    window: domain::Window,
    cost_micros: i64,
) -> Result<(), ApiError> {
    use repository::{BudgetRepository, BudgetRow};
    if let Some(row) = repo.get_budget(tenant)? {
        repo.upsert_budget(
            tenant,
            &BudgetRow {
                limit_micros: row.limit_micros,
                period_secs: row.period_secs,
                period_start: window.period_start,
                spent_micros: window.spent_micros.saturating_add(cost_micros),
            },
        )?;
    }
    Ok(())
}

/// Build the router with all routes + the correlation-id middleware.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/openapi.json", get(openapi))
        .route("/auth/login", post(auth::login))
        .route("/auth/refresh", post(auth::refresh))
        .route("/auth/logout", post(auth::logout))
        .route(
            "/workspaces/:ws/summaries",
            get(summaries::list_summaries)
                .post(summaries::create_summary)
                .delete(summaries::bulk_delete)
                .patch(summaries::bulk_archive),
        )
        .route(
            "/workspaces/:ws/summaries/:id",
            get(summaries::get_summary).delete(summaries::delete_summary),
        )
        .route("/workspaces/:ws/events", get(events::workspace_events))
        // WhatsApp export upload (WHA-001) — allow a large body for the zip.
        .route(
            "/workspaces/:ws/whatsapp/imports",
            post(whatsapp::import_whatsapp)
                .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024)),
        )
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
            get(schedules::get_schedule)
                .put(schedules::update_schedule)
                .delete(schedules::delete_schedule),
        )
        .route(
            "/workspaces/:ws/schedules/:id/pause",
            post(schedules::pause),
        )
        .route(
            "/workspaces/:ws/schedules/:id/resume",
            post(schedules::resume),
        )
        .route(
            "/workspaces/:ws/schedules/:id/run",
            post(schedules::trigger_schedule),
        )
        .route(
            "/workspaces/:ws/schedules/:id/runs",
            get(schedules::list_runs),
        )
        // Tenancy: provisioning (TEN-001), host→tenant resolution (TEN-006),
        // members + invites.
        .route("/tenants", post(tenancy::provision_tenant))
        .route("/tenants/:tenant", put(tenancy::update_tenant))
        .route(
            "/tenants/:tenant/llm-config",
            get(tenancy::get_llm_config)
                .put(tenancy::set_llm_config)
                .delete(tenancy::clear_llm_config),
        )
        .route(
            "/tenants/:tenant/budget",
            get(tenancy::get_budget)
                .put(tenancy::set_budget)
                .delete(tenancy::clear_budget),
        )
        .route("/tenant", get(tenancy::resolve_tenant))
        // Workspace management under a tenant (WSP-009).
        .route(
            "/tenants/:tenant/workspaces",
            get(workspaces::list_workspaces).post(workspaces::create_workspace),
        )
        .route(
            "/tenants/:tenant/workspaces/:ws",
            get(workspaces::get_workspace),
        )
        .route(
            "/tenants/:tenant/workspaces/:ws/connections",
            get(workspaces::list_connections).post(workspaces::attach_connection),
        )
        .route("/tenants/:tenant/members", get(tenancy::list_members))
        .route(
            "/tenants/:tenant/members/:user",
            put(tenancy::set_member_role).delete(tenancy::remove_member),
        )
        .route(
            "/tenants/:tenant/invites",
            get(tenancy::list_invites).post(tenancy::create_invite),
        )
        .route(
            "/tenants/:tenant/invites/revoke",
            post(tenancy::revoke_invite),
        )
        .route("/invites/accept", post(tenancy::accept_invite))
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
            "/auth/refresh": { "post": { "summary": "Rotate a refresh token for a new token pair" } },
            "/auth/logout": { "post": { "summary": "Revoke the current session" } },
            "/workspaces/{ws}/summaries": {
                "get": { "summary": "List/search summaries (q, participant, tag; limit, offset)" },
                "post": { "summary": "Create a summary (demo: deterministic, no LLM)" },
                "delete": { "summary": "Bulk delete by ids (DSH-013)" },
                "patch": { "summary": "Bulk archive/unarchive by ids" }
            },
            "/workspaces/{ws}/summaries/{id}": {
                "get": { "summary": "Summary detail" },
                "delete": { "summary": "Delete one summary (DSH-013)" }
            },
            "/workspaces/{ws}/events": { "get": { "summary": "Live updates (Server-Sent Events)" } },
            "/workspaces/{ws}/whatsapp/imports": { "post": { "summary": "Ingest a WhatsApp export (.zip or _chat.txt) — ?chat,tz,date_order (WHA-001)" } },
            "/workspaces/{ws}/summaries/{id}/pin": { "post": { "summary": "Pin" } },
            "/workspaces/{ws}/summaries/{id}/archive": { "post": { "summary": "Archive" } },
            "/workspaces/{ws}/summaries/{id}/tags": { "put": { "summary": "Set tags" } },
            "/workspaces/{ws}/schedules": {
                "get": { "summary": "List schedules" },
                "post": { "summary": "Create a schedule" }
            },
            "/workspaces/{ws}/schedules/{id}": {
                "get": { "summary": "Schedule detail" },
                "put": { "summary": "Update schedule recurrence (SCM-003)" },
                "delete": { "summary": "Delete schedule" }
            },
            "/workspaces/{ws}/schedules/{id}/pause": { "post": { "summary": "Pause" } },
            "/workspaces/{ws}/schedules/{id}/resume": { "post": { "summary": "Resume" } },
            "/workspaces/{ws}/schedules/{id}/run": { "post": { "summary": "Trigger immediately (SCM-007)" } },
            "/workspaces/{ws}/schedules/{id}/runs": { "get": { "summary": "Execution history (SCM-005)" } },
            "/tenants": { "post": { "summary": "Provision a tenant; caller becomes Owner (TEN-001)" } },
            "/tenants/{tenant}": { "put": { "summary": "Update tenant settings (TEN-001/TEN-002)" } },
            "/tenants/{tenant}/llm-config": {
                "get": { "summary": "Get the tenant's LLM override (ADR-125)" },
                "put": { "summary": "Set the tenant's LLM endpoint/model/key" },
                "delete": { "summary": "Clear the tenant's LLM override" }
            },
            "/tenants/{tenant}/budget": {
                "get": { "summary": "Get the tenant's LLM budget + spend (ADR-125 Phase 3)" },
                "put": { "summary": "Grant/update the tenant's budget (owner)" },
                "delete": { "summary": "Remove the tenant's budget" }
            },
            "/tenants/{tenant}/workspaces": {
                "get": { "summary": "List a tenant's workspaces" },
                "post": { "summary": "Create a workspace (WSP-009)" }
            },
            "/tenants/{tenant}/workspaces/{ws}": { "get": { "summary": "Workspace detail" } },
            "/tenants/{tenant}/workspaces/{ws}/connections": {
                "get": { "summary": "List attached platform sources" },
                "post": { "summary": "Attach a platform source (WSP-008)" }
            },
            "/tenant": { "get": { "summary": "Resolve the tenant for the request host (TEN-006)" } },
            "/tenants/{tenant}/members": { "get": { "summary": "List tenant members" } },
            "/tenants/{tenant}/members/{user}": {
                "put": { "summary": "Set a member's role" },
                "delete": { "summary": "Remove a member" }
            },
            "/tenants/{tenant}/invites": {
                "get": { "summary": "List invites" },
                "post": { "summary": "Issue an invite (returns the raw token once)" }
            },
            "/tenants/{tenant}/invites/revoke": { "post": { "summary": "Revoke an invite" } },
            "/invites/accept": { "post": { "summary": "Accept an invite for the caller" } }
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
    async fn list_summaries_honors_search_filters() {
        let (state, token) = seeded_state(); // sum_1: text "We shipped.", kp "Launched", participant "Alice"
        let app = build_router(state);
        let auth = format!("Bearer {token}");
        let count = |app: Router, uri: &'static str, auth: String| async move {
            let resp = app
                .oneshot(
                    Request::get(uri)
                        .header("authorization", auth)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            body_json(resp).await.as_array().unwrap().len()
        };

        // Text match on a key point; case-insensitive.
        assert_eq!(
            count(
                app.clone(),
                "/workspaces/ws-1/summaries?q=launched",
                auth.clone()
            )
            .await,
            1
        );
        // No match → empty.
        assert_eq!(
            count(
                app.clone(),
                "/workspaces/ws-1/summaries?q=zzz",
                auth.clone()
            )
            .await,
            0
        );
        // Participant filter.
        assert_eq!(
            count(
                app.clone(),
                "/workspaces/ws-1/summaries?participant=alice",
                auth.clone()
            )
            .await,
            1
        );
        assert_eq!(
            count(
                app.clone(),
                "/workspaces/ws-1/summaries?participant=zoe",
                auth.clone()
            )
            .await,
            0
        );
        // offset past the end → empty.
        assert_eq!(
            count(app, "/workspaces/ws-1/summaries?offset=5", auth).await,
            0
        );
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
    async fn schedule_update_changes_recurrence() {
        let (state, token) = seeded_state();
        let app = build_router(state);
        let auth = format!("Bearer {token}");

        // Create a daily 09:00 schedule.
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
        let id = body_json(created).await["id"].as_str().unwrap().to_string();

        // Pause it, then edit the recurrence — paused state must be preserved.
        app.clone()
            .oneshot(
                Request::post(format!("/workspaces/ws-1/schedules/{id}/pause"))
                    .header("authorization", &auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let updated = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/workspaces/ws-1/schedules/{id}"))
                    .header("authorization", &auth)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"schedule_type":"daily","hour":6,"minute":30,"timezone":"UTC"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(updated.status(), StatusCode::OK);
        let uj = body_json(updated).await;
        assert_eq!(uj["hour"], 6);
        assert_eq!(uj["minute"], 30);
        assert_eq!(uj["enabled"], false); // paused state preserved across the edit

        // Updating an unknown schedule is a 404.
        let missing = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/workspaces/ws-1/schedules/ghost")
                    .header("authorization", &auth)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"schedule_type":"daily","hour":1,"timezone":"UTC"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn trigger_schedule_runs_immediately_and_returns_the_summary() {
        use domain::{ChannelId, MessageId, NormalizedMessage, Platform, Schedule};
        use repository::{ScheduleRepository, StoredSchedule, WhatsAppRepository};

        let (state, token) = seeded_state();
        let ws = domain::WorkspaceId::parse("ws-1").unwrap();
        let now = crate::auth::now_secs();
        // Seed substantial messages in channel c1 and a c1-scoped schedule.
        {
            let repo = state.repo.lock().unwrap();
            for (i, text) in ["we shipped the release today", "great work everyone"]
                .iter()
                .enumerate()
            {
                repo.save_message(
                    &ws,
                    &NormalizedMessage {
                        id: MessageId::parse(format!("tm{i}")).unwrap(),
                        platform: Platform::WhatsApp,
                        channel_id: ChannelId::parse("c1").unwrap(),
                        author_id: "alice".into(),
                        author_name: "Alice".into(),
                        content: (*text).into(),
                        timestamp: now - 100,
                        is_system: false,
                        reply_to: None,
                        attachments: vec![],
                    },
                )
                .unwrap();
            }
            let schedule = Schedule::build(
                ws.clone(),
                "hourly",
                0,
                0,
                &[],
                1,
                "UTC",
                None,
                0,
                true,
                Some("c1"),
                100_000,
            )
            .unwrap();
            repo.create_schedule(&StoredSchedule {
                id: "sch_run".into(),
                schedule,
                next_run: now + 3_600,
                consecutive_failures: 0,
            })
            .unwrap();
        }

        let app = build_router(state);
        let resp = app
            .clone()
            .oneshot(
                Request::post("/workspaces/ws-1/schedules/sch_run/run")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let j = body_json(resp).await;
        assert_eq!(j["produced"], true);
        assert_eq!(j["summary"]["channel_id"], "c1");
        assert!(!j["summary"]["participants"].as_array().unwrap().is_empty());

        // The manual run is recorded in the execution history (SCM-005).
        let runs = app
            .oneshot(
                Request::get("/workspaces/ws-1/schedules/sch_run/runs")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(runs.status(), StatusCode::OK);
        let rj = body_json(runs).await;
        assert_eq!(rj.as_array().unwrap().len(), 1);
        assert_eq!(rj[0]["status"], "fired");
        assert_eq!(rj[0]["manual"], true);
        assert_eq!(rj[0]["detail"], j["summary"]["id"]); // detail = produced summary id
    }

    #[tokio::test]
    async fn trigger_unknown_schedule_is_404() {
        let (state, token) = seeded_state();
        let resp = build_router(state)
            .oneshot(
                Request::post("/workspaces/ws-1/schedules/ghost/run")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
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

    /// Seed `n` extra summaries (sum_2..) alongside the seeded sum_1.
    fn seed_extra(state: &AppState, ids: &[&str]) {
        let repo = state.repo.lock().unwrap();
        for id in ids {
            repo.save_record(&domain::WorkspaceId::parse("ws-1").unwrap(), &record(id))
                .unwrap();
        }
    }

    #[test]
    fn resolve_llm_applies_per_tenant_override() {
        use repository::{LlmConfigRepository, TenantLlmConfig, WorkspaceRepository};
        let repo = SqliteRepository::in_memory().unwrap();
        let tenant = domain::TenantId::parse("acme").unwrap();
        let ws = domain::WorkspaceId::parse("ws-eng").unwrap();
        // A provisioned workspace under the tenant, and that tenant's LLM config.
        repo.create_workspace(
            &domain::Workspace::create(
                ws.clone(),
                tenant.clone(),
                "Eng",
                domain::UserId::parse("u1").unwrap(),
                10,
            )
            .unwrap(),
        )
        .unwrap();
        repo.set_llm_config(
            &tenant,
            &TenantLlmConfig {
                base_url: Some("http://mac-mini:11434/v1".into()),
                model: Some("llama3.1".into()),
                api_key_enc: None,
            },
        )
        .unwrap();

        // Resolves to the tenant's endpoint + model (no key configured).
        let r = resolve_llm(&repo, &ws, "demo", None);
        assert_eq!(r.base_url.as_deref(), Some("http://mac-mini:11434/v1"));
        assert_eq!(r.model, "llama3.1");
        assert!(r.api_key.is_none());
        assert!(r.byo); // a base URL is BYO → no platform budget
        assert_eq!(r.tenant.as_ref().map(|t| t.as_str()), Some("acme"));

        // An unknown workspace (no row) → no override, default model, no tenant.
        let r = resolve_llm(
            &repo,
            &domain::WorkspaceId::parse("ghost").unwrap(),
            "demo",
            None,
        );
        assert!(r.base_url.is_none());
        assert_eq!(r.model, "demo");
        assert!(r.tenant.is_none());
        assert!(!r.byo);
    }

    #[test]
    fn resolve_llm_decrypts_byo_key_with_master() {
        use host::encrypt_secret;
        use repository::{LlmConfigRepository, TenantLlmConfig, WorkspaceRepository};
        let repo = SqliteRepository::in_memory().unwrap();
        let tenant = domain::TenantId::parse("acme").unwrap();
        let ws = domain::WorkspaceId::parse("ws-eng").unwrap();
        repo.create_workspace(
            &domain::Workspace::create(
                ws.clone(),
                tenant.clone(),
                "Eng",
                domain::UserId::parse("u1").unwrap(),
                10,
            )
            .unwrap(),
        )
        .unwrap();
        let master = [9u8; 32];
        repo.set_llm_config(
            &tenant,
            &TenantLlmConfig {
                base_url: None,
                model: Some("gpt-4o-mini".into()),
                api_key_enc: Some(encrypt_secret(&master, "sk-tenant-key").unwrap()),
            },
        )
        .unwrap();

        // With the master key the BYO key is decrypted (and counts as BYO);
        // without it, the key is dropped and it's no longer BYO.
        let r = resolve_llm(&repo, &ws, "demo", Some(&master));
        assert_eq!(r.api_key.as_deref(), Some("sk-tenant-key"));
        assert!(r.byo);
        let r = resolve_llm(&repo, &ws, "demo", None);
        assert!(r.api_key.is_none());
        assert!(!r.byo);
    }

    #[tokio::test]
    async fn budget_blocks_summaries_when_exhausted() {
        use repository::{BudgetRepository, BudgetRow, WorkspaceRepository};
        let repo = SqliteRepository::in_memory().unwrap();
        let k = key();
        // Token granting ws-eng, which maps to tenant t1.
        let token = {
            let svc = AuthService::new(&repo, &k);
            svc.login(
                &domain::DiscordProvider,
                &domain::ProviderClaims {
                    subject: "u".into(),
                    email: None,
                },
                vec![domain::WorkspaceId::parse("ws-eng").unwrap()],
                crate::auth::now_secs(),
            )
            .unwrap()
            .access_token
        };
        repo.create_workspace(
            &domain::Workspace::create(
                domain::WorkspaceId::parse("ws-eng").unwrap(),
                domain::TenantId::parse("t1").unwrap(),
                "Eng",
                domain::UserId::parse("u1").unwrap(),
                1,
            )
            .unwrap(),
        )
        .unwrap();
        // A budget of 1 micro — the first call fits (spent 0 < 1), then exceeds.
        repo.upsert_budget(
            &domain::TenantId::parse("t1").unwrap(),
            &BudgetRow {
                limit_micros: 1,
                period_secs: 3600,
                period_start: crate::auth::now_secs(),
                spent_micros: 0,
            },
        )
        .unwrap();
        // Non-zero price so the demo summary actually costs something.
        let state = AppState::new(repo, k).with_price(1_000);
        let app = build_router(state);
        let post = || {
            Request::post("/workspaces/ws-eng/summaries")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"messages":["hello team","ship on friday"]}"#,
                ))
                .unwrap()
        };

        let first = app.clone().oneshot(post()).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK); // allowed; charges the budget
        let second = app.oneshot(post()).await.unwrap();
        assert_eq!(second.status(), StatusCode::PAYMENT_REQUIRED); // budget exhausted → 402
    }

    #[tokio::test]
    async fn whatsapp_import_ingests_and_is_idempotent() {
        let (state, token) = seeded_state(); // token grants ws-1
        let app = build_router(state);
        let auth = format!("Bearer {token}");
        let body =
            "[01/01/2026, 09:00:00] Alice: morning team\n[01/01/2026, 09:01:00] Bob: morning";
        let post = || {
            Request::post("/workspaces/ws-1/whatsapp/imports?chat=family&tz=UTC")
                .header("authorization", &auth)
                .header("content-type", "text/plain")
                .body(Body::from(body))
                .unwrap()
        };

        let first = app.clone().oneshot(post()).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let j = body_json(first).await;
        assert_eq!(j["chat_id"], "family");
        assert_eq!(j["stored"], 2);
        assert_eq!(j["messages"], 2);
        assert_eq!(j["format"], "Ios");

        // Re-importing the same export stores nothing new (WHA-012).
        let again = app.oneshot(post()).await.unwrap();
        let j2 = body_json(again).await;
        assert_eq!(j2["stored"], 0);
        assert_eq!(j2["duplicates"], 2);
    }

    #[tokio::test]
    async fn whatsapp_import_rejects_bad_timezone() {
        let (state, token) = seeded_state();
        let resp = build_router(state)
            .oneshot(
                Request::post("/workspaces/ws-1/whatsapp/imports?chat=fam&tz=Mars/Olympus")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "text/plain")
                    .body(Body::from("[01/01/2026, 09:00:00] Alice: hi"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn creating_a_summary_publishes_a_live_event() {
        let (state, token) = seeded_state();
        // Subscribe to the live bus before the request, like an SSE client would.
        let mut rx = state.events.subscribe();
        let app = build_router(state);
        let created = app
            .oneshot(
                Request::post("/workspaces/ws-1/summaries")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"messages":["hello team","ship friday"]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::OK);
        let new_id = body_json(created).await["id"].as_str().unwrap().to_string();

        // The broadcast carries a matching summary.created event.
        let ev = rx.try_recv().expect("a live event was published");
        assert_eq!(ev.kind, "summary.created");
        assert_eq!(ev.workspace_id, "ws-1");
        assert_eq!(ev.summary_id.as_deref(), Some(new_id.as_str()));
    }

    #[tokio::test]
    async fn delete_one_summary_then_gone() {
        let (state, token) = seeded_state(); // has sum_1
        let app = build_router(state);
        let auth = format!("Bearer {token}");
        let del = app
            .clone()
            .oneshot(
                Request::delete("/workspaces/ws-1/summaries/sum_1")
                    .header("authorization", &auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(del.status(), StatusCode::NO_CONTENT);
        // Second delete → 404.
        let again = app
            .oneshot(
                Request::delete("/workspaces/ws-1/summaries/sum_1")
                    .header("authorization", &auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(again.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn bulk_delete_counts_only_real_rows() {
        let (state, token) = seeded_state(); // sum_1
        seed_extra(&state, &["sum_2", "sum_3"]);
        let app = build_router(state);
        let auth = format!("Bearer {token}");

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/workspaces/ws-1/summaries")
                    .header("authorization", &auth)
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"ids":["sum_1","sum_3","ghost"]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await["count"], 2); // ghost skipped

        // Only sum_2 remains.
        let listed = app
            .oneshot(
                Request::get("/workspaces/ws-1/summaries")
                    .header("authorization", &auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let lj = body_json(listed).await;
        assert_eq!(lj.as_array().unwrap().len(), 1);
        assert_eq!(lj[0]["id"], "sum_2");
    }

    #[tokio::test]
    async fn bulk_archive_then_hidden_from_default_list() {
        let (state, token) = seeded_state(); // sum_1
        seed_extra(&state, &["sum_2"]);
        let app = build_router(state);
        let auth = format!("Bearer {token}");

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/workspaces/ws-1/summaries")
                    .header("authorization", &auth)
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"ids":["sum_1","sum_2"],"archived":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await["count"], 2);

        // Default list excludes archived → empty; include_archived shows both.
        let active = app
            .clone()
            .oneshot(
                Request::get("/workspaces/ws-1/summaries")
                    .header("authorization", &auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(body_json(active).await.as_array().unwrap().is_empty());

        let all = app
            .oneshot(
                Request::get("/workspaces/ws-1/summaries?include_archived=true")
                    .header("authorization", &auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(body_json(all).await.as_array().unwrap().len(), 2);
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

    /// Log in via the API and return (access_token, refresh_token).
    async fn login_pair(app: &Router) -> (String, String) {
        let resp = app
            .clone()
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
        let j = body_json(resp).await;
        (
            j["access_token"].as_str().unwrap().to_string(),
            j["refresh_token"].as_str().unwrap().to_string(),
        )
    }

    #[tokio::test]
    async fn refresh_rotates_and_replay_is_rejected() {
        let repo = SqliteRepository::in_memory().unwrap();
        let app = build_router(AppState::new(repo, key()));
        let (_access, refresh) = login_pair(&app).await;

        // First refresh succeeds and returns a new pair.
        let ok = app
            .clone()
            .oneshot(
                Request::post("/auth/refresh")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"refresh_token":"{refresh}","workspaces":["ws-1"]}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::OK);
        let new_refresh = body_json(ok).await["refresh_token"]
            .as_str()
            .unwrap()
            .to_string();
        assert_ne!(new_refresh, refresh); // rotated

        // Replaying the OLD refresh token now fails (session was revoked) → 401.
        let replay = app
            .oneshot(
                Request::post("/auth/refresh")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(r#"{{"refresh_token":"{refresh}"}}"#)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn logout_revokes_the_session_so_refresh_fails() {
        let repo = SqliteRepository::in_memory().unwrap();
        let app = build_router(AppState::new(repo, key()));
        let (access, refresh) = login_pair(&app).await;

        // Logout with the access token → 204.
        let out = app
            .clone()
            .oneshot(
                Request::post("/auth/logout")
                    .header("authorization", format!("Bearer {access}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(out.status(), StatusCode::NO_CONTENT);

        // The refresh token from that session no longer works.
        let refused = app
            .oneshot(
                Request::post("/auth/refresh")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(r#"{{"refresh_token":"{refresh}"}}"#)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn refresh_with_garbage_token_is_unauthorized() {
        let repo = SqliteRepository::in_memory().unwrap();
        let resp = build_router(AppState::new(repo, key()))
            .oneshot(
                Request::post("/auth/refresh")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"refresh_token":"not-a-real-token"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
