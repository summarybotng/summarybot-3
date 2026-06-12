//! Knowledge subsystem endpoints (PRD §8.1; ADR-127) — semantic search over the
//! units extracted from a workspace's summaries, plus the ingest helper the
//! summary paths call after a summary is stored.

use crate::auth::AuthUser;
use crate::{ApiError, AppState};
use axum::extract::{Path, Query, State};
use axum::Json;
use domain::summarize::ExtractedSummary;
use host::KnowledgeService;
use repository::SqliteRepository;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct SearchQuery {
    /// The natural-language query.
    #[serde(default)]
    pub q: String,
    /// Max results (default 10, capped at 50).
    pub k: Option<usize>,
}

#[derive(Serialize)]
pub struct HitDto {
    pub id: String,
    pub summary_id: String,
    pub kind: String,
    pub text: String,
    pub source_ids: Vec<String>,
    pub score: f32,
}

/// `GET /workspaces/:ws/wiki/search?q=...&k=...` — semantic search (KNO-005).
pub async fn search(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Vec<HitDto>>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let q = query.q.trim();
    if q.is_empty() {
        return Ok(Json(vec![]));
    }
    let k = query.k.unwrap_or(10).min(50);
    let repo = state.repo.lock().expect("repo mutex");
    let hits = KnowledgeService::new(&*repo, &*state.embedder)
        .search(&workspace, q, k)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(
        hits.into_iter()
            .map(|h| HitDto {
                id: h.id,
                summary_id: h.summary_id,
                kind: h.kind,
                text: h.text,
                source_ids: h.source_ids,
                score: h.score,
            })
            .collect(),
    ))
}

/// Ingest a produced summary's knowledge units (KNO-001..003). Best-effort: a
/// failure (e.g. the embedder is down) is logged, never propagated — knowledge
/// ingestion must not fail the summary that was already stored/delivered.
/// Call with the repo lock held.
pub(crate) fn ingest_summary(
    state: &AppState,
    repo: &SqliteRepository,
    workspace: &domain::WorkspaceId,
    summary: &ExtractedSummary,
    summary_id: &str,
    now: i64,
) {
    if let Err(e) =
        KnowledgeService::new(repo, &*state.embedder).ingest(workspace, summary, summary_id, now)
    {
        eprintln!("knowledge ingest failed for {summary_id}: {e}");
    }
}
