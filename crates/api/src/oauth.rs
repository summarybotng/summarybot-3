//! OAuth 2.0 login endpoints (PRD §6.1; ADR-126) — the real authorization-code
//! flow that replaces the dev `POST /auth/login` claims seam. Feature-gated
//! (`oauth`); providers are configured from the environment.
//!
//! Flow: `GET /auth/oauth/:provider/start` mints a PKCE verifier + a stateless
//! HMAC-signed `state` (carrying the verifier + requested workspaces) and 302s
//! to the provider. `GET /auth/oauth/:provider/callback` verifies `state`,
//! exchanges the `code` for tokens, fetches the userinfo, maps it to a domain
//! identity, mints a session (reusing `AuthService::login`), and redirects back
//! to the SPA with the session in the URL fragment.

use crate::auth::{now_secs, AuthUser};
use crate::{ApiError, AppState};
use axum::extract::{Path, Query, State};
use axum::response::Redirect;
use axum::Json;
use domain::{
    DiscordProvider, GoogleProvider, IdentityProvider, OAuthProvider, Permission, ProviderClaims,
    TenantId,
};
use host::AuthService;
use repository::{TenantPlugin, TenantPluginRepository};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;

/// How long a `start` request's signed state stays valid (the user must finish
/// consent within this window).
const STATE_TTL_SECS: i64 = 600;

/// Resolve a provider's config from the environment, or `None` if unconfigured.
/// Needs `<PROVIDER>_CLIENT_ID` + `<PROVIDER>_CLIENT_SECRET`; the redirect URI is
/// `{OAUTH_REDIRECT_BASE}/auth/oauth/{name}/callback`.
fn provider_from_env(name: &str) -> Option<(OAuthProvider, String, String)> {
    let scopes = |s: &[&str]| s.iter().map(|x| x.to_string()).collect();
    let params = |p: &[(&str, &str)]| {
        p.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    };
    let (id_var, secret_var, mut provider) = match name {
        "google" => (
            "GOOGLE_CLIENT_ID",
            "GOOGLE_CLIENT_SECRET",
            OAuthProvider {
                name: name.to_string(),
                auth_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
                token_url: "https://oauth2.googleapis.com/token".into(),
                userinfo_url: Some("https://openidconnect.googleapis.com/v1/userinfo".into()),
                client_id: String::new(),
                scopes: scopes(&["openid", "email", "profile"]),
                extra_auth_params: params(&[("access_type", "offline"), ("prompt", "consent")]),
            },
        ),
        "discord" => (
            "DISCORD_CLIENT_ID",
            "DISCORD_CLIENT_SECRET",
            OAuthProvider {
                name: name.to_string(),
                auth_url: "https://discord.com/api/oauth2/authorize".into(),
                token_url: "https://discord.com/api/oauth2/token".into(),
                userinfo_url: Some("https://discord.com/api/users/@me".into()),
                client_id: String::new(),
                scopes: scopes(&["identify", "email"]),
                extra_auth_params: params(&[]),
            },
        ),
        _ => return None,
    };
    provider.client_id = env::var(id_var).ok().filter(|s| !s.is_empty())?;
    let client_secret = env::var(secret_var).ok().filter(|s| !s.is_empty())?;
    let base = env::var("OAUTH_REDIRECT_BASE").unwrap_or_else(|_| "http://localhost:8080".into());
    let redirect_uri = format!("{}/auth/oauth/{name}/callback", base.trim_end_matches('/'));
    Some((provider, client_secret, redirect_uri))
}

#[derive(Deserialize)]
pub struct StartQuery {
    /// Comma-separated workspaces this session should be granted.
    #[serde(default)]
    pub workspaces: String,
}

