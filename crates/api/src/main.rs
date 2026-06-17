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
    // (e.g. a Mac mini's `llama3.1`, or a hosted OpenRouter model id). An explicit
    // `LLM_MODEL` always wins; otherwise pick a sensible default for the active
    // backend so summaries work without model config (v2 parity — v2 shipped a
    // baked-in Claude default and never asked the user to set one).
    if let Some(model) = resolve_model() {
        eprintln!("summarization model: {model}");
        state = state.with_model(&model);
    }
    // Process-default model price (micros per 1k tokens) so platform-key spend
    // is measurable for per-tenant budgets (ADR-125 Phase 3). Default 0 (demo).
    if let Some(price) = env::var("LLM_PRICE_PER_KTOKEN")
        .ok()
        .and_then(|p| p.trim().parse::<i64>().ok())
    {
        state = state.with_price(price);
    }
    // Usable summarization context window (tokens). Drives map-reduce chunking
    // (ADR-095): set this to what the backend actually honors — small for a
    // local model (e.g. 24000) so long chats are summarized in full rather than
    // truncated; left large for hosted models. Defaults to a large window.
    if let Some(ctx) = env::var("LLM_CONTEXT_TOKENS")
        .ok()
        .and_then(|c| c.trim().parse::<i64>().ok())
    {
        state = state.with_context_tokens(ctx);
        eprintln!("summarization context window: {ctx} tokens (map-reduce chunking)");
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
    // Platform operators (ADR-119/131): a comma-separated list of user ids with
    // the deployment-wide operator capability (e.g. per-tenant plugin disable).
    // Config-only — never grantable in-app.
    if let Some(raw) = env::var("PLATFORM_OPERATOR_IDS")
        .ok()
        .filter(|v| !v.trim().is_empty())
    {
        let ids: Vec<String> = raw.split(',').map(|s| s.trim().to_string()).collect();
        let n = ids.iter().filter(|s| !s.is_empty()).count();
        state = state.with_operators(ids);
        eprintln!("platform operators configured: {n}");
    }

    // Knowledge embedder (ADR-127): a local model over HTTP when configured,
    // else the deterministic demo embedder.
    state = state.with_embedder(select_embedder());

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

/// Economical, broadly-available current Claude model used as the default when
/// OpenRouter is the backend and no `LLM_MODEL` is set (v2 defaulted to Claude).
/// Overridable any time via `LLM_MODEL`.
#[cfg(feature = "http-llm")]
const DEFAULT_OPENROUTER_MODEL: &str = "anthropic/claude-3.5-haiku";

/// Resolve the summarization model: an explicit `LLM_MODEL` always wins. Failing
/// that, choose a default that matches the active backend so summaries work out
/// of the box. A local `LLM_BASE_URL` still needs an explicit `LLM_MODEL` — its
/// model name is deployment-specific (e.g. `qwen2.5:14b`) and can't be guessed —
/// and the demo backend keeps the built-in `"demo"` id (it ignores the name).
fn resolve_model() -> Option<String> {
    if let Some(m) = env::var("LLM_MODEL")
        .ok()
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
    {
        return Some(m);
    }
    #[cfg(feature = "http-llm")]
    {
        let has_base = env::var("LLM_BASE_URL")
            .ok()
            .map(|b| !b.trim().is_empty())
            .unwrap_or(false);
        let has_openrouter = env::var("OPENROUTER_API_KEY")
            .ok()
            .map(|k| !k.trim().is_empty())
            .unwrap_or(false);
        // OpenRouter is selected only when there's no explicit base URL (see
        // `select_llm`'s precedence) — match that here.
        if !has_base && has_openrouter {
            return Some(DEFAULT_OPENROUTER_MODEL.to_string());
        }
    }
    None
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
        let mut client = HttpLlmClient::new(base, key);
        // Best-effort context-window bump for a local Ollama (its default ~4k
        // truncates large prompts). Note: Ollama's `/v1` endpoint ignores this —
        // the prompt instruction is placed last so it survives truncation anyway.
        if let Some(n) = env::var("LLM_NUM_CTX")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
        {
            client = client.with_num_ctx(n);
        }
        return Arc::new(client);
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

/// Pick the knowledge embedder (ADR-127). With `http-llm` + `LLM_BASE_URL`, an
/// OpenAI-compatible `/embeddings` endpoint (the local model, `EMBED_MODEL`,
/// default `nomic-embed-text`, sharing `LLM_API_KEY`); otherwise the demo
/// embedder. A model change invalidates stored vectors — reindex on change (Q#8).
#[cfg(feature = "http-llm")]
fn select_embedder() -> Arc<dyn host::Embedder> {
    if let Some(base) = env::var("LLM_BASE_URL")
        .ok()
        .filter(|b| !b.trim().is_empty())
    {
        let model = env::var("EMBED_MODEL")
            .ok()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| "nomic-embed-text".to_string());
        let key = env::var("LLM_API_KEY")
            .ok()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .map(Secret::new);
        eprintln!("embedder: HTTP {} ({model})", base.trim());
        return Arc::new(host::HttpEmbedder::new(base.trim().to_string(), key, model));
    }
    eprintln!("embedder: demo (set LLM_BASE_URL for a real embedding model)");
    Arc::new(host::DemoEmbedder::default())
}

/// Without `http-llm`, the demo embedder is the only option.
#[cfg(not(feature = "http-llm"))]
fn select_embedder() -> Arc<dyn host::Embedder> {
    eprintln!("embedder: demo (build with --features http-llm for a real embedder)");
    Arc::new(host::DemoEmbedder::default())
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
