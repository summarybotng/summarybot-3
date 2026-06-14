//! Per-tenant delivery plugin admin (ADR-126, two-layer plugin model).
//!
//! The **tenant** half of plugin configuration: a tenant admin (ManageSettings)
//! enables a plugin and configures/connects its account-level credentials once
//! (the `Tenant`-scoped fields of the sink descriptor). Workspaces then add
//! destinations carrying only the `Workspace`-scoped target. Credentials are
//! stored encrypted with the operator master key, exactly like
//! `tenant_llm_config`; secret fields are never echoed back.
//!
//! Authorization + tenant parsing + audit reuse the helpers in [`crate::tenancy`].

use crate::auth::{now_secs, AuthUser};
use crate::tenancy::{audit, authorize, parse_tenant};
use crate::{ApiError, AppState};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use domain::Permission;
use host::{FieldHint, SinkDescriptor};
use repository::{TenantPlugin, TenantPluginRepository};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One config field exposed to the admin form (never carries a secret value).
#[derive(Serialize)]
pub struct PluginFieldDto {
    pub name: String,
    pub label: String,
    pub secret: bool,
    pub required: bool,
}

fn field_dto(f: &host::FieldSpec) -> PluginFieldDto {
    PluginFieldDto {
        name: f.name.to_string(),
        label: f.label.to_string(),
        secret: f.secret,
        required: f.required,
    }
}

/// A plugin's tenant-level state for the admin UI.
#[derive(Serialize)]
pub struct TenantPluginDto {
    pub kind: String,
    pub display_name: String,
    /// Fields the tenant configures once (credentials/connection).
    pub tenant_fields: Vec<PluginFieldDto>,
    /// Fields a workspace sets per destination (the target) — shown for context.
    pub workspace_fields: Vec<PluginFieldDto>,
    pub enabled: bool,
    /// All required tenant fields present (always true when there are none).
    pub configured: bool,
    /// OAuth plugins: a refresh token has been captured via Connect.
    pub connected: bool,
    /// Whether this plugin uses an OAuth "Connect" button (vs. typed config).
    pub supports_connect: bool,
    /// Non-secret one-line summary of the configured credentials.
    pub hint: Option<String>,
    /// ADR-131: a platform operator has disabled this plugin for the tenant —
    /// read-only for tenant admins (they can't override it); the operator toggle.
    pub operator_disabled: bool,
}

/// Whether a plugin captures its credential via the OAuth connect flow
/// (Google Drive — ADR-126; Confluence — ADR-132).
pub(crate) fn supports_connect(kind: &str) -> bool {
    kind == "gdrive" || kind == "confluence"
}

fn descriptor(kind: &str) -> Option<SinkDescriptor> {
    host::sink_descriptors().into_iter().find(|d| d.id == kind)
}

/// Decrypt a tenant plugin's stored config blob into a JSON object map.
fn decode_tenant_config(master: Option<&[u8; 32]>, enc: &str) -> Option<Map<String, Value>> {
    let plain = host::decrypt_secret(master?, enc).ok()?;
    serde_json::from_str::<Value>(&plain)
        .ok()
        .and_then(|v| v.as_object().cloned())
}

/// Build the DTO for one descriptor given the tenant's stored row (if any) and
/// the decrypted config (if readable). Never includes secret values.
fn to_dto(
    desc: &SinkDescriptor,
    row: Option<&TenantPlugin>,
    config: &Map<String, Value>,
) -> TenantPluginDto {
    let configured = desc
        .tenant_fields()
        .filter(|f| f.required)
        .all(|f| config.get(f.name).and_then(Value::as_str).is_some_and(|v| !v.is_empty()));
    // Hint: the non-secret tenant fields with a Full hint, joined.
    let hint_parts: Vec<String> = desc
        .tenant_fields()
        .filter(|f| matches!(f.hint, FieldHint::Full))
        .filter_map(|f| config.get(f.name).and_then(Value::as_str))
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect();
    let hint = (!hint_parts.is_empty()).then(|| hint_parts.join(" · "));
    TenantPluginDto {
        kind: desc.id.to_string(),
        display_name: desc.display_name.to_string(),
        tenant_fields: desc.tenant_fields().map(field_dto).collect(),
        workspace_fields: desc.workspace_fields().map(field_dto).collect(),
        enabled: row.map(|r| r.enabled).unwrap_or(false),
        configured,
        connected: row.map(|r| r.connected).unwrap_or(false),
        supports_connect: supports_connect(desc.id),
        hint,
        operator_disabled: row.map(|r| r.operator_disabled).unwrap_or(false),
    }
}

