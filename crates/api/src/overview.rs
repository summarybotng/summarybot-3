//! Workspace Overview (ADR-133 A6) — the dashboard home: headline counts,
//! recent summaries, coverage, and config status, composed read-only from the
//! existing stores. No new storage.

use crate::auth::AuthUser;
use crate::{ApiError, AppState};
use axum::extract::{Path, State};
use axum::Json;
use serde::Serialize;

#[derive(Serialize)]
pub struct RecentSummaryDto {
    pub id: String,
    pub title: String,
    pub created_at: i64,
}

#[derive(Serialize)]
pub struct OverviewDto {
    pub summary_count: i64,
    pub schedule_count: i64,
    pub member_count: i64,
    pub unresolved_errors: i64,
    pub total_cost_micros: i64,
    pub coverage_percent: f64,
    pub last_summary_at: Option<i64>,
    pub recent: Vec<RecentSummaryDto>,
}

/// `GET /workspaces/:ws/overview` — headline counts + recent activity.
pub async fn overview(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
) -> Result<Json<OverviewDto>, ApiError> {
    use repository::{
        MembershipRepository, OperationalErrorRepository, ScheduleRepository,
        StructuredSummaryRepository, WorkspaceRepository,
    };
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let repo = state.repo.lock().expect("repo mutex");

    let spend = repo
        .workspace_spend(&workspace, 0)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let schedule_count = repo
        .list_for_workspace(&workspace)
        .map(|s| s.len() as i64)
        .unwrap_or(0);
    // Members are tenant-scoped; a dev workspace with no tenant has none.
    let member_count = match repo.find_workspace(&workspace) {
        Ok(Some(w)) => repo.list_members(&w.tenant_id).map(|m| m.len() as i64).unwrap_or(0),
        _ => 0,
    };
    let unresolved_errors = repo.unresolved_error_count(&workspace).unwrap_or(0);
    let coverage_percent = host::content_coverage(&*repo, &workspace)
        .map(|c| c.total_coverage_percent)
        .unwrap_or(0.0);
    let recent_records = repo
        .list_records(&workspace, false, 5)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let last_summary_at = recent_records.first().map(|r| r.created_at);
    let recent = recent_records
        .into_iter()
        .map(|r| RecentSummaryDto {
            id: r.id,
            title: r.summary.text.chars().take(80).collect(),
            created_at: r.created_at,
        })
        .collect();

    Ok(Json(OverviewDto {
        summary_count: spend.summary_count,
        schedule_count,
        member_count,
        unresolved_errors,
        total_cost_micros: spend.total_micros,
        coverage_percent,
        last_summary_at,
        recent,
    }))
}
