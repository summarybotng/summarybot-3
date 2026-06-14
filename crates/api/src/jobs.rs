//! Jobs view (ADR-040) — surface long-running / background work (e.g. a
//! retrospective by-week run) as job rows with status, progress, and cost. The
//! `jobs` ledger already backs async work (ADR-013); this exposes it read-only.

use crate::auth::AuthUser;
use crate::{ApiError, AppState};
use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct JobsQuery {
    pub limit: Option<u32>,
}

#[derive(Serialize)]
pub struct JobDto {
    pub id: String,
    pub job_type: String,
    pub status: String,
    pub progress_current: u32,
    pub progress_total: u32,
    pub cost_micros: i64,
    pub failure_reason: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// `GET /workspaces/:ws/jobs?limit=N` — the workspace's jobs, newest first.
pub async fn list_jobs(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
    Query(q): Query<JobsQuery>,
) -> Result<Json<Vec<JobDto>>, ApiError> {
    use repository::JobRepository;
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let limit = q.limit.unwrap_or(50).min(200);
    let repo = state.repo.lock().expect("repo mutex");
    let jobs = repo
        .list_jobs(&workspace, limit)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(
        jobs.into_iter()
            .map(|j| JobDto {
                id: j.id.as_str().to_string(),
                job_type: j.job_type.as_str().to_string(),
                status: j.status.as_str().to_string(),
                progress_current: j.progress_current,
                progress_total: j.progress_total,
                cost_micros: j.cost_micros,
                failure_reason: j.failure_reason,
                created_at: j.created_at,
                updated_at: j.updated_at,
            })
            .collect(),
    ))
}
