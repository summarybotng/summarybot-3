//! API server entrypoint (PRD §12.6) — the I/O shell: read config, open the DB,
//! bind tokio, serve the router. The request logic lives in the `api` lib and is
//! tested there via `oneshot`; this is the thin async runtime around it.

use anyhow::{Context, Result};
use api::{build_router, spawn_scheduler, AppState};
use domain::Secret;
use host::llm::{GlobalRateLimiter, LlmClient, RateLimitConfig};
use repository::SqliteRepository;
use std::env;
use std::sync::Arc;
use tower_http::services::{ServeDir, ServeFile};

#[tokio::main]
async fn main() -> Result<()> {
    // Config from the environment (see PRD §11 / V3 guide).
    let bind = env::var("API_BIND").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
    let secret = env::var("SECRET_KEY")
        .context("SECRET_KEY is required (HS256 signing key for access tokens)")?;
    let db_path = env::var("DATABASE_URL").unwrap_or_else(|_| "summarybot.db".to_string());

    let scheduler_secs: u64 = env::var("SCHEDULER_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);

    let repo = open_db(&db_path)?;
    // One process-wide LLM backend + rate limiter, shared by the on-demand
    // endpoint and the scheduler (LEG-001 budget is global).
    let limiter = Arc::new(GlobalRateLimiter::new(RateLimitConfig::default()));
    let llm = select_llm();
    // Product base domain for host→tenant routing (TEN-006).
    let base_domain = env::var("BASE_DOMAIN").unwrap_or_else(|_| "summarybot.app".to_string());
    let mut state = AppState::with_llm(repo, Secret::new(secret.into_bytes()), llm, limiter)
        .with_base_domain(base_domain);
    // Summarization model name (ADR-125): whatever the configured backend serves
    // (e.g. a Mac mini's `llama3.1`, or a hosted OpenRouter model id). Defaults to
    // the deterministic demo model.
    if let Some(model) = env::var("LLM_MODEL").ok().filter(|m| !m.trim().is_empty()) {
        state = state.with_model(model.trim());
    }
    // Master key for encrypting stored tenant API keys (ADR-125 Phase 2b). When
    // unset, tenants can configure a keyless endpoint but not store a BYO key.
    if let Some(raw) = env::var("LLM_CONFIG_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
    {
        let key = host::parse_master_key(&raw).context(
            "LLM_CONFIG_KEY must be 64 hex chars (32 bytes, e.g. `openssl rand -hex 32`)",
        )?;
        state = state.with_config_key(key);
        eprintln!("tenant API-key encryption: enabled");
    }

    // Background scheduler: fire due schedules on an interval (SCH-005/006).
    spawn_scheduler(state.clone(), scheduler_secs);

    // Serve the built dashboard SPA (WEB_DIR, default web/dist): hashed assets
    // under /assets, and index.html (200) for any other non-API path so client
    // routing / reloads work. API routes are matched before the fallback.
    let web_dir = env::var("WEB_DIR").unwrap_or_else(|_| "web/dist".to_string());
    let app = build_router(state)
        .nest_service("/assets", ServeDir::new(format!("{web_dir}/assets")))
        .fallback_service(ServeFile::new(format!("{web_dir}/index.html")));

    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    eprintln!("SummaryBot API listening on {bind}");
    axum::serve(listener, app).await.context("serving")?;
    Ok(())
}

/// Pick the process-default LLM backend from config (ADR-125, the "process
/// default" rung). Precedence with the `http-llm` feature: an explicit
/// OpenAI-compatible `LLM_BASE_URL` (local Mac mini / self-hosted / any
/// provider, optional `LLM_API_KEY`) → `OPENROUTER_API_KEY` → demo.
#[cfg(feature = "http-llm")]
fn select_llm() -> Arc<dyn LlmClient + Send + Sync> {
    use host::llm::HttpLlmClient;
    if let Some(base) = env::var("LLM_BASE_URL")
        .ok()
        .filter(|b| !b.trim().is_empty())
    {
        let base = base.trim().to_string();
        let key = env::var("LLM_API_KEY")
            .ok()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .map(Secret::new);
        eprintln!(
            "LLM backend: HTTP {base}{}",
            if key.is_some() { " (+key)" } else { "" }
        );
        return Arc::new(HttpLlmClient::new(base, key));
    }
    match env::var("OPENROUTER_API_KEY") {
        Ok(key) if !key.trim().is_empty() => {
            eprintln!("LLM backend: OpenRouter");
            Arc::new(HttpLlmClient::openrouter(Secret::new(key)))
        }
        _ => {
            eprintln!("LLM backend: demo (set LLM_BASE_URL or OPENROUTER_API_KEY for a real LLM)");
            Arc::new(host::llm::DemoLlmClient)
        }
    }
}

/// Without the `http-llm` feature the only backend is the demo client.
#[cfg(not(feature = "http-llm"))]
fn select_llm() -> Arc<dyn LlmClient + Send + Sync> {
    eprintln!("LLM backend: demo (build with --features http-llm for a real LLM)");
    Arc::new(host::llm::DemoLlmClient)
}

/// Open the SQLite database, honoring the `sqlite:///path` URL form.
fn open_db(db_path: &str) -> Result<SqliteRepository> {
    let path = db_path.strip_prefix("sqlite://").unwrap_or(db_path);
    if path.is_empty() || path == ":memory:" {
        SqliteRepository::in_memory()
    } else {
        SqliteRepository::open(path).with_context(|| format!("opening database at {path}"))
    }
}
