//! WhatsApp export ingestion endpoint (PRD §2.3, WHA-001; ADR-121).
//!
//! Accepts a `.zip` (or a raw `_chat.txt`) upload as the request body, extracts
//! the transcript, parses it under a declared timezone + date order (the file
//! carries neither — WHA-020), then anonymizes/dedups/persists via the host
//! ingestor. Workspace-scoped + authenticated; re-uploading the same export is
//! idempotent at the message level (WHA-012).

use crate::auth::AuthUser;
use crate::{ApiError, AppState};
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::Json;
use domain::{ChannelId, DateOrder, WorkspaceId};
use host::IngestContext;
use serde::{Deserialize, Serialize};

/// `?chat=<channel id>&tz=<IANA zone>&date_order=dmy|mdy`
#[derive(Deserialize)]
pub struct ImportQuery {
    /// The workspace channel id this chat is filed under (so schedules can scope
    /// to it).
    pub chat: String,
    /// IANA timezone the export's local timestamps are in (default UTC).
    #[serde(default = "default_tz")]
    pub tz: String,
    /// Locale **hint** for ambiguous dates only — `dmy` (default) or `mdy`.
    /// The order is inferred from the data when possible; this is the fallback.
    #[serde(default)]
    pub date_order: Option<String>,
}

fn default_tz() -> String {
    "UTC".to_string()
}

/// Outcome of an import.
#[derive(Serialize)]
pub struct ImportDto {
    pub chat_id: String,
    /// Detected export dialect (`Ios` / `Android`).
    pub format: String,
    pub messages: usize,
    pub stored: u64,
    pub duplicates: u64,
    pub new_participants: u64,
    pub date_start: Option<i64>,
    pub date_end: Option<i64>,
}

/// `POST /workspaces/:ws/whatsapp/imports` — body is the export (zip or text).
pub async fn import_whatsapp(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
    Query(q): Query<ImportQuery>,
    body: Bytes,
) -> Result<Json<ImportDto>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace = WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let chat = ChannelId::parse(q.chat).map_err(|e| ApiError::bad_request(e.to_string()))?;
    if body.is_empty() {
        return Err(ApiError::bad_request("empty upload"));
    }
    let order = match q.date_order.as_deref() {
        Some("mdy") | Some("MDY") => DateOrder::MonthDayYear,
        _ => DateOrder::DayMonthYear,
    };

    // The HMAC key for phone anonymization (WHA-006) reuses the server's secret;
    // a dedicated key is a hardening refinement.
    let (summary, parsed) = {
        let repo = state.repo.lock().expect("repo mutex");
        let ctx = IngestContext {
            workspace_id: &workspace,
            chat_id: &chat,
            uploader: &user.0.sub,
        };
        host::ingest_whatsapp_zip(
            &*repo,
            &state.signing_key,
            &ctx,
            &body,
            &q.tz,
            order,
            crate::auth::now_secs(),
        )
        .map_err(|e| ApiError::bad_request(e.to_string()))?
    };

    Ok(Json(ImportDto {
        chat_id: chat.as_str().to_string(),
        format: format!("{:?}", parsed.format),
        messages: parsed.messages.len(),
        stored: summary.stored,
        duplicates: summary.duplicates,
        new_participants: summary.new_participants,
        date_start: parsed.date_range.map(|(s, _)| s),
        date_end: parsed.date_range.map(|(_, e)| e),
    }))
}

/// One classified gap in a chat's coverage, with a copy-ready ask (WHA-017/019).
#[derive(Serialize)]
pub struct GapDto {
    pub start: i64,
    pub end: i64,
    /// `before_join` | `between_imports` | `after_last`.
    pub kind: String,
    /// Whether a member could plausibly export this range to fill it.
    pub can_fill: bool,
}

/// A chat's merged coverage picture (WHA-016).
#[derive(Serialize)]
pub struct CoverageDto {
    pub chat_id: String,
    pub earliest: Option<i64>,
    pub latest: Option<i64>,
    /// Total seconds covered by the union of import spans.
    pub covered_secs: i64,
    pub gaps: Vec<GapDto>,
}

/// A chat in the workspace overview: its import stats plus coverage (WHA-017).
#[derive(Serialize)]
pub struct ChatCoverageDto {
    pub chat_id: String,
    pub import_count: i64,
    pub message_count: i64,
    pub coverage: CoverageDto,
}

fn gap_kind_tag(k: domain::GapKind) -> &'static str {
    match k {
        domain::GapKind::BeforeJoin => "before_join",
        domain::GapKind::BetweenImports => "between_imports",
        domain::GapKind::AfterLast => "after_last",
    }
}

fn coverage_dto(chat_id: String, report: domain::CoverageReport) -> CoverageDto {
    CoverageDto {
        chat_id,
        earliest: report.earliest,
        latest: report.latest,
        covered_secs: report.covered_secs,
        gaps: report
            .gaps
            .into_iter()
            .map(|g| GapDto {
                start: g.start,
                end: g.end,
                kind: gap_kind_tag(g.kind).to_string(),
                can_fill: g.can_fill,
            })
            .collect(),
    }
}

/// `GET /workspaces/:ws/whatsapp/chats` — per-chat coverage overview (WHA-017).
pub async fn list_chats(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
) -> Result<Json<Vec<ChatCoverageDto>>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace = WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let now = crate::auth::now_secs();
    let chats = {
        let repo = state.repo.lock().expect("repo mutex");
        host::workspace_coverage(&*repo, &workspace, now)
            .map_err(|e| ApiError::Internal(e.to_string()))?
    };
    Ok(Json(
        chats
            .into_iter()
            .map(|c| ChatCoverageDto {
                chat_id: c.summary.chat_id.clone(),
                import_count: c.summary.import_count,
                message_count: c.summary.message_count,
                coverage: coverage_dto(c.summary.chat_id, c.report),
            })
            .collect(),
    ))
}

/// `GET /workspaces/:ws/whatsapp/chats/:chat/coverage` — one chat's gaps (WHA-016).
pub async fn chat_coverage(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, chat)): Path<(String, String)>,
) -> Result<Json<CoverageDto>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace = WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let channel = ChannelId::parse(&chat).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let now = crate::auth::now_secs();
    let report = {
        let repo = state.repo.lock().expect("repo mutex");
        host::coverage_for(&*repo, &workspace, &channel, now)
            .map_err(|e| ApiError::Internal(e.to_string()))?
    };
    Ok(Json(coverage_dto(channel.as_str().to_string(), report)))
}
