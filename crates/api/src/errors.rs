//! Operational errors view (ADR-133 A3; ADR-031/024/041). Surfaces recorded
//! operational failures (sync/summarize/deliver) with operation/severity/scope,
//! and lets an admin resolve them. Read + resolve only; errors are *recorded* by
//! the operations that fail (see `record_operational_error`).

use crate::auth::AuthUser;
use crate::{ApiError, AppState};
use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Deserialize)]
pub struct ErrorsQuery {
    pub limit: Option<u32>,
    /// Include already-resolved errors (default: unresolved only).
    pub include_resolved: Option<bool>,
}

#[derive(Serialize)]
pub struct ErrorDto {
    pub id: String,
    pub operation: String,
    pub error_class: String,
    pub severity: String,
    pub channel_id: Option<String>,
    pub message: String,
    pub resolved: bool,
    pub created_at: i64,
}

#[derive(Serialize)]
pub struct ErrorsDto {
    pub unresolved: i64,
    pub errors: Vec<ErrorDto>,
}

/// `GET /workspaces/:ws/errors` — the operational error log, newest first.
pub async fn list_errors(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
    Query(q): Query<ErrorsQuery>,
) -> Result<Json<ErrorsDto>, ApiError> {
    use repository::OperationalErrorRepository;
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let limit = q.limit.unwrap_or(100).min(500);
    let include_resolved = q.include_resolved.unwrap_or(false);
    let repo = state.repo.lock().expect("repo mutex");
    let unresolved = repo
        .unresolved_error_count(&workspace)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let errors = repo
        .list_errors(&workspace, include_resolved, limit)
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .into_iter()
        .map(|e| ErrorDto {
            id: e.id,
            operation: e.operation,
            error_class: e.error_class,
            severity: e.severity,
            channel_id: e.channel_id,
            message: e.message,
            resolved: e.resolved,
            created_at: e.created_at,
        })
        .collect();
    Ok(Json(ErrorsDto { unresolved, errors }))
}

/// `POST /workspaces/:ws/errors/:id/resolve` — mark one resolved.
pub async fn resolve_error(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    use repository::OperationalErrorRepository;
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let repo = state.repo.lock().expect("repo mutex");
    let resolved = repo
        .resolve_error(&workspace, &id)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(serde_json::json!({ "resolved": resolved })))
}

/// `POST /workspaces/:ws/errors/resolve-all` — bulk-resolve (ADR-133 "Bulk Resolve").
pub async fn resolve_all_errors(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    use repository::OperationalErrorRepository;
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let repo = state.repo.lock().expect("repo mutex");
    let count = repo
        .resolve_all_errors(&workspace)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(serde_json::json!({ "resolved": count })))
}

/// Record an operational failure (ADR-133/031). Best-effort: a logging failure
/// must never mask the original error. `message` must be pre-sanitized (no
/// secrets, ADR-031 §4). Called from the operations that can fail.
pub fn record_operational_error(
    repo: &repository::SqliteRepository,
    workspace: &domain::WorkspaceId,
    operation: &str,
    class: domain::FailureClass,
    channel_id: Option<String>,
    message: impl Into<String>,
) {
    use repository::OperationalErrorRepository;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let e = repository::OperationalError::new(
        format!("err_{nanos}"),
        operation,
        class.as_str(),
        class.severity(),
        channel_id,
        message,
        now,
    );
    let _ = repo.record_error(workspace, &e);
}