/// `GET /auth/oauth/:provider/start?workspaces=ws-a,ws-b` → 302 to the provider.
pub async fn start(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(q): Query<StartQuery>,
) -> Result<Redirect, ApiError> {
    let (cfg, _secret, redirect_uri) = provider_from_env(&provider).ok_or_else(|| {
        ApiError::bad_request(format!("oauth provider '{provider}' not configured"))
    })?;

    let verifier = host::oauth::random_url_token(32).map_err(ApiError::Internal)?;
    let challenge = host::oauth::pkce_challenge(&verifier);
    let nonce = host::oauth::random_url_token(12).map_err(ApiError::Internal)?;
    let payload = serde_json::json!({
        "p": provider,
        "v": verifier,
        "ws": q.workspaces,
        "n": nonce,
        "exp": now_secs() + STATE_TTL_SECS,
    })
    .to_string();
    let signed = host::oauth::sign_state(state.signing_key.expose_secret(), &payload);
    Ok(Redirect::to(&cfg.authorize_url(
        &redirect_uri,
        &signed,
        &challenge,
    )))
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub error: Option<String>,
}

/// `GET /auth/oauth/:provider/callback?code&state` → exchange + login, then
/// redirect to the SPA with the session in the fragment.
pub async fn callback(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(q): Query<CallbackQuery>,
) -> Result<Redirect, ApiError> {
    if let Some(err) = q.error.filter(|e| !e.is_empty()) {
        return Err(ApiError::bad_request(format!(
            "oauth provider error: {err}"
        )));
    }
    let (cfg, secret, redirect_uri) = provider_from_env(&provider).ok_or_else(|| {
        ApiError::bad_request(format!("oauth provider '{provider}' not configured"))
    })?;

    // Verify the signed state (CSRF + carries the verifier and requested ws).
    let payload = host::oauth::verify_state(state.signing_key.expose_secret(), &q.state)
        .ok_or_else(|| ApiError::bad_request("invalid oauth state".to_string()))?;
    let claims_state: HashMap<String, serde_json::Value> = serde_json::from_str(&payload)
        .map_err(|_| ApiError::bad_request("bad state".to_string()))?;
    let state_field = |k: &str| claims_state.get(k).and_then(|v| v.as_str()).unwrap_or("");
    if state_field("p") != provider {
        return Err(ApiError::bad_request("state/provider mismatch".to_string()));
    }
    let exp = claims_state
        .get("exp")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if now_secs() > exp {
        return Err(ApiError::bad_request("oauth state expired".to_string()));
    }
    let verifier = state_field("v").to_string();

    // Exchange the code, then resolve the signed-in identity.
    let tokens = host::oauth::exchange_code(&cfg, &secret, &redirect_uri, &q.code, &verifier)
        .map_err(ApiError::Internal)?;
    let userinfo = host::oauth::fetch_userinfo(
        cfg.userinfo_url.as_deref().unwrap_or_default(),
        &tokens.access_token,
    )
    .map_err(ApiError::Internal)?;

    let (idp, claims) = map_identity(&provider, &userinfo)?;
    let workspaces = parse_workspaces(state_field("ws"))?;

    let pair = {
        let repo = state.repo.lock().expect("repo mutex");
        AuthService::new(&*repo, &state.signing_key)
            .login(idp.as_ref(), &claims, workspaces, now_secs())
            .map_err(|e| ApiError::bad_request(e.to_string()))?
    };

    // Hand the session to the SPA via the URL fragment (kept out of server logs
    // and Referer headers).
    let frag = serde_json::json!({
        "access_token": pair.access_token,
        "refresh_token": pair.refresh_token,
        "user_id": pair.user_id.as_str(),
        "workspaces": state_field("ws"),
    })
    .to_string();
    use base64::Engine as _;
    let blob = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(frag);
    Ok(Redirect::to(&format!("/#session={blob}")))
}

// ---- Per-tenant plugin connect flow (ADR-126) ------------------------------
//
// A tenant admin connects an OAuth-backed plugin (Google Drive) so its refresh
// token is captured server-side instead of pasted. The `connect` POST authorizes
// the admin and returns a consent URL whose signed state carries {tenant, kind,
// verifier}; the provider redirects to the fixed `/oauth/connect/callback`, which
// trusts that signed state, exchanges the code, and stores the refresh token in
// the tenant plugin config (`connected = true`).

