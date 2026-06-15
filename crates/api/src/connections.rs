//! Live platform connection endpoints (ADR-128) — store a bot token (encrypted)
//! and sync a source's recent messages into the message store, from which the
//! existing summarize / schedule paths read unchanged. Platform-generic: the
//! `:platform` path segment selects Discord or Slack; the fetcher is built by
//! [`host::make_platform_fetcher`], which errors if that platform's ingestion
//! feature isn't compiled in. Token storage works regardless (it's just an
//! encrypted blob); `status.supported` tells the UI whether sync will work.

use crate::auth::AuthUser;
use crate::{ApiError, AppState};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use domain::Platform;
use serde::{Deserialize, Serialize};

/// Parse + validate the platform path segment as a *live* source (Discord/Slack;
/// not WhatsApp, which is upload-only).
fn live_platform(raw: &str) -> Result<Platform, ApiError> {
    match Platform::parse(raw) {
        Ok(p @ (Platform::Discord | Platform::Slack)) => Ok(p),
        Ok(Platform::WhatsApp) => Err(ApiError::bad_request(
            "WhatsApp is upload-only, not a live source",
        )),
        Err(_) => Err(ApiError::bad_request("unknown platform")),
    }
}

/// Whether this build compiled in ingestion for `platform`.
fn supported(platform: Platform) -> bool {
    match platform {
        Platform::Discord => cfg!(feature = "discord"),
        Platform::Slack => cfg!(feature = "slack"),
        Platform::WhatsApp => false,
    }
}

#[derive(Serialize)]
pub struct ConnectionStatusDto {
    /// Whether a bot token is stored for this workspace + platform (never the
    /// secret itself).
    pub token_set: bool,
    /// Whether this server build can actually fetch from the platform.
    pub supported: bool,
}

/// `GET /workspaces/:ws/connections/:platform` — token + support status.
pub async fn status(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, platform)): Path<(String, String)>,
) -> Result<Json<ConnectionStatusDto>, ApiError> {
    use repository::PlatformCredentialRepository;
    user.require_workspace(&ws)?;
    let platform = live_platform(&platform)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let repo = state.repo.lock().expect("repo mutex");
    let token_set = repo
        .get_platform_token(&workspace, platform.as_str())
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .is_some();
    Ok(Json(ConnectionStatusDto {
        token_set,
        supported: supported(platform),
    }))
}

#[derive(Deserialize)]
pub struct SetTokenRequest {
    /// The platform **bot** token (Discord bot token / Slack `xoxb-…`), stored
    /// encrypted at rest (AES-256-GCM).
    pub token: String,
}

/// `PUT /workspaces/:ws/connections/:platform/token` — set/replace the token.
pub async fn set_token(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, platform)): Path<(String, String)>,
    Json(body): Json<SetTokenRequest>,
) -> Result<Json<ConnectionStatusDto>, ApiError> {
    use repository::PlatformCredentialRepository;
    user.require_workspace(&ws)?;
    let platform = live_platform(&platform)?;
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
    repo.set_platform_token(
        &workspace,
        platform.as_str(),
        &token_enc,
        crate::auth::now_secs(),
    )
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(ConnectionStatusDto {
        token_set: true,
        supported: supported(platform),
    }))
}

/// `DELETE /workspaces/:ws/connections/:platform/token` — clear the token.
pub async fn delete_token(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, platform)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    use repository::PlatformCredentialRepository;
    user.require_workspace(&ws)?;
    let platform = live_platform(&platform)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let repo = state.repo.lock().expect("repo mutex");
    let removed = repo
        .delete_platform_token(&workspace, platform.as_str())
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

#[derive(Deserialize)]
pub struct SyncRequest {
    /// Source scope id: the Discord guild (server) id. Slack ignores it (the bot
    /// token is workspace-scoped).
    #[serde(default)]
    pub scope_id: Option<String>,
    /// Specific channel ids; omit/empty to sync all of the source's channels.
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

/// `POST /workspaces/:ws/connections/:platform/sync` — fetch the source's recent
/// messages and persist them into the message store (ADR-128). Idempotent on the
/// native message id, so overlapping windows converge rather than duplicate.
pub async fn sync(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, platform)): Path<(String, String)>,
    Json(body): Json<SyncRequest>,
) -> Result<Json<SyncResponse>, ApiError> {
    use host::FetchScope;
    use repository::PlatformCredentialRepository;

    user.require_workspace(&ws)?;
    let platform = live_platform(&platform)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;
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
            .get_platform_token(&workspace, platform.as_str())
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .ok_or_else(|| {
                ApiError::bad_request(format!(
                    "no {} bot token set for this workspace",
                    platform.as_str()
                ))
            })?;
        host::decrypt_secret(master, &enc).map_err(|e| ApiError::Internal(e.to_string()))?
    };

    // Build the fetcher (errors if the platform's feature isn't compiled in).
    let fetcher = host::make_platform_fetcher(platform, token, body.scope_id.clone())
        .map_err(ApiError::bad_request)?;

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

    // Track the sync as a job (ADR-013/040) so platform ingestion shows in the
    // Jobs view. Recorded (Running) before the fetch; finalized below with
    // progress = stored / fetched.
    let scope_label = match &scope {
        FetchScope::Workspace => format!("{} · all channels", platform.as_str()),
        FetchScope::Channels(c) => format!("{} · {} channel(s)", platform.as_str(), c.len()),
        _ => format!("{} sync", platform.as_str()),
    };
    let mut job = domain::Job::record(
        domain::JobId::parse(format!("job_sync_{}_{}", platform.as_str(), now))
            .map_err(|e| ApiError::bad_request(e.to_string()))?,
        workspace.clone(),
        domain::JobType::Sync,
        now,
    )
    .with_scope(scope_label)
    .with_creation_source("manual")
    .with_date_range(start, now);
    let _ = job.start(now);

    // Fetch + persist via the shared helper (also used by the scheduler).
    let report = {
        use repository::JobRepository;
        let repo = state.repo.lock().expect("repo mutex");
        let _ = repo.create_job(&job);
        match host::sync_into_store(&*fetcher, &*repo, &workspace, &scope, start, now) {
            Ok(r) => {
                job.set_progress(r.stored as u32, r.fetched as u32, crate::auth::now_secs());
                let _ = job.complete(0, crate::auth::now_secs());
                let _ = repo.update_job(&job);
                // Per-channel fetch failures are soft-fails (ADR-041/097): the
                // sync succeeds over what it could read, and each unreadable
                // channel is recorded for the Errors view (ADR-133/031).
                for (channel, message) in &r.errors {
                    crate::errors::record_operational_error(
                        &repo,
                        &workspace,
                        "sync",
                        domain::FailureClass::Unknown,
                        Some(channel.clone()),
                        format!("could not read channel during {} sync: {message}", platform.as_str()),
                    );
                }
                r
            }
            Err(e) => {
                let _ = job.fail(domain::FailureClass::Unknown, 0, crate::auth::now_secs());
                let _ = repo.update_job(&job);
                crate::errors::record_operational_error(
                    &repo,
                    &workspace,
                    "sync",
                    domain::FailureClass::Unknown,
                    None,
                    format!("{} sync failed: {e}", platform.as_str()),
                );
                return Err(ApiError::Internal(e.to_string()));
            }
        }
    };

    Ok(Json(SyncResponse {
        channel_ids: report.channel_ids,
        fetched: report.fetched,
        stored: report.stored,
        errors: report
            .errors
            .into_iter()
            .map(|(channel, message)| SyncErrorDto { channel, message })
            .collect(),
    }))
}

