//! Summary management endpoints (PRD §5.1): list / detail / pin / archive / tag.
//! Workspace-scoped and auth-gated; thin over `StructuredSummaryRepository`.

use crate::auth::AuthUser;
use crate::{ApiError, AppState};
use axum::extract::{Path, Query, State};
use axum::Json;
use repository::{StructuredSummaryRepository, SummaryRecord};
use serde::{Deserialize, Serialize};

/// JSON shape of a stored summary (domain types have no serde, so map here).
#[derive(Serialize)]
pub struct SummaryDto {
    pub id: String,
    pub channel_id: Option<String>,
    pub model: String,
    pub cost_micros: i64,
    pub degraded: bool,
    pub pinned: bool,
    pub archived: bool,
    pub tags: Vec<String>,
    pub created_at: i64,
    pub text: String,
    pub key_points: Vec<String>,
    pub action_items: Vec<ActionItemDto>,
    pub technical_terms: Vec<String>,
    pub participants: Vec<String>,
    pub citations: Vec<CitationDto>,
}

#[derive(Serialize)]
pub struct ActionItemDto {
    pub text: String,
    pub assignee: Option<String>,
}

#[derive(Serialize)]
pub struct CitationDto {
    pub message_id: String,
    pub quote: Option<String>,
}

impl From<SummaryRecord> for SummaryDto {
    fn from(r: SummaryRecord) -> Self {
        SummaryDto {
            id: r.id,
            channel_id: r.channel_id.map(|c| c.as_str().to_string()),
            model: r.model,
            cost_micros: r.cost_micros,
            degraded: r.degraded,
            pinned: r.pinned,
            archived: r.archived,
            tags: r.tags,
            created_at: r.created_at,
            text: r.summary.text,
            key_points: r.summary.key_points,
            action_items: r
                .summary
                .action_items
                .into_iter()
                .map(|a| ActionItemDto {
                    text: a.text,
                    assignee: a.assignee,
                })
                .collect(),
            technical_terms: r.summary.technical_terms,
            participants: r.summary.participants,
            citations: r
                .summary
                .citations
                .into_iter()
                .map(|c| CitationDto {
                    message_id: c.message_id.as_str().to_string(),
                    quote: c.quote,
                })
                .collect(),
        }
    }
}

/// `?include_archived=true&limit=50`
#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub include_archived: bool,
    #[serde(default = "default_limit")]
    pub limit: u32,
}

fn default_limit() -> u32 {
    50
}

/// `GET /workspaces/:ws/summaries`
pub async fn list_summaries(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<SummaryDto>>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let repo = state.repo.lock().expect("repo mutex");
    let records = repo.list_records(&workspace, q.include_archived, q.limit.min(500))?;
    Ok(Json(records.into_iter().map(SummaryDto::from).collect()))
}

/// `GET /workspaces/:ws/summaries/:id`
pub async fn get_summary(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, id)): Path<(String, String)>,
) -> Result<Json<SummaryDto>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let repo = state.repo.lock().expect("repo mutex");
    let record = repo
        .get_record(&workspace, &id)?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(SummaryDto::from(record)))
}

/// Re-read a record after a mutation, or 404 if it vanished.
fn reload(
    repo: &repository::SqliteRepository,
    workspace: &domain::WorkspaceId,
    id: &str,
    changed: bool,
) -> Result<Json<SummaryDto>, ApiError> {
    if !changed {
        return Err(ApiError::NotFound);
    }
    let record = repo.get_record(workspace, id)?.ok_or(ApiError::NotFound)?;
    Ok(Json(SummaryDto::from(record)))
}

macro_rules! flag_handler {
    ($name:ident, $method:ident, $value:expr) => {
        pub async fn $name(
            State(state): State<AppState>,
            user: AuthUser,
            Path((ws, id)): Path<(String, String)>,
        ) -> Result<Json<SummaryDto>, ApiError> {
            user.require_workspace(&ws)?;
            let workspace =
                domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
            let repo = state.repo.lock().expect("repo mutex");
            let changed = repo.$method(&workspace, &id, $value)?;
            reload(&repo, &workspace, &id, changed)
        }
    };
}

flag_handler!(pin, set_pinned, true);
flag_handler!(unpin, set_pinned, false);
flag_handler!(archive, set_archived, true);
flag_handler!(unarchive, set_archived, false);

#[derive(Deserialize)]
pub struct TagsRequest {
    pub tags: Vec<String>,
}

/// `PUT /workspaces/:ws/summaries/:id/tags`
pub async fn set_tags(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, id)): Path<(String, String)>,
    Json(body): Json<TagsRequest>,
) -> Result<Json<SummaryDto>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let repo = state.repo.lock().expect("repo mutex");
    let changed = repo.set_tags(&workspace, &id, &body.tags)?;
    reload(&repo, &workspace, &id, changed)
}
