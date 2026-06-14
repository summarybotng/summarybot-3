//! Workspace coverage aggregates (ADR-133) — the raw inputs the host turns into
//! a per-channel coverage picture via the pure `analyze_coverage` algorithm.
//!
//! Two cheap GROUP-BY reads: each channel's stored-message date range + count,
//! and the covered windows (`period_start..period_end`) of its summaries. A
//! workspace-wide summary (no single `channel_id`, e.g. an all-channels or
//! category schedule) covers every channel, so the host applies those spans to
//! each channel rather than dropping them.

use crate::SqliteRepository;
use anyhow::Result;
use domain::WorkspaceId;
use rusqlite::params;

/// One channel's stored-content extent (unix seconds) + message count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelContent {
    pub channel_id: String,
    pub earliest: i64,
    pub latest: i64,
    pub message_count: i64,
}

/// One summary's covered window. `channel_id` is `None` for a workspace-wide
/// (all-channels / category) summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummarySpan {
    pub channel_id: Option<String>,
    pub start: i64,
    pub end: i64,
}

/// Read aggregates for the coverage dashboard (ADR-133).
pub trait CoverageRepository {
    /// Per-channel stored-message extent + count (channels with ≥1 message).
    fn channel_content(&self, workspace: &WorkspaceId) -> Result<Vec<ChannelContent>>;
    /// Covered windows of the workspace's summaries (newest first not required).
    /// Zero-width windows (`start == end`, e.g. ad-hoc pasted text) are excluded
    /// so they don't add phantom points to the gap analysis.
    fn summary_spans(&self, workspace: &WorkspaceId) -> Result<Vec<SummarySpan>>;
}

impl CoverageRepository for SqliteRepository {
    fn channel_content(&self, workspace: &WorkspaceId) -> Result<Vec<ChannelContent>> {
        let mut stmt = self.conn.prepare(
            "SELECT channel_id, MIN(timestamp), MAX(timestamp), COUNT(*)
             FROM messages WHERE workspace_id = ?1
             GROUP BY channel_id ORDER BY channel_id",
        )?;
        let rows = stmt
            .query_map(params![workspace.as_str()], |row| {
                Ok(ChannelContent {
                    channel_id: row.get(0)?,
                    earliest: row.get(1)?,
                    latest: row.get(2)?,
                    message_count: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn summary_spans(&self, workspace: &WorkspaceId) -> Result<Vec<SummarySpan>> {
        let mut stmt = self.conn.prepare(
            "SELECT channel_id, period_start, period_end
             FROM summary_records
             WHERE workspace_id = ?1 AND period_end > period_start AND archived = 0",
        )?;
        let rows = stmt
            .query_map(params![workspace.as_str()], |row| {
                Ok(SummarySpan {
                    channel_id: row.get(0)?,
                    start: row.get(1)?,
                    end: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}
