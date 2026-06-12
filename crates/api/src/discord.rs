//! Discord live ingestion endpoints (ADR-128) — store the bot token (encrypted)
//! and sync a guild's recent messages into the message store, from which the
//! existing summarize / schedule paths read unchanged.
//!
//! Feature-gated (`discord`): without it the routes aren't registered and the
//! network client isn't compiled, keeping the default build offline.

use crate::auth::AuthUser;
use crate::{ApiError, AppState};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

/// Platform key for Discord credentials/connections.
const PLATFORM: &str = "discord";

#[derive(Serialize)]
pub struct ConnectionStatusDto {
    /// Whether a bot token is stored for this workspace (the secret is never
    /// returned).
    pub token_set: bool,
}

/// `GET /workspaces/:ws/connections/discord` — whether a bot token is configured.
pub async fn status(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
) -> Result<Json<ConnectionStatusDto>, ApiError> {
    use repository::PlatformCredentialRepository;
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let repo = state.repo.lock().expect("repo mutex");
    let token_set = repo
        .get_platform_token(&workspace, PLATFORM)
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .is_some();
    Ok(Json(ConnectionStatusDto { token_set }))
}

#[derive(Deserialize)]
pub struct SetTokenRequest {
    /// The Discord **bot** token (stored encrypted at rest, AES-256-GCM).
    pub token: String,
}

/// `PUT /workspaces/:ws/connections/discord/token` — set/replace the bot token.
pub async fn set_token(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
    Json(body): Json<SetTokenRequest>,
) -> Result<Json<ConnectionStatusDto>, ApiError> {
    use repository::PlatformCredentialRepository;
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let token = body.token.trim();
    if token.is_empty() {
        return Err(ApiError::bad_request("token must not be empty"));
    }
    let Some(master) = state.master_key() else {
        return Err(ApiError::bad_request(
            "server has no encryption key configured (set LLM_CONFIG_KEY) — cannot store a bot token safely".to_string(),
        ));
    };
    let token_enc =
        host::encrypt_secret(master, token).map_err(|e| ApiError::Internal(e.to_string()))?;
    let repo = state.repo.lock().expect("repo mutex");
    repo.set_platform_token(&workspace, PLATFORM, &token_enc, crate::auth::now_secs())
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(ConnectionStatusDto { token_set: true }))
}

/// `DELETE /workspaces/:ws/connections/discord/token` — clear the bot token.
pub async fn delete_token(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
) -> Result<StatusCode, ApiError> {
    use repository::PlatformCredentialRepository;
    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let repo = state.repo.lock().expect("repo mutex");
    let removed = repo
        .delete_platform_token(&workspace, PLATFORM)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

#[derive(Deserialize)]
pub struct SyncRequest {
    /// The Discord guild (server) id to pull from.
    pub guild_id: String,
    /// Specific channel ids; omit/empty to sync all of the guild's text channels.
    #[serde(default)]
    pub channels: Vec<String>,
    /// How far back to fetch, in seconds (e.g. 86400 = last day).
    pub lookback_secs: i64,
}

#[derive(Serialize)]
pub struct SyncErrorDto {
    pub channel: String,
    pub message: String,
}

#[derive(Serialize)]
pub struct SyncResponse {
    /// Channels fetched from (their ids — the UI can summarize each directly).
    pub channel_ids: Vec<String>,
    /// Messages fetched within the window.
    pub fetched: usize,
    /// Newly stored (idempotent: re-syncing the same window stores 0).
    pub stored: usize,
    /// Per-channel fetch failures (one bad channel doesn't fail the sync).
    pub errors: Vec<SyncErrorDto>,
}

/// `POST /workspaces/:ws/connections/discord/sync` — fetch the guild's recent
/// messages and persist them into the message store (ADR-128). Idempotent on the
/// native message id, so overlapping windows converge rather than duplicate.
pub async fn sync(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
    Json(body): Json<SyncRequest>,
) -> Result<Json<SyncResponse>, ApiError> {
    use host::{DiscordFetcher, FetchScope, PlatformFetcher};
    use repository::{PlatformCredentialRepository, WhatsAppRepository};

    user.require_workspace(&ws)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    if body.guild_id.trim().is_empty() {
        return Err(ApiError::bad_request("guild_id must not be empty"));
    }
    if body.lookback_secs <= 0 {
        return Err(ApiError::bad_request("lookback_secs must be positive"));
    }
    let now = crate::auth::now_secs();
    let start = now - body.lookback_secs;

    // Decrypt the stored bot token.
    let token = {
        let Some(master) = state.master_key() else {
            return Err(ApiError::bad_request(
                "server has no encryption key configured (set LLM_CONFIG_KEY)".to_string(),
            ));
        };
        let repo = state.repo.lock().expect("repo mutex");
        let enc = repo
            .get_platform_token(&workspace, PLATFORM)
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .ok_or_else(|| ApiError::bad_request("no Discord bot token set for this workspace"))?;
        host::decrypt_secret(master, &enc).map_err(|e| ApiError::Internal(e.to_string()))?
    };

    // Fetch off the lock (network I/O); resolve scope first.
    let fetcher = DiscordFetcher::new(token, body.guild_id.trim());
    let scope = if body.channels.is_empty() {
        FetchScope::Workspace
    } else {
        let mut ids = Vec::with_capacity(body.channels.len());
        for c in &body.channels {
            ids.push(
                domain::ChannelId::parse(c).map_err(|e| ApiError::bad_request(e.to_string()))?,
            );
        }
        FetchScope::Channels(ids)
    };
    let channels = fetcher
        .resolve_channels(&scope)
        .map_err(|e| ApiError::Internal(format!("resolve channels: {}", e.message)))?;
    let result = fetcher.fetch_messages(&channels, start, now);

    // Persist (idempotent); count newly stored.
    let mut stored = 0usize;
    {
        let repo = state.repo.lock().expect("repo mutex");
        for m in &result.messages {
            if repo
                .save_message(&workspace, m)
                .map_err(|e| ApiError::Internal(e.to_string()))?
            {
                stored += 1;
            }
        }
    }

    Ok(Json(SyncResponse {
        channel_ids: channels.iter().map(|c| c.as_str().to_string()).collect(),
        fetched: result.messages.len(),
        stored,
        errors: result
            .errors
            .into_iter()
            .map(|e| SyncErrorDto {
                channel: e.channel.as_str().to_string(),
                message: e.message,
            })
            .collect(),
    }))
}
