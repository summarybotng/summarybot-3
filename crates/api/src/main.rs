//! API server entrypoint (PRD §12.6) — the I/O shell: read config, open the DB,
//! bind tokio, serve the router. The request logic lives in the `api` lib and is
//! tested there via `oneshot`; this is the thin async runtime around it.

use anyhow::{Context, Result};
use api::{build_router, AppState};
use domain::Secret;
use repository::SqliteRepository;
use std::env;

#[tokio::main]
async fn main() -> Result<()> {
    // Config from the environment (see PRD §11 / V3 guide).
    let bind = env::var("API_BIND").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
    let secret = env::var("SECRET_KEY")
        .context("SECRET_KEY is required (HS256 signing key for access tokens)")?;
    let db_path = env::var("DATABASE_URL").unwrap_or_else(|_| "summarybot.db".to_string());

    let repo = open_db(&db_path)?;
    let state = AppState::new(repo, Secret::new(secret.into_bytes()));
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    eprintln!("SummaryBot API listening on {bind}");
    axum::serve(listener, app).await.context("serving")?;
    Ok(())
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
