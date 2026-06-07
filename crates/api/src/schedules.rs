//! Schedule management endpoints (PRD §5.2): create / list / detail / pause /
//! resume / delete. Workspace-scoped, auth-gated; thin over `ScheduleRepository`
//! + the domain recurrence engine (which computes the initial `next_run`).

use crate::auth::{now_secs, AuthUser};
use crate::summaries::{demo_ladder, SummaryDto};
use crate::{ApiError, AppState};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use host::llm::ResilientLlm;
use host::{ScheduleRunner, SummarizingScheduleRunner};
use repository::{
    RunStatus, ScheduleRepository, ScheduleRun, ScheduleRunRepository, StoredSchedule,
    StructuredSummaryRepository,
};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Create-schedule body (mirrors the recurrence definition; primitives only so
/// the API stays free of chrono types — the domain parses them).
#[derive(Deserialize)]
pub struct CreateScheduleRequest {
    pub schedule_type: String,
    #[serde(default)]
    pub hour: u32,
    #[serde(default)]
    pub minute: u32,
    /// Weekdays Mon=0..Sun=6 (for `weekly`).
    #[serde(default)]
    pub days: Vec<u32>,
    #[serde(default = "default_dom")]
    pub day_of_month: u32,
    #[serde(default = "default_tz")]
    pub timezone: String,
    #[serde(default)]
    pub once_at: Option<i64>,
    #[serde(default)]
    pub custom_interval_secs: i64,
    /// Channel to summarize (ADR-011 scope); omit for a not-yet-scoped schedule.
    #[serde(default)]
    pub channel: Option<String>,
    /// How far back each run reads messages (seconds).
    #[serde(default = "default_lookback")]
    pub lookback_secs: i64,
}

fn default_dom() -> u32 {
    1
}
fn default_tz() -> String {
    "UTC".to_string()
}
fn default_lookback() -> i64 {
    86_400
}

#[derive(Serialize)]
pub struct ScheduleDto {
    pub id: String,
    pub schedule_type: String,
    pub hour: u32,
    pub minute: u32,
    pub days: Vec<u32>,
    pub day_of_month: u32,
    pub timezone: String,
    pub enabled: bool,
    pub channel: Option<String>,
    pub lookback_secs: i64,
    pub next_run: i64,
    pub consecutive_failures: u32,
}

impl From<StoredSchedule> for ScheduleDto {
    fn from(s: StoredSchedule) -> Self {
        ScheduleDto {
            id: s.id,
            schedule_type: s.schedule.schedule_type.as_str().to_string(),
            hour: s.schedule.at.hour,
            minute: s.schedule.at.minute,
            days: s.schedule.day_numbers(),
            day_of_month: s.schedule.day_of_month,
            timezone: s.schedule.timezone_name().to_string(),
            enabled: s.schedule.enabled,
            channel: s.schedule.channel.as_ref().map(|c| c.as_str().to_string()),
            lookback_secs: s.schedule.lookback_secs,
            next_run: s.next_run,
            consecutive_failures: s.consecutive_failures,
        }
    }
}

fn workspace(ws: String) -> Result<domain::WorkspaceId, ApiError> {
    domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))
}

/// `POST /workspaces/:ws/schedules`
pub async fn create_schedule(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
    Json(body): Json<CreateScheduleRequest>,
) -> Result<Json<ScheduleDto>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace = workspace(ws)?;
    let schedule = domain::Schedule::build(
        workspace,
        &body.schedule_type,
        body.hour,
        body.minute,
        &body.days,
        body.day_of_month,
        &body.timezone,
        body.once_at,
        body.custom_interval_secs,
        true,
        body.channel.as_deref(),
        body.lookback_secs,
    )
    .map_err(|e| ApiError::bad_request(e.to_string()))?;

    let now = now_secs();
    let next_run = schedule
        .next_run(now)
        .ok_or_else(|| ApiError::bad_request("schedule has no future run"))?;
    let stored = StoredSchedule {
        id: format!("sch_{}", unique_suffix()),
        schedule,
        next_run,
        consecutive_failures: 0,
    };
    {
        let repo = state.repo.lock().expect("repo mutex");
        repo.create_schedule(&stored)?;
    }
    Ok(Json(ScheduleDto::from(stored)))
}

/// `PUT /workspaces/:ws/schedules/:id` — replace a schedule's recurrence
/// definition (SCM-003). The enabled/paused state and failure count are
/// preserved; `next_run` is recomputed from the new recurrence.
pub async fn update_schedule(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, id)): Path<(String, String)>,
    Json(body): Json<CreateScheduleRequest>,
) -> Result<Json<ScheduleDto>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace = workspace(ws)?;
    let repo = state.repo.lock().expect("repo mutex");
    let current = repo
        .get_schedule(&workspace, &id)?
        .ok_or(ApiError::NotFound)?;
    // Rebuild the definition, preserving the current paused/enabled state.
    let schedule = domain::Schedule::build(
        workspace.clone(),
        &body.schedule_type,
        body.hour,
        body.minute,
        &body.days,
        body.day_of_month,
        &body.timezone,
        body.once_at,
        body.custom_interval_secs,
        current.schedule.enabled,
        body.channel.as_deref(),
        body.lookback_secs,
    )
    .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let next_run = schedule
        .next_run(now_secs())
        .ok_or_else(|| ApiError::bad_request("schedule has no future run"))?;
    if !repo.update_schedule(&workspace, &id, &schedule, next_run)? {
        return Err(ApiError::NotFound);
    }
    Ok(Json(ScheduleDto::from(StoredSchedule {
        id,
        schedule,
        next_run,
        consecutive_failures: current.consecutive_failures,
    })))
}