/// The Google OAuth app (client id/secret) from the environment, if configured.
fn google_app() -> Option<(OAuthProvider, String)> {
    provider_from_env("google").map(|(p, secret, _login_redirect)| (p, secret))
}

/// The single fixed redirect URI registered for the connect flow.
fn connect_redirect_uri() -> String {
    let base = env::var("OAUTH_REDIRECT_BASE").unwrap_or_else(|_| "http://localhost:8080".into());
    format!("{}/oauth/connect/callback", base.trim_end_matches('/'))
}

#[derive(Serialize)]
pub struct ConnectUrl {
    /// The provider consent URL the SPA should navigate to.
    pub url: String,
}

/// `POST /tenants/:tenant/plugins/:kind/connect` — admin starts the OAuth connect
/// for a plugin and gets back the consent URL (ManageSettings).
pub async fn connect_plugin(
    State(state): State<AppState>,
    user: AuthUser,
    Path((tenant, kind)): Path<(String, String)>,
) -> Result<Json<ConnectUrl>, ApiError> {
    if !crate::plugins::supports_connect(&kind) {
        return Err(ApiError::bad_request(format!(
            "plugin '{kind}' has no connect flow"
        )));
    }
    let tenant_id = crate::tenancy::parse_tenant(tenant.clone())?;
    {
        let repo = state.repo.lock().expect("repo mutex");
        crate::tenancy::authorize(&repo, &user.0.sub, &tenant_id, Permission::ManageSettings)?;
    }
    let (mut cfg, _secret) = google_app().ok_or_else(|| {
        ApiError::bad_request("server has no GOOGLE_CLIENT_ID configured".to_string())
    })?;
    // Drive file-scope (create/manage only files the app made) + offline consent
    // so Google returns a refresh token.
    cfg.scopes = vec!["https://www.googleapis.com/auth/drive.file".to_string()];
    let redirect_uri = connect_redirect_uri();
    let verifier = host::oauth::random_url_token(32).map_err(ApiError::Internal)?;
    let challenge = host::oauth::pkce_challenge(&verifier);
    let nonce = host::oauth::random_url_token(12).map_err(ApiError::Internal)?;
    let payload = serde_json::json!({
        "t": tenant,
        "k": kind,
        "v": verifier,
        "n": nonce,
        "exp": now_secs() + STATE_TTL_SECS,
    })
    .to_string();
    let signed = host::oauth::sign_state(state.signing_key.expose_secret(), &payload);
    Ok(Json(ConnectUrl {
        url: cfg.authorize_url(&redirect_uri, &signed, &challenge),
    }))
}

