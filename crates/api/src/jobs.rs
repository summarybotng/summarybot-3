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
    // ADR-133 §B context.
    pub scope: Option<String>,
    pub schedule_name: Option<String>,
    pub summary_ids: Vec<String>,
    pub date_start: i64,
    pub date_end: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub creation_source: Option<String>,
    pub pause_reason: Option<String>,
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
                scope: j.scope,
                schedule_name: j.schedule_name,
                summary_ids: j.summary_ids,
                date_start: j.date_start,
                date_end: j.date_end,
                started_at: j.started_at,
                completed_at: j.completed_at,
                creation_source: j.creation_source,
                pause_reason: j.pause_reason,
            })
            .collect(),
    ))
}

/// `POST /workspaces/:ws/jobs/:id/retry` — re-run a job (ADR-133 D1). Our jobs
/// wrap *synchronous* work, so only a **scheduled** job can be replayed: we
/// re-fire its originating schedule (same path as a manual trigger), which
/// records a fresh job. Other types (ad-hoc summaries, sync) aren't replayable
/// from the job row alone — re-run them from their own screen. There is no
/// pause/resume: nothing sits mid-flight in a synchronous model (recurring
/// cadence is paused on the Schedules screen).
pub async fn retry_job(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    use host::llm::ResilientLlm;
    use host::{ScheduleRunner, SummarizingScheduleRunner};
    use repository::{JobRepository, ScheduleRepository, StructuredSummaryRepository};
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let job_id = domain::JobId::parse(id).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let now = crate::auth::now_secs();

    let job = {
        let repo = state.repo.lock().expect("repo mutex");
        repo.get_job(&workspace, &job_id)
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .ok_or(ApiError::NotFound)?
    };
    if job.job_type != domain::JobType::Scheduled {
        return Err(ApiError::bad_request(
            "only scheduled jobs can be retried automatically — re-run others from their screen",
        ));
    }
    let Some(schedule_id) = job.schedule_name.clone() else {
        return Err(ApiError::bad_request("job has no originating schedule to retry"));
    };

    let repo = state.repo.lock().expect("repo mutex");
    let resolution = crate::resolve_llm(&repo, &workspace, &state.model, state.master_key());
    let charge = crate::budget_gate(&repo, &resolution, now)?;
    let engine = ResilientLlm::new(
        state.client_for_base(resolution.base_url, resolution.api_key),
        state.limiter.clone(),
    );
    let ladder = state.ladder_for(&resolution.model);
    let stored = repo
        .get_schedule(&workspace, &schedule_id)
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::bad_request("the originating schedule no longer exists"))?;
    let deliverers = state.deliverers();
    let runner = SummarizingScheduleRunner::new(&*repo, &engine, &ladder)
        .with_delivery(&deliverers, state.master_key().copied())
        .with_knowledge(&*state.embedder);
    runner
        .run(&stored, now)
        .map_err(|e| ApiError::Internal(format!("retry failed: {e}")))?;
    let produced = repo
        .get_record(&workspace, &format!("sum_{schedule_id}_{now}"))
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if let Some(rec) = &produced {
        crate::knowledge::ingest_summary(
            &state,
            &repo,
            &workspace,
            &rec.summary,
            &rec.id,
            rec.channel_id.as_ref().map(|c| c.as_str()),
            now,
        );
        if let Some((tenant, window)) = &charge {
            crate::budget_charge(&repo, tenant, *window, rec.cost_micros)?;
        }
    }
    Ok(Json(serde_json::json!({ "produced": produced.map(|r| r.id) })))
}
