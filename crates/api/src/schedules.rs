//! Schedule management endpoints (PRD §5.2): create / list / detail / pause /
//! resume / delete. Workspace-scoped, auth-gated; thin over `ScheduleRepository`
//! + the domain recurrence engine (which computes the initial `next_run`).

use crate::auth::{now_secs, AuthUser};
use crate::{ApiError, AppState};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use repository::{ScheduleRepository, StoredSchedule};
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
}

fn default_dom() -> u32 {
    1
}
fn default_tz() -> String {
    "UTC".to_string()
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
