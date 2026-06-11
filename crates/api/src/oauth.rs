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

use crate::auth::now_secs;
use crate::{ApiError, AppState};
use axum::extract::{Path, Query, State};
use axum::response::Redirect;
use domain::{DiscordProvider, GoogleProvider, IdentityProvider, OAuthProvider, ProviderClaims};
use host::AuthService;
use serde::Deserialize;
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
