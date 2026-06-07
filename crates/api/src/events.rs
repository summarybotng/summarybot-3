//! Live updates over Server-Sent Events (PRD §12.6 item 4).
//!
//! A process-wide [`tokio::sync::broadcast`] channel carries [`LiveEvent`]s;
//! mutations (a summary created/deleted) publish onto it, and the SSE endpoint
//! streams the events for one workspace to a connected client so the dashboard
//! updates without polling. Single-instance for now (an in-process channel);
//! horizontal scale swaps the channel for Redis pub/sub behind the same shape.

use crate::auth::AuthUser;
use crate::{ApiError, AppState};
use axum::extract::{Path, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use domain::WorkspaceId;
use serde::Serialize;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

/// A workspace-scoped live event pushed to subscribed dashboards.
#[derive(Clone, Debug, Serialize)]
pub struct LiveEvent {
    pub workspace_id: String,
    /// `summary.created` | `summary.deleted`.
    pub kind: String,
    /// The summary the event concerns, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_id: Option<String>,
}

impl LiveEvent {
    fn new(workspace: &WorkspaceId, kind: &str, summary_id: Option<String>) -> Self {
        Self {
            workspace_id: workspace.as_str().to_string(),
            kind: kind.to_string(),
            summary_id,
        }
    }

    pub fn summary_created(workspace: &WorkspaceId, id: &str) -> Self {
        Self::new(workspace, "summary.created", Some(id.to_string()))
    }

    pub fn summary_deleted(workspace: &WorkspaceId, id: &str) -> Self {
        Self::new(workspace, "summary.deleted", Some(id.to_string()))
    }
}

/// `GET /workspaces/:ws/events` — Server-Sent Events stream for a workspace.
/// Auth-gated; emits one SSE message per [`LiveEvent`] whose workspace matches,
/// with a periodic keep-alive comment so idle connections survive proxies.
pub async fn workspace_events(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, axum::Error>>>, ApiError> {
    user.require_workspace(&ws)?;
    let want = ws;
    let stream = BroadcastStream::new(state.events.subscribe()).filter_map(move |res| match res {
        // Only this workspace's events; a lagged receiver just drops frames.
        Ok(ev) if ev.workspace_id == want => {
            Some(Event::default().event(ev.kind.clone()).json_data(&ev))
        }
        _ => None,
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