/// `GET /oauth/connect/callback?code&state` — the provider redirect. Trusts the
/// HMAC-signed state (only an authorized admin could have minted it), exchanges
/// the code, and stores the captured refresh token in the tenant plugin config.
pub async fn connect_callback(
    State(state): State<AppState>,
    Query(q): Query<CallbackQuery>,
) -> Result<Redirect, ApiError> {
    if let Some(err) = q.error.filter(|e| !e.is_empty()) {
        return Err(ApiError::bad_request(format!("oauth provider error: {err}")));
    }
    let payload = host::oauth::verify_state(state.signing_key.expose_secret(), &q.state)
        .ok_or_else(|| ApiError::bad_request("invalid oauth state".to_string()))?;
    let st: HashMap<String, serde_json::Value> = serde_json::from_str(&payload)
        .map_err(|_| ApiError::bad_request("bad state".to_string()))?;
    let f = |k: &str| st.get(k).and_then(|v| v.as_str()).unwrap_or("");
    if now_secs() > st.get("exp").and_then(|v| v.as_i64()).unwrap_or(0) {
        return Err(ApiError::bad_request("oauth state expired".to_string()));
    }
    let (tenant_raw, kind, verifier) = (f("t"), f("k"), f("v"));
    if tenant_raw.is_empty() || kind.is_empty() || verifier.is_empty() {
        return Err(ApiError::bad_request("incomplete oauth state".to_string()));
    }
    let tenant = TenantId::parse(tenant_raw).map_err(|e| ApiError::bad_request(e.to_string()))?;

    let (cfg, secret) = google_app().ok_or_else(|| {
        ApiError::bad_request("server has no GOOGLE_CLIENT_ID configured".to_string())
    })?;
    let redirect_uri = connect_redirect_uri();
    let tokens = host::oauth::exchange_code(&cfg, &secret, &redirect_uri, &q.code, verifier)
        .map_err(ApiError::Internal)?;
    let refresh = tokens.refresh_token.filter(|t| !t.is_empty()).ok_or_else(|| {
        ApiError::bad_request(
            "Google did not return a refresh token — remove the app's prior access at \
             myaccount.google.com and reconnect".to_string(),
        )
    })?;

    // The tenant config for gdrive is just the refresh token (folder is a
    // workspace target). Encrypt + store, enable, and mark connected.
    let master = state.master_key().ok_or_else(|| {
        ApiError::bad_request("key encryption not configured on the server (set LLM_CONFIG_KEY)")
    })?;
    let blob = serde_json::json!({ "refresh_token": refresh }).to_string();
    let config_enc = host::encrypt_secret(master, &blob).map_err(|e| ApiError::Internal(e.to_string()))?;
    {
        let repo = state.repo.lock().expect("repo mutex");
        // Preserve a platform operator's veto (ADR-131) across a (re)connect — an
        // OAuth reconnect must not silently re-enable a plugin the operator disabled.
        let operator_disabled = repo
            .get_tenant_plugin(&tenant, kind)?
            .map(|p| p.operator_disabled)
            .unwrap_or(false);
        repo.upsert_tenant_plugin(
            &tenant,
            &TenantPlugin {
                kind: kind.to_string(),
                enabled: true,
                config_enc: Some(config_enc),
                connected: true,
                updated_at: now_secs(),
                operator_disabled,
            },
        )?;
        crate::tenancy::audit(&repo, &user_system(), "tenant.plugin.connected", kind.to_string());
    }
    Ok(Redirect::to(&format!("/#connected={kind}")))
}

/// The connect callback has no authenticated user (it's a provider redirect); the
/// signed state is the capability. Audit the capture as a system actor.
fn user_system() -> domain::UserId {
    domain::UserId::parse("system").expect("valid system user id")
}

/// Map a provider's userinfo to a domain identity provider + claims.
fn map_identity(
    provider: &str,
    userinfo: &serde_json::Value,
) -> Result<(Box<dyn IdentityProvider>, ProviderClaims), ApiError> {
    let s = |k: &str| userinfo.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let email = s("email");
    match provider {
        "google" => {
            let subject = s("sub").or_else(|| email.clone()).ok_or_else(|| {
                ApiError::bad_request("google userinfo missing sub/email".to_string())
            })?;
            Ok((Box::new(GoogleProvider), ProviderClaims { subject, email }))
        }
        "discord" => {
            let subject = s("id")
                .ok_or_else(|| ApiError::bad_request("discord userinfo missing id".to_string()))?;
            Ok((Box::new(DiscordProvider), ProviderClaims { subject, email }))
        }
        other => Err(ApiError::bad_request(format!("unknown provider: {other}"))),
    }
}

fn parse_workspaces(csv: &str) -> Result<Vec<domain::WorkspaceId>, ApiError> {
    csv.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|w| domain::WorkspaceId::parse(w).map_err(|e| ApiError::bad_request(e.to_string())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_identity_reads_google_and_discord() {
        let g = serde_json::json!({ "sub": "g-123", "email": "a@b.com" });
        let (_, claims) = map_identity("google", &g).unwrap();
        assert_eq!(claims.subject, "g-123");
        assert_eq!(claims.email.as_deref(), Some("a@b.com"));

        let d = serde_json::json!({ "id": "d-456", "email": "c@d.com" });
        let (_, claims) = map_identity("discord", &d).unwrap();
        assert_eq!(claims.subject, "d-456");
    }

    #[test]
    fn parse_workspaces_splits_and_validates() {
        assert_eq!(parse_workspaces("ws-a, ws-b").unwrap().len(), 2);
        assert!(parse_workspaces("").unwrap().is_empty());
    }
}