/// `GET /tenants/:tenant/plugins` — every compiled sink plugin with this tenant's
/// enablement/config state (ManageSettings). Secret values are never returned.
pub async fn list_plugins(
    State(state): State<AppState>,
    user: AuthUser,
    Path(tenant): Path<String>,
) -> Result<Json<Vec<TenantPluginDto>>, ApiError> {
    let tenant = parse_tenant(tenant)?;
    let master = state.master_key();
    let repo = state.repo.lock().expect("repo mutex");
    authorize(&repo, &user.0.sub, &tenant, Permission::ManageSettings)?;
    let rows = repo.list_tenant_plugins(&tenant)?;
    let out = host::sink_descriptors()
        .iter()
        .map(|desc| {
            let row = rows.iter().find(|r| r.kind == desc.id);
            let config = row
                .and_then(|r| r.config_enc.as_deref())
                .and_then(|enc| decode_tenant_config(master, enc))
                .unwrap_or_default();
            to_dto(desc, row, &config)
        })
        .collect();
    Ok(Json(out))
}

/// Body for enabling/configuring a tenant plugin. `config` carries the
/// tenant-scoped fields; a secret field that is omitted/blank is **kept** (so
/// toggling enablement doesn't wipe a stored token).
#[derive(Deserialize)]
pub struct SetPluginRequest {
    pub enabled: bool,
    #[serde(default)]
    pub config: Map<String, Value>,
}

/// `PUT /tenants/:tenant/plugins/:kind` — enable + configure a plugin's
/// tenant-level credentials (ManageSettings). Validates against the descriptor's
/// tenant fields, encrypts with the master key, and preserves an existing
/// connected OAuth token. Returns the updated DTO.
pub async fn set_plugin(
    State(state): State<AppState>,
    user: AuthUser,
    Path((tenant, kind)): Path<(String, String)>,
    Json(body): Json<SetPluginRequest>,
) -> Result<Json<TenantPluginDto>, ApiError> {
    let tenant = parse_tenant(tenant)?;
    let desc = descriptor(&kind).ok_or_else(|| {
        ApiError::bad_request(format!("unknown plugin kind '{kind}' (not built into this server)"))
    })?;
    let master = state.master_key();

    let repo = state.repo.lock().expect("repo mutex");
    authorize(&repo, &user.0.sub, &tenant, Permission::ManageSettings)?;
    let current = repo.get_tenant_plugin(&tenant, &kind)?;
    let current_cfg = current
        .as_ref()
        .and_then(|r| r.config_enc.as_deref())
        .and_then(|enc| decode_tenant_config(master, enc))
        .unwrap_or_default();

    // Build the new tenant config from the descriptor's tenant fields.
    let mut new_cfg = Map::new();
    for f in desc.tenant_fields() {
        let provided = body
            .config
            .get(f.name)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty());
        match provided {
            Some(v) => {
                if f.name.contains("url") && !(v.starts_with("https://") || v.starts_with("http://"))
                {
                    return Err(ApiError::bad_request(format!("{} must be an http(s) URL", f.name)));
                }
                new_cfg.insert(f.name.to_string(), Value::String(v.to_string()));
            }
            // Omitted/blank: keep a secret's current value (e.g. an API token or
            // the OAuth refresh token) so editing other fields doesn't wipe it.
            None => {
                if f.secret {
                    if let Some(cur) = current_cfg.get(f.name) {
                        new_cfg.insert(f.name.to_string(), cur.clone());
                    }
                }
            }
        }
    }

    let config_enc = if new_cfg.is_empty() {
        None
    } else {
        let blob = Value::Object(new_cfg.clone()).to_string();
        let master = master.ok_or_else(|| {
            ApiError::bad_request("key encryption not configured on the server (set LLM_CONFIG_KEY)")
        })?;
        Some(host::encrypt_secret(master, &blob).map_err(|e| ApiError::Internal(e.to_string()))?)
    };
    // Preserve a captured OAuth token's connected flag across edits.
    let connected = current.as_ref().map(|r| r.connected).unwrap_or(false)
        && new_cfg.contains_key("refresh_token");

    let row = TenantPlugin {
        kind: kind.clone(),
        enabled: body.enabled,
        config_enc,
        connected,
        updated_at: now_secs(),
        // A tenant admin can't clear a platform operator's veto (ADR-131).
        operator_disabled: current.as_ref().map(|r| r.operator_disabled).unwrap_or(false),
    };
    repo.upsert_tenant_plugin(&tenant, &row)?;
    audit(
        &repo,
        &user.0.sub,
        "tenant.plugin.set",
        format!("{kind} enabled={}", body.enabled),
    );
    Ok(Json(to_dto(&desc, Some(&row), &new_cfg)))
}

