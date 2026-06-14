//! Prompt templates / custom perspectives (ADR-133 A2). Workspace-scoped named
//! instruction presets, plus the built-in [`domain::summarize::Perspective`]s.
//! A template/perspective can steer an on-demand summary (see `summaries.rs`).

use crate::auth::AuthUser;
use crate::{ApiError, AppState};
use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Serialize)]
pub struct PerspectiveDto {
    pub id: String,
    pub label: String,
}

#[derive(Serialize)]
pub struct TemplateDto {
    pub id: String,
    pub name: String,
    pub content: String,
    pub based_on: Option<String>,
    pub usage_count: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Serialize)]
pub struct PromptsDto {
    /// Built-in perspectives (not editable).
    pub perspectives: Vec<PerspectiveDto>,
    /// Workspace-defined named templates.
    pub templates: Vec<TemplateDto>,
}

#[derive(Deserialize)]
pub struct TemplateInput {
    pub name: String,
    pub content: String,
    pub based_on: Option<String>,
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn to_dto(t: repository::PromptTemplate) -> TemplateDto {
    TemplateDto {
        id: t.id,
        name: t.name,
        content: t.content,
        based_on: t.based_on,
        usage_count: t.usage_count,
        created_at: t.created_at,
        updated_at: t.updated_at,
    }
}

/// `GET /workspaces/:ws/prompts` — built-in perspectives + workspace templates.
pub async fn list_prompts(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
) -> Result<Json<PromptsDto>, ApiError> {
    use repository::PromptTemplateRepository;
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let repo = state.repo.lock().expect("repo mutex");
    let templates = repo
        .list_templates(&workspace)
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .into_iter()
        .map(to_dto)
        .collect();
    let perspectives = domain::summarize::Perspective::all()
        .into_iter()
        .map(|p| PerspectiveDto { id: p.as_str().to_string(), label: p.label().to_string() })
        .collect();
    Ok(Json(PromptsDto { perspectives, templates }))
}

fn validate(input: &TemplateInput) -> Result<(), ApiError> {
    if input.name.trim().is_empty() {
        return Err(ApiError::bad_request("name must not be empty"));
    }
    if input.content.trim().is_empty() {
        return Err(ApiError::bad_request("content must not be empty"));
    }
    Ok(())
}

/// `POST /workspaces/:ws/prompts` — create a named template.
pub async fn create_prompt(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
    Json(input): Json<TemplateInput>,
) -> Result<Json<TemplateDto>, ApiError> {
    use repository::PromptTemplateRepository;
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    validate(&input)?;
    let now = now_secs();
    let t = repository::PromptTemplate {
        id: format!("tmpl_{}", now_secs_nanos()),
        name: input.name.trim().to_string(),
        content: input.content.trim().to_string(),
        based_on: input.based_on,
        usage_count: 0,
        created_at: now,
        updated_at: now,
    };
    let repo = state.repo.lock().expect("repo mutex");
    repo.create_template(&workspace, &t)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(to_dto(t)))
}

/// `PUT /workspaces/:ws/prompts/:id` — update name/content.
pub async fn update_prompt(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, id)): Path<(String, String)>,
    Json(input): Json<TemplateInput>,
) -> Result<Json<TemplateDto>, ApiError> {
    use repository::PromptTemplateRepository;
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    validate(&input)?;
    let repo = state.repo.lock().expect("repo mutex");
    let mut t = repo
        .get_template(&workspace, &id)
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or(ApiError::NotFound)?;
    t.name = input.name.trim().to_string();
    t.content = input.content.trim().to_string();
    t.updated_at = now_secs();
    repo.update_template(&workspace, &t)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(to_dto(t)))
}

/// `DELETE /workspaces/:ws/prompts/:id`.
pub async fn delete_prompt(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    use repository::PromptTemplateRepository;
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let repo = state.repo.lock().expect("repo mutex");
    let removed = repo
        .delete_template(&workspace, &id)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(serde_json::json!({ "removed": removed })))
}

fn now_secs_nanos() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}
