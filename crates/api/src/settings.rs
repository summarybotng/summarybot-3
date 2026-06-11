//! Per-workspace summarization settings endpoints (SUM-007).
//!
//! Free-text guidance appended to this workspace's summary prompts (e.g. a
//! perspective/focus). Workspace-scoped + authenticated.

use crate::auth::AuthUser;
use crate::{ApiError, AppState};
use axum::extract::{Path, State};
use axum::Json;
use repository::{WorkspaceSettings, WorkspaceSettingsRepository};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct SettingsDto {
    pub summary_instructions: Option<String>,
}

fn workspace(ws: String) -> Result<domain::WorkspaceId, ApiError> {
    domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))
}

/// `GET /workspaces/:ws/settings`
pub async fn get_settings(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
) -> Result<Json<SettingsDto>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace = workspace(ws)?;
    let repo = state.repo.lock().expect("repo mutex");
    let s = repo.get_settings(&workspace)?;
    Ok(Json(SettingsDto {
        summary_instructions: s.summary_instructions,
    }))
}

/// `PUT /workspaces/:ws/settings`
pub async fn set_settings(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
    Json(body): Json<SettingsDto>,
) -> Result<Json<SettingsDto>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace = workspace(ws)?;
    // Bound the guidance so a prompt can't be ballooned without limit.
    if let Some(text) = &body.summary_instructions {
        if text.len() > 2000 {
            return Err(ApiError::bad_request(
                "summary_instructions too long (max 2000 chars)".to_string(),
            ));
        }
    }
    let repo = state.repo.lock().expect("repo mutex");
    repo.set_settings(
        &workspace,
        &WorkspaceSettings {
            summary_instructions: body.summary_instructions,
        },
    )?;
    let s = repo.get_settings(&workspace)?;
    Ok(Json(SettingsDto {
        summary_instructions: s.summary_instructions,
    }))
}