/// `GET /workspaces/:ws/schedules`
pub async fn list_schedules(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
) -> Result<Json<Vec<ScheduleDto>>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace = workspace(ws)?;
    let repo = state.repo.lock().expect("repo mutex");
    let schedules = repo.list_for_workspace(&workspace)?;
    Ok(Json(schedules.into_iter().map(ScheduleDto::from).collect()))
}

/// `GET /workspaces/:ws/schedules/:id`
pub async fn get_schedule(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, id)): Path<(String, String)>,
) -> Result<Json<ScheduleDto>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace = workspace(ws)?;
    let repo = state.repo.lock().expect("repo mutex");
    let s = repo
        .get_schedule(&workspace, &id)?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(ScheduleDto::from(s)))
}

async fn set_enabled(
    state: AppState,
    user: AuthUser,
    ws: String,
    id: String,
    enabled: bool,
) -> Result<Json<ScheduleDto>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace = workspace(ws)?;
    let repo = state.repo.lock().expect("repo mutex");
    if !repo.set_enabled(&workspace, &id, enabled)? {
        return Err(ApiError::NotFound);
    }
    let s = repo
        .get_schedule(&workspace, &id)?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(ScheduleDto::from(s)))
}

/// `POST /workspaces/:ws/schedules/:id/pause`
pub async fn pause(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, id)): Path<(String, String)>,
) -> Result<Json<ScheduleDto>, ApiError> {
    set_enabled(state, user, ws, id, false).await
}

/// `POST /workspaces/:ws/schedules/:id/resume`
pub async fn resume(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, id)): Path<(String, String)>,
) -> Result<Json<ScheduleDto>, ApiError> {
    set_enabled(state, user, ws, id, true).await
}

/// Result of an immediate trigger.
#[derive(Serialize)]
pub struct TriggerResponse {
    /// Whether a summary was actually produced (false when the schedule is
    /// unscoped or the lookback window had nothing substantial).
    pub produced: bool,
    pub summary: Option<SummaryDto>,
}

/// `POST /workspaces/:ws/schedules/:id/run` — trigger a schedule immediately
/// (SCM-007), out of band. Runs the same pipeline a scheduled tick would (resolve
/// scope → summarize → store), but does **not** alter the schedule's cadence or
/// `next_run`. Produces a summary only when the schedule is channel-scoped and
/// the lookback window holds substantial messages.
pub async fn trigger_schedule(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, id)): Path<(String, String)>,
) -> Result<Json<TriggerResponse>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace = workspace(ws)?;
    // Shared process-wide backend + limiter, same as the scheduler driver.
    let engine = ResilientLlm::new(state.llm.clone(), state.limiter.clone());
    let ladder = demo_ladder();
    let now = now_secs();

    let repo = state.repo.lock().expect("repo mutex");
    let stored = repo
        .get_schedule(&workspace, &id)?
        .ok_or(ApiError::NotFound)?;
    let runner = SummarizingScheduleRunner::new(&*repo, &engine, &ladder);
    match runner.run(&stored, now) {
        Ok(()) => {
            // The runner stores under this deterministic id when it produces one.
            let produced = repo
                .get_record(&workspace, &format!("sum_{id}_{now}"))?
                .map(SummaryDto::from);
            let detail = produced.as_ref().map(|s| s.id.clone());
            repo.record_run(
                &workspace,
                &id,
                now,
                RunStatus::Fired,
                detail.as_deref(),
                true,
            )?;
            Ok(Json(TriggerResponse {
                produced: produced.is_some(),
                summary: produced,
            }))
        }
        Err(reason) => {
            repo.record_run(&workspace, &id, now, RunStatus::Failed, Some(&reason), true)?;
            Err(ApiError::Internal(reason))
        }
    }
}

/// JSON shape of a recorded run (SCM-005).
#[derive(Serialize)]
pub struct RunDto {
    pub id: i64,
    pub schedule_id: String,
    pub ran_at: i64,
    pub status: String,
    pub detail: Option<String>,
    pub manual: bool,
}

impl From<ScheduleRun> for RunDto {
    fn from(r: ScheduleRun) -> Self {
        RunDto {
            id: r.id,
            schedule_id: r.schedule_id,
            ran_at: r.ran_at,
            status: r.status.as_str().to_string(),
            detail: r.detail,
            manual: r.manual,
        }
    }
}

/// `?limit=50`
#[derive(Deserialize)]
pub struct RunsQuery {
    #[serde(default = "default_runs_limit")]
    pub limit: u32,
}

fn default_runs_limit() -> u32 {
    50
}

/// `GET /workspaces/:ws/schedules/:id/runs` — execution history, newest first
/// (SCM-005).
pub async fn list_runs(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, id)): Path<(String, String)>,
    Query(q): Query<RunsQuery>,
) -> Result<Json<Vec<RunDto>>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace = workspace(ws)?;
    let repo = state.repo.lock().expect("repo mutex");
    let runs = repo.list_runs(&workspace, &id, q.limit.min(500))?;
    Ok(Json(runs.into_iter().map(RunDto::from).collect()))
}

/// `DELETE /workspaces/:ws/schedules/:id`
pub async fn delete_schedule(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    user.require_workspace(&ws)?;
    let workspace = workspace(ws)?;
    let repo = state.repo.lock().expect("repo mutex");
    if repo.delete_schedule(&workspace, &id)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}
