//! Auth extractor, login handler, and the correlation-ID middleware
//! (PRD §6.1, §12.2 item 3 — the per-request correlation middleware lands here).

use crate::{ApiError, AppState};
use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderValue, Request, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use axum::Json;
use domain::{AccessClaims, DiscordProvider, EmailProvider, GoogleProvider, IdentityProvider};
use host::{new_correlation_id, verify_token, AuthError, AuthService};
use repository::SessionRepository;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Current unix seconds (the API's clock at the I/O edge).
pub(crate) fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// An authenticated caller, extracted from a verified `Bearer` access token.
/// Handlers that take this are guaranteed a valid, unexpired token.
pub struct AuthUser(pub AccessClaims);

impl AuthUser {
    /// 403 unless the token grants access to `workspace`.
    pub fn require_workspace(&self, workspace: &str) -> Result<(), ApiError> {
        if self.0.workspaces.iter().any(|w| w.as_str() == workspace) {
            Ok(())
        } else {
            Err(ApiError::Forbidden)
        }
    }
}

#[axum::async_trait]
impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let token = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or(ApiError::Unauthorized)?;
        let claims = verify_token(token, &state.signing_key, now_secs())
            .map_err(|_| ApiError::Unauthorized)?;
        Ok(AuthUser(claims))
    }
}

/// Stamp every response with a correlation id (generating one per request) so a
/// request can be followed end-to-end (§12.2 item 3).
pub async fn correlation_id(request: Request<axum::body::Body>, next: Next) -> Response {
    let cid = new_correlation_id()
        .map(|c| c.as_str().to_string())
        .unwrap_or_else(|_| "cid_unknown".to_string());
    let mut response = next.run(request).await;
    if let Ok(value) = HeaderValue::from_str(&cid) {
        response.headers_mut().insert("x-correlation-id", value);
    }
    response
}

/// Login body: a pre-verified provider identity (the OAuth redirect dance that
/// *produces* these claims is a Phase-5 network seam; the token issuance here is
/// real).
#[derive(Deserialize)]
pub struct LoginRequest {
    pub provider: String,
    pub subject: String,
    #[serde(default)]
    pub email: Option<String>,
    /// Workspaces this session should be granted.
    #[serde(default)]
    pub workspaces: Vec<String>,
}

#[derive(Serialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub user_id: String,
    pub access_expires_at: i64,
}

/// `POST /auth/login` — exchange verified provider claims for a token pair.
pub async fn login(
    State(state): State<AppState>,
    Json(body): Json<LoginRequest>,
) -> Result<Json<TokenResponse>, ApiError> {
    let provider: Box<dyn IdentityProvider> = match body.provider.as_str() {
        "discord" => Box::new(DiscordProvider),
        "google" => Box::new(GoogleProvider),
        "email" => Box::new(EmailProvider),
        other => return Err(ApiError::bad_request(format!("unknown provider: {other}"))),
    };
    let mut workspaces = Vec::with_capacity(body.workspaces.len());
    for w in &body.workspaces {
        workspaces.push(
            domain::WorkspaceId::parse(w.clone())
                .map_err(|e| ApiError::bad_request(e.to_string()))?,
        );
    }
    let claims = domain::ProviderClaims {
        subject: body.subject,
        email: body.email,
    };

    let repo = state.repo.lock().expect("repo mutex");
    let svc = AuthService::new(&*repo, &state.signing_key);
    let pair = svc
        .login(provider.as_ref(), &claims, workspaces, now_secs())
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(Json(TokenResponse {
        access_token: pair.access_token,
        refresh_token: pair.refresh_token,
        user_id: pair.user_id.as_str().to_string(),
        access_expires_at: pair.access_expires_at,
    }))
}

/// Refresh body: the opaque refresh token + the workspaces to re-grant on the
/// new access token (claims are minted fresh at issue time).
#[derive(Deserialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
    #[serde(default)]
    pub workspaces: Vec<String>,
}

/// `POST /auth/refresh` — exchange a refresh token for a new pair, **rotating**
/// the session (the presented token is revoked; a replay now fails). An invalid,
/// expired, or revoked token is a 401.
pub async fn refresh(
    State(state): State<AppState>,
    Json(body): Json<RefreshRequest>,
) -> Result<Json<TokenResponse>, ApiError> {
    let mut workspaces = Vec::with_capacity(body.workspaces.len());
    for w in &body.workspaces {
        workspaces.push(
            domain::WorkspaceId::parse(w.clone())
                .map_err(|e| ApiError::bad_request(e.to_string()))?,
        );
    }
    let repo = state.repo.lock().expect("repo mutex");
    let svc = AuthService::new(&*repo, &state.signing_key);
    let pair = svc
        .refresh(&body.refresh_token, workspaces, now_secs())
        .map_err(|e| match e {
            // A refused refresh token is an auth failure, not a bad request.
            AuthError::Refresh(_) => ApiError::Unauthorized,
            other => ApiError::bad_request(other.to_string()),
        })?;
    Ok(Json(TokenResponse {
        access_token: pair.access_token,
        refresh_token: pair.refresh_token,
        user_id: pair.user_id.as_str().to_string(),
        access_expires_at: pair.access_expires_at,
    }))
}

/// `POST /auth/logout` — revoke the caller's current session (the one that
/// minted this access token), so its refresh token can no longer rotate.
/// Idempotent: 204 whether or not a live session was found.
pub async fn logout(State(state): State<AppState>, user: AuthUser) -> Result<StatusCode, ApiError> {
    let repo = state.repo.lock().expect("repo mutex");
    repo.revoke_session(&user.0.session_id)?;
    Ok(StatusCode::NO_CONTENT)
}
