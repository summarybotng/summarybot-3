//! User problem reports (ADR-039) — the "Report a problem" button. Any
//! authenticated member can submit; admins list + triage them. Distinct from the
//! system-generated operational error log (`errors.rs`).

use crate::auth::AuthUser;
use crate::{ApiError, AppState};
use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Allowed categories (ADR-039). Anything else is rejected.
const CATEGORIES: &[&str] = &[
    "summary_quality",
    "missing_summary",
    "parameter_mismatch",
    "feed_issue",
    "schedule_failure",
    "ui_bug",
    "performance",
    "other",
];
const STATUSES: &[&str] = &["open", "investigating", "resolved", "wont_fix"];
/// Allowed severities (ADR-039). Defaults to `medium` when omitted/unknown.
const SEVERITIES: &[&str] = &["low", "medium", "high", "critical"];

#[derive(Deserialize)]
pub struct CreateIssueRequest {
    pub category: String,
    #[serde(default)]
    pub severity: Option<String>,
    pub description: String,
    #[serde(default)]
    pub resource_type: Option<String>,
    #[serde(default)]
    pub resource_id: Option<String>,
    #[serde(default)]
    pub page_url: Option<String>,
    /// Reporter's browser/user-agent, auto-captured by the web client.
    #[serde(default)]
    pub browser: Option<String>,
}

#[derive(Serialize)]
pub struct IssueDto {
    pub id: String,
    pub category: String,
    pub severity: String,
    pub description: String,
    pub resource_type: Option<String>,
    pub resource_id: Option<String>,
    pub page_url: Option<String>,
    pub browser: Option<String>,
    pub reported_by: Option<String>,
    pub status: String,
    pub created_at: i64,
}

#[derive(Serialize)]
pub struct IssuesDto {
    pub open: i64,
    /// Total matching the current filters (for pagination), ignoring limit/offset.
    pub total: i64,
    pub categories: Vec<&'static str>,
    pub severities: Vec<&'static str>,
    pub issues: Vec<IssueDto>,
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}
fn nanos() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}
fn to_dto(r: repository::ProblemReport) -> IssueDto {
    IssueDto {
        id: r.id,
        category: r.category,
        severity: r.severity,
        description: r.description,
        resource_type: r.resource_type,
        resource_id: r.resource_id,
        page_url: r.page_url,
        browser: r.browser,
        reported_by: r.reported_by,
        status: r.status,
        created_at: r.created_at,
    }
}

/// `POST /workspaces/:ws/issues` — submit a problem report (any member).
pub async fn create_issue(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
    Json(body): Json<CreateIssueRequest>,
) -> Result<Json<IssueDto>, ApiError> {
    use repository::ProblemReportRepository;
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let category = body.category.trim().to_string();
    if !CATEGORIES.contains(&category.as_str()) {
        return Err(ApiError::bad_request("unknown category"));
    }
    if body.description.trim().is_empty() {
        return Err(ApiError::bad_request("description must not be empty"));
    }
    let severity = body
        .severity
        .map(|s| s.trim().to_string())
        .filter(|s| SEVERITIES.contains(&s.as_str()))
        .unwrap_or_else(|| "medium".to_string());
    let report = repository::ProblemReport {
        id: format!("issue_{}", nanos()),
        category,
        severity,
        description: body.description.trim().to_string(),
        resource_type: body.resource_type.filter(|s| !s.trim().is_empty()),
        resource_id: body.resource_id.filter(|s| !s.trim().is_empty()),
        page_url: body.page_url.filter(|s| !s.trim().is_empty()),
        browser: body.browser.filter(|s| !s.trim().is_empty()).map(|s| {
            // Cap stored UA length defensively (boundary validation).
            s.chars().take(512).collect()
        }),
        reported_by: Some(user.0.sub.as_str().to_string()),
        status: "open".to_string(),
        created_at: now_secs(),
    };
    let repo = state.repo.lock().expect("repo mutex");
    repo.create_report(&workspace, &report)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(to_dto(report)))
}

#[derive(Deserialize)]
pub struct IssuesQuery {
    pub include_resolved: Option<bool>,
    pub severity: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// `GET /workspaces/:ws/issues` — list reports (newest first), filterable by
/// severity and paginated via limit/offset.
pub async fn list_issues(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
    Query(q): Query<IssuesQuery>,
) -> Result<Json<IssuesDto>, ApiError> {
    use repository::ProblemReportRepository;
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    // Only honour a known severity; an unknown value is ignored (no filter).
    let severity = q
        .severity
        .as_deref()
        .map(str::trim)
        .filter(|s| SEVERITIES.contains(s));
    let include_resolved = q.include_resolved.unwrap_or(false);
    let limit = q.limit.unwrap_or(100).min(500);
    let offset = q.offset.unwrap_or(0);
    let repo = state.repo.lock().expect("repo mutex");
    let open = repo.open_report_count(&workspace).unwrap_or(0);
    let total = repo
        .count_reports(&workspace, include_resolved, severity)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let issues = repo
        .list_reports(&workspace, include_resolved, severity, limit, offset)
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .into_iter()
        .map(to_dto)
        .collect();
    Ok(Json(IssuesDto {
        open,
        total,
        categories: CATEGORIES.to_vec(),
        severities: SEVERITIES.to_vec(),
        issues,
    }))
}

#[derive(Deserialize)]
pub struct StatusRequest {
    pub status: String,
}

/// `POST /workspaces/:ws/issues/:id/status` — triage/resolve a report.
pub async fn set_issue_status(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, id)): Path<(String, String)>,
    Json(body): Json<StatusRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    use repository::ProblemReportRepository;
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    if !STATUSES.contains(&body.status.as_str()) {
        return Err(ApiError::bad_request("status must be open|investigating|resolved|wont_fix"));
    }
    let repo = state.repo.lock().expect("repo mutex");
    let changed = repo
        .set_report_status(&workspace, &id, &body.status)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(serde_json::json!({ "updated": changed })))
}
