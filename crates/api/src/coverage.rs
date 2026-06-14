//! Coverage view (ADR-133) — how much of each channel's stored history is
//! covered by summaries, server-wide + per-channel, with gap periods. Generalizes
//! the WhatsApp-only coverage (ADR-121) to every source. Read-only.

use crate::auth::AuthUser;
use crate::{ApiError, AppState};
use axum::extract::{Path, State};
use axum::Json;
use serde::Serialize;

#[derive(Serialize)]
pub struct GapDto {
    pub start: i64,
    pub end: i64,
    pub kind: String,
}

#[derive(Serialize)]
pub struct ChannelCoverageDto {
    pub channel_id: String,
    pub earliest_content: i64,
    pub latest_content: i64,
    pub message_count: i64,
    pub summary_count: i64,
    pub coverage_percent: f64,
    pub gap_count: i64,
    pub gaps: Vec<GapDto>,
}

#[derive(Serialize)]
pub struct CoverageDto {
    pub total_coverage_percent: f64,
    pub total_gaps: i64,
    pub total_channels: i64,
    pub covered_channels: i64,
    pub total_summaries: i64,
    pub earliest_content: Option<i64>,
    pub latest_content: Option<i64>,
    pub channels: Vec<ChannelCoverageDto>,
}

fn gap_kind(k: domain::GapKind) -> &'static str {
    match k {
        domain::GapKind::BeforeJoin => "before_start",
        domain::GapKind::BetweenImports => "between",
        domain::GapKind::AfterLast => "after_last",
    }
}

/// `GET /workspaces/:ws/coverage` — server-wide + per-channel summary coverage.
pub async fn coverage(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
) -> Result<Json<CoverageDto>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let repo = state.repo.lock().expect("repo mutex");
    let cov = host::content_coverage(&*repo, &workspace)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(CoverageDto {
        total_coverage_percent: cov.total_coverage_percent,
        total_gaps: cov.total_gaps,
        total_channels: cov.total_channels,
        covered_channels: cov.covered_channels,
        total_summaries: cov.total_summaries,
        earliest_content: cov.earliest_content,
        latest_content: cov.latest_content,
        channels: cov
            .channels
            .into_iter()
            .map(|c| ChannelCoverageDto {
                channel_id: c.channel_id,
                earliest_content: c.earliest_content,
                latest_content: c.latest_content,
                message_count: c.message_count,
                summary_count: c.summary_count,
                coverage_percent: c.coverage_percent,
                gap_count: c.gap_count,
                gaps: c
                    .gaps
                    .into_iter()
                    .map(|g| GapDto {
                        start: g.start,
                        end: g.end,
                        kind: gap_kind(g.kind).to_string(),
                    })
                    .collect(),
            })
            .collect(),
    }))
}