/// A server (Discord guild) the bot can reach.
#[derive(Serialize)]
pub struct ServerDto {
    pub id: String,
    pub name: String,
}

/// `GET /workspaces/:ws/connections/:platform/servers` — the servers the stored
/// bot token can reach (WSP-006), so the dashboard offers a picker instead of a
/// pasted guild id. Discord → the bot's guilds; Slack → empty (token is
/// workspace-scoped).
pub async fn servers(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, platform)): Path<(String, String)>,
) -> Result<Json<Vec<ServerDto>>, ApiError> {
    use repository::PlatformCredentialRepository;
    user.require_workspace(&ws)?;
    let platform = live_platform(&platform)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;

    let token = {
        let Some(master) = state.master_key() else {
            return Err(ApiError::bad_request(
                "server has no encryption key configured (set LLM_CONFIG_KEY)".to_string(),
            ));
        };
        let repo = state.repo.lock().expect("repo mutex");
        let enc = repo
            .get_platform_token(&workspace, platform.as_str())
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .ok_or_else(|| {
                ApiError::bad_request(format!(
                    "no {} bot token set for this workspace",
                    platform.as_str()
                ))
            })?;
        host::decrypt_secret(master, &enc).map_err(|e| ApiError::Internal(e.to_string()))?
    };

    let servers = host::list_servers(platform, token).map_err(ApiError::bad_request)?;
    Ok(Json(
        servers
            .into_iter()
            .map(|s| ServerDto { id: s.id, name: s.name })
            .collect(),
    ))
}

/// `?scope_id=<guild>` — the Discord guild (server) id to browse; Slack ignores it.
#[derive(Deserialize)]
pub struct ChannelsQuery {
    #[serde(default)]
    pub scope_id: Option<String>,
}

/// One browsable channel, grouped by category where the platform has them.
#[derive(Serialize)]
pub struct ChannelDto {
    pub id: String,
    pub name: String,
    /// Discord category name; `null` for Slack / uncategorized channels.
    pub category: Option<String>,
    /// Discord category id (stable scope target for a category schedule, ADR-011);
    /// `null` for Slack / uncategorized channels.
    pub category_id: Option<String>,
}

/// `GET /workspaces/:ws/connections/:platform/channels?scope_id=<guild>` — the
/// source's browsable channel directory (WSP-006): every summarizable channel
/// with its category, so the dashboard can present the server's channels for
/// point-and-click selection instead of typing ids.
pub async fn channels(
    State(state): State<AppState>,
    user: AuthUser,
    Path((ws, platform)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<ChannelsQuery>,
) -> Result<Json<Vec<ChannelDto>>, ApiError> {
    use repository::PlatformCredentialRepository;
    user.require_workspace(&ws)?;
    let platform = live_platform(&platform)?;
    let workspace =
        domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))?;

    let token = {
        let Some(master) = state.master_key() else {
            return Err(ApiError::bad_request(
                "server has no encryption key configured (set LLM_CONFIG_KEY)".to_string(),
            ));
        };
        let repo = state.repo.lock().expect("repo mutex");
        let enc = repo
            .get_platform_token(&workspace, platform.as_str())
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .ok_or_else(|| {
                ApiError::bad_request(format!(
                    "no {} bot token set for this workspace",
                    platform.as_str()
                ))
            })?;
        host::decrypt_secret(master, &enc).map_err(|e| ApiError::Internal(e.to_string()))?
    };

    let fetcher = host::make_platform_fetcher(platform, token, q.scope_id.clone())
        .map_err(ApiError::bad_request)?;
    let dir = fetcher.channel_directory().map_err(ApiError::bad_request)?;
    Ok(Json(
        dir.into_iter()
            .map(|c| ChannelDto {
                id: c.id.as_str().to_string(),
                name: c.name,
                category: c.category,
                category_id: c.category_id,
            })
            .collect(),
    ))
}