/// `DELETE /tenants/:tenant/plugins/:kind` — disable + clear a plugin's tenant
/// config (ManageSettings). Idempotent 204.
pub async fn delete_plugin(
    State(state): State<AppState>,
    user: AuthUser,
    Path((tenant, kind)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let tenant = parse_tenant(tenant)?;
    let repo = state.repo.lock().expect("repo mutex");
    authorize(&repo, &user.0.sub, &tenant, Permission::ManageSettings)?;
    // A standing operator veto (ADR-131) must survive a tenant-admin clear: keep a
    // cleared, disabled row carrying the veto rather than deleting it outright.
    if repo.get_tenant_plugin(&tenant, &kind)?.is_some_and(|p| p.operator_disabled) {
        repo.upsert_tenant_plugin(
            &tenant,
            &TenantPlugin {
                kind: kind.clone(),
                enabled: false,
                config_enc: None,
                connected: false,
                updated_at: now_secs(),
                operator_disabled: true,
            },
        )?;
    } else {
        repo.delete_tenant_plugin(&tenant, &kind)?;
    }
    audit(&repo, &user.0.sub, "tenant.plugin.clear", kind);
    Ok(StatusCode::NO_CONTENT)
}

// ---- platform-operator controls (ADR-131) --------------------------------

/// Whether the caller is a configured platform operator.
#[derive(Serialize)]
pub struct OperatorStatusDto {
    pub is_operator: bool,
}

/// `GET /operator/status` — does the caller hold the operator capability? Lets
/// the dashboard reveal operator-only controls. Always 200 (just a boolean).
pub async fn operator_status(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<OperatorStatusDto>, ApiError> {
    Ok(Json(OperatorStatusDto {
        is_operator: state.is_operator(&user.0.sub),
    }))
}

/// Reject non-operators with 404 — an operator endpoint is invisible to everyone
/// else (don't confirm the route exists).
fn require_operator(state: &AppState, user: &AuthUser) -> Result<(), ApiError> {
    if state.is_operator(&user.0.sub) {
        Ok(())
    } else {
        Err(ApiError::NotFound)
    }
}

/// `GET /operator/tenants/:tenant/plugins` — every compiled plugin with this
/// tenant's `operator_disabled` state (operator-only).
pub async fn list_operator_plugins(
    State(state): State<AppState>,
    user: AuthUser,
    Path(tenant): Path<String>,
) -> Result<Json<Vec<TenantPluginDto>>, ApiError> {
    require_operator(&state, &user)?;
    let tenant = parse_tenant(tenant)?;
    let master = state.master_key();
    let repo = state.repo.lock().expect("repo mutex");
    let rows = repo.list_tenant_plugins(&tenant)?;
    let out = host::sink_descriptors()
        .iter()
        .map(|desc| {
            let row = rows.iter().find(|r| r.kind == desc.id);
            let config = row
                .and_then(|r| r.config_enc.as_deref())
                .and_then(|enc| decode_tenant_config(master, enc))
                .unwrap_or_default();
            to_dto(desc, row, &config)
        })
        .collect();
    Ok(Json(out))
}

/// Body for an operator's per-tenant plugin veto.
#[derive(Deserialize)]
pub struct SetOperatorPluginRequest {
    pub disabled: bool,
}

/// `PUT /operator/tenants/:tenant/plugins/:kind` `{ disabled }` — set/clear the
/// platform-operator veto for one (tenant, kind), preserving the tenant's own
/// enablement/config (operator-only; ADR-131). Audited under the operator.
pub async fn set_operator_plugin(
    State(state): State<AppState>,
    user: AuthUser,
    Path((tenant, kind)): Path<(String, String)>,
    Json(body): Json<SetOperatorPluginRequest>,
) -> Result<Json<TenantPluginDto>, ApiError> {
    require_operator(&state, &user)?;
    let tenant = parse_tenant(tenant)?;
    let Some(desc) = descriptor(&kind) else {
        return Err(ApiError::bad_request(format!("unknown plugin kind: {kind}")));
    };
    let master = state.master_key();
    let repo = state.repo.lock().expect("repo mutex");
    // Read-modify-write so the tenant's enablement/credentials are untouched.
    let current = repo.get_tenant_plugin(&tenant, &kind)?;
    let row = TenantPlugin {
        kind: kind.clone(),
        enabled: current.as_ref().map(|r| r.enabled).unwrap_or(false),
        config_enc: current.as_ref().and_then(|r| r.config_enc.clone()),
        connected: current.as_ref().map(|r| r.connected).unwrap_or(false),
        updated_at: now_secs(),
        operator_disabled: body.disabled,
    };
    repo.upsert_tenant_plugin(&tenant, &row)?;
    audit(
        &repo,
        &user.0.sub,
        "operator.plugin.veto",
        format!("{}/{} disabled={}", tenant.as_str(), kind, body.disabled),
    );
    let config = row
        .config_enc
        .as_deref()
        .and_then(|enc| decode_tenant_config(master, enc))
        .unwrap_or_default();
    Ok(Json(to_dto(&desc, Some(&row), &config)))
}
