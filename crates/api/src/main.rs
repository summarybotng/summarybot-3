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
    let state = AppState::with_llm(repo, Secret::new(secret.into_bytes()), llm, limiter);

    // Background scheduler: fire due schedules on an interval (SCH-005/006).
    spawn_scheduler(state.clone(), scheduler_secs);

    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    eprintln!("SummaryBot API listening on {bind}");
    axum::serve(listener, app).await.context("serving")?;
    Ok(())
}

/// Pick the LLM backend from config. With the `openrouter` feature **and** a
/// non-empty `OPENROUTER_API_KEY`, use the live network client; otherwise fall
/// back to the deterministic demo client (no key/network).
#[cfg(feature = "openrouter")]
fn select_llm() -> Arc<dyn LlmClient + Send + Sync> {
    match env::var("OPENROUTER_API_KEY") {
        Ok(key) if !key.trim().is_empty() => {
            eprintln!("LLM backend: OpenRouter (live)");
            Arc::new(host::llm::OpenRouterClient::new(Secret::new(key)))
        }
        _ => {
            eprintln!("LLM backend: demo (set OPENROUTER_API_KEY for live LLM)");
            Arc::new(host::llm::DemoLlmClient)
        }
    }
}

/// Without the `openrouter` feature the only backend is the demo client.
#[cfg(not(feature = "openrouter"))]
fn select_llm() -> Arc<dyn LlmClient + Send + Sync> {
    eprintln!("LLM backend: demo (build with --features openrouter for live LLM)");
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
