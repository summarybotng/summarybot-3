//! Summary delivery destinations endpoints (DSH-010/011; ADR-126 plugin sinks).
//!
//! Manage where a workspace's summaries are delivered *besides* the always-on
//! dashboard. Sinks are plugins (webhook, Confluence, …) described by a config
//! schema; this layer is schema-driven — it validates submitted config against
//! the plugin descriptor, stores it as one encrypted JSON blob, and never echoes
//! secret fields back (only a per-field non-secret hint). Workspace-scoped +
//! authenticated.

use crate::auth::AuthUser;
use crate::{ApiError, AppState};
use axum::extract::{Path, State};
use axum::Json;
use host::{FieldHint, SinkDescriptor};
use repository::{
    DestinationRepository, StoredDestination, TenantPluginRepository, WorkspaceRepository,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

/// A destination as shown to the client — never includes secret config values.
#[derive(Serialize)]
pub struct DestinationDto {
    pub id: String,
    pub kind: String,
    pub enabled: bool,
    /// Non-secret summary of the config (e.g. `https://hooks.slack.com · #ops`).
    pub hint: Option<String>,
}

/// A plugin descriptor exposed to the dashboard so it can render a config form.
#[derive(Serialize)]
pub struct PluginDto {
    pub id: String,
    pub display_name: String,
    pub fields: Vec<PluginFieldDto>,
}

#[derive(Serialize)]
pub struct PluginFieldDto {
    pub name: String,
    pub label: String,
    pub secret: bool,
    pub required: bool,
}

/// Create a destination: a plugin kind + its config object.
#[derive(Deserialize)]
pub struct CreateDestinationRequest {
    pub kind: String,
    #[serde(default)]
    pub config: serde_json::Map<String, Value>,
}

fn workspace(ws: String) -> Result<domain::WorkspaceId, ApiError> {
    domain::WorkspaceId::parse(ws).map_err(|e| ApiError::bad_request(e.to_string()))
}

fn descriptor(kind: &str) -> Option<SinkDescriptor> {
    host::sink_descriptors().into_iter().find(|d| d.id == kind)
}

/// Scheme+host of a URL, for a non-secret display hint.
fn host_only(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let host = rest.split('/').next().unwrap_or(rest);
    (!host.is_empty()).then(|| format!("{scheme}://{host}"))
}

/// Build the non-secret hint for a stored destination from its plugin schema.
fn hint(kind: &str, config: &Value, master: Option<&[u8; 32]>) -> Option<String> {
    let desc = descriptor(kind)?;
    let parts: Vec<String> = desc
        .fields
        .iter()
        .filter_map(|f| {
            let v = config.get(f.name).and_then(Value::as_str)?;
            match f.hint {
                FieldHint::Full => Some(v.to_string()),
                FieldHint::Host => host_only(v),
                FieldHint::None => None,
            }
        })
        .collect();
    let _ = master;
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn decrypt_config(row: &StoredDestination, master: Option<&[u8; 32]>) -> Option<Value> {
    let (enc, key) = (row.address_enc.as_deref()?, master?);
    let plain = host::decrypt_secret(key, enc).ok()?;
    serde_json::from_str(&plain).ok()
}

fn to_dto(row: &StoredDestination, master: Option<&[u8; 32]>) -> DestinationDto {
    let hint = decrypt_config(row, master).and_then(|c| hint(&row.kind, &c, master));
    DestinationDto {
        id: row.id.clone(),
        kind: row.kind.clone(),
        enabled: row.enabled,
        hint,
    }
}

/// `GET /workspaces/:ws/destinations/plugins` — sink plugins available in this
/// build, with their config schema (so the dashboard can render a form).
pub async fn list_plugins(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
) -> Result<Json<Vec<PluginDto>>, ApiError> {
    user.require_workspace(&ws)?;
    let _ = state;
    // Only the Workspace-scoped (target) fields are entered per destination; the
    // tenant-scoped credentials are configured by an admin under /tenants/:t/plugins.
    let plugins = host::sink_descriptors()
        .into_iter()
        .map(|d| PluginDto {
            id: d.id.to_string(),
            display_name: d.display_name.to_string(),
            fields: d
                .workspace_fields()
                .map(|f| PluginFieldDto {
                    name: f.name.to_string(),
                    label: f.label.to_string(),
                    secret: f.secret,
                    required: f.required,
                })
                .collect(),
        })
        .collect();
    Ok(Json(plugins))
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

/// `POST /workspaces/:ws/destinations` — add a sink destination (DSH-010).
pub async fn create_destination(
    State(state): State<AppState>,
    user: AuthUser,
    Path(ws): Path<String>,
    Json(body): Json<CreateDestinationRequest>,
) -> Result<Json<DestinationDto>, ApiError> {
    user.require_workspace(&ws)?;
    let workspace = workspace(ws)?;

    let desc = descriptor(&body.kind).ok_or_else(|| {
        ApiError::bad_request(format!("unsupported destination kind: {}", body.kind))
    })?;

    // Validate against the plugin's **workspace-scoped** schema only (the target);
    // tenant credentials are configured separately by an admin (two-layer model).
    let mut config = serde_json::Map::new();
    for f in desc.workspace_fields() {
        let raw = body
            .config
            .get(f.name)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        if raw.is_empty() {
            if f.required {
                return Err(ApiError::bad_request(format!(
                    "missing required field: {}",
                    f.name
                )));
            }
            continue;
        }
        if f.name.contains("url") && !(raw.starts_with("https://") || raw.starts_with("http://")) {
            return Err(ApiError::bad_request(format!(
                "{} must be an http(s) URL",
                f.name
            )));
        }
        config.insert(f.name.to_string(), Value::String(raw.to_string()));
    }

    // Config is stored encrypted; we need the master key to do so.
    let Some(master) = state.master_key() else {
        return Err(ApiError::bad_request(
            "server has no encryption key configured (set LLM_CONFIG_KEY) — cannot store destination config safely".to_string(),
        ));
    };
    let blob = Value::Object(config).to_string();
    let address_enc =
        host::encrypt_secret(master, &blob).map_err(|e| ApiError::Internal(e.to_string()))?;

    let row = StoredDestination {
        id: format!("dst_{}", unique_suffix()),
        kind: body.kind,
        address_enc: Some(address_enc),
        enabled: true,
        created_at: crate::auth::now_secs(),
    };
    let repo = state.repo.lock().expect("repo mutex");
    // Honor the tenant enablement gate: a workspace can't add a destination for a
    // plugin its tenant has explicitly disabled (ADR-126). Unprovisioned (dev)
    // workspaces and kinds the tenant hasn't touched are allowed (default-on).
    if let Some(tenant) = repo.find_workspace(&workspace)?.map(|w| w.tenant_id) {
        if let Some(tp) = repo.get_tenant_plugin(&tenant, &row.kind)? {
            if !tp.enabled {
                return Err(ApiError::bad_request(format!(
                    "the '{}' plugin is disabled for your tenant — enable it under Plugins first",
                    row.kind
                )));
            }
        }
    }
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

/// Result of a destination test send.
#[derive(Serialize)]
pub struct TestResult {
    pub ok: bool,
    pub detail: Option<String>,
}

/// `POST /workspaces/:ws/destinations/:id/test` — send a sample payload so a
/// user can confirm the destination works (DSH-010). Requires the plugin's
/// deliverer to be compiled in and the master key to decrypt the config.
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
    // Resolve the effective config exactly as a real delivery would: merge the
    // tenant's account credentials + inject any platform bot token, honoring an
    // explicit tenant disable (ADR-126).
    let config = decrypt_config(&row, master)
        .and_then(|config| host::resolve_destination_config(&*repo, &workspace, &row.kind, config, master));
    drop(repo);

    let Some(config) = config else {
        return Ok(Json(TestResult {
            ok: false,
            detail: Some(
                "destination config could not be read, or the plugin is disabled for your tenant"
                    .to_string(),
            ),
        }));
    };
    let Some(deliverer) = deliverers.iter().find(|d| d.id() == row.kind) else {
        return Ok(Json(TestResult {
            ok: false,
            detail: Some(format!(
                "no '{}' deliverer in this build (enable its cargo feature)",
                row.kind
            )),
        }));
    };
    let sample = host::RenderedSummary::from_text(
        "SummaryBot test delivery — your destination is configured correctly.",
    );
    match deliverer.deliver(&config, &sample) {
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
