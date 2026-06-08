//! Summary delivery destinations endpoints (DSH-010/011).
//!
//! Manage where a workspace's summaries are delivered *besides* the always-on
//! dashboard. Today: webhooks (generic + Slack/Discord incoming-webhook URLs).
//! The URL is a secret — it's encrypted at rest with the operator master key
//! (ADR-125 Phase 2b) and never returned to the client; the list shows only a
//! scheme+host hint. Workspace-scoped + authenticated.

use crate::auth::AuthUser;
use crate::{ApiError, AppState};
use axum::extract::{Path, State};
use axum::Json;
use repository::{DestinationRepository, StoredDestination};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// A destination as shown to the client — never includes the secret address.
#[derive(Serialize)]
pub struct DestinationDto {
    pub id: String,
    pub kind: String,
    pub enabled: bool,
    /// Scheme+host of the configured URL (e.g. `https://hooks.slack.com`), so a
    /// user can recognize it without exposing the secret path.
    pub hint: Option<String>,
}

/// Create a webhook destination.
#[derive(Deserialize)]
pub struct CreateDestinationRequest {
    /// Destination kind; only `webhook` is supported today.
    #[serde(default = "default_kind")]
    pub kind: String,
    /// The webhook URL (http/https). Stored encrypted.
    pub url: String,
}

fn default_kind() -> String {
    "webhook".to_string()
}

fn workspace(ws: String) -> Result<domain::WorkspaceId, ApiError> {
    domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))
}

/// Scheme+host of a URL, for a non-secret display hint.
fn host_hint(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let host = rest.split('/').next().unwrap_or(rest);
    if host.is_empty() {
        None
    } else {
        Some(format!("{scheme}://{host}"))
    }
}

fn to_dto(row: &StoredDestination, master: Option<&[u8; 32]>) -> DestinationDto {
    let hint = match (&row.address_enc, master) {
        (Some(enc), Some(key)) => host::decrypt_secret(key, enc)
            .ok()
            .and_then(|u| host_hint(&u)),
        _ => None,
    };
    DestinationDto {
        id: row.id.clone(),
        kind: row.kind.clone(),
        enabled: row.enabled,
        hint,
    }
}

/// `GET /workspaces/:ws/destinations`
pub async fn list_destinations(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
) -> Result<Json<Vec<DestinationDto>>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace = workspace(ws)?;
    let repo = state.repo.lock().expect("repo mutex");
    let rows = repo.list_destinations(&workspace)?;
    let master = state.master_key();
    Ok(Json(rows.iter().map(|r| to_dto(r, master)).collect()))
}

/// `POST /workspaces/:ws/destinations` — add a webhook (DSH-010).
pub async fn create_destination(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
    Json(body): Json<CreateDestinationRequest>,
) -> Result<Json<DestinationDto>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace = workspace(ws)?;

    if body.kind != "webhook" {
        return Err(ApiError::bad_request(format!(
            "unsupported destination kind: {}",
            body.kind
        )));
    }
    let url = body.url.trim();
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(ApiError::bad_request(
            "url must be an http(s) webhook URL".to_string(),
        ));
    }
    // The URL is a secret; we must be able to encrypt it at rest.
    let Some(master) = state.master_key() else {
        return Err(ApiError::bad_request(
            "server has no encryption key configured (set LLM_CONFIG_KEY) — cannot store a webhook URL safely".to_string(),
        ));
    };
    let address_enc =
        host::encrypt_secret(master, url).map_err(|e| ApiError::Internal(e.to_string()))?;

    let row = StoredDestination {
        id: format!("dst_{}", unique_suffix()),
        kind: "webhook".into(),
        address_enc: Some(address_enc),
        enabled: true,
        created_at: crate::auth::now_secs(),
    };
    let repo = state.repo.lock().expect("repo mutex");
    repo.upsert_destination(&workspace, &row)?;
    Ok(Json(to_dto(&row, Some(master))))
}

/// `DELETE /workspaces/:ws/destinations/:id`
pub async fn delete_destination(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, id)): Path<(String, String)>,
) -> Result<axum::http::StatusCode, ApiError> {
    user.require_workspace(&ws)?;
    let workspace = workspace(ws)?;
    let repo = state.repo.lock().expect("repo mutex");
    if repo.delete_destination(&workspace, &id)? {
        Ok(axum::http::StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

/// Result of a webhook test send.
#[derive(Serialize)]
pub struct TestResult {
    pub ok: bool,
    pub detail: Option<String>,
}

/// `POST /workspaces/:ws/destinations/:id/test` — send a sample payload to the
/// destination so a user can confirm it works (DSH-010). Requires the `http-llm`
/// build (network delivery) and the master key to decrypt the address.
pub async fn test_destination(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, id)): Path<(String, String)>,
) -> Result<Json<TestResult>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace = workspace(ws)?;
    let master = state.master_key();
    let deliverers = state.deliverers();

    let repo = state.repo.lock().expect("repo mutex");
    let row = repo
        .list_destinations(&workspace)?
        .into_iter()
        .find(|d| d.id == id)
        .ok_or(ApiError::NotFound)?;
    drop(repo);

    let kind = host::delivery::parse_kind(&row.kind)
        .ok_or_else(|| ApiError::bad_request("unknown destination kind".to_string()))?;
    let address = match (&row.address_enc, master) {
        (Some(enc), Some(key)) => {
            host::decrypt_secret(key, enc).map_err(|e| ApiError::Internal(e.to_string()))?
        }
        _ => {
            return Ok(Json(TestResult {
                ok: false,
                detail: Some("destination has no readable address".to_string()),
            }))
        }
    };
    let Some(deliverer) = deliverers.iter().find(|d| d.kind() == kind) else {
        return Ok(Json(TestResult {
            ok: false,
            detail: Some(
                "no deliverer available (build the server with --features http-llm)".to_string(),
            ),
        }));
    };
    let dest = domain::Destination {
        kind,
        platform: None,
        address: Some(address),
    };
    match deliverer.deliver(
        &dest,
        "SummaryBot test delivery — your webhook is configured correctly.",
    ) {
        Ok(()) => Ok(Json(TestResult {
            ok: true,
            detail: None,
        })),
        Err(e) => Ok(Json(TestResult {
            ok: false,
            detail: Some(e),
        })),
    }
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}
