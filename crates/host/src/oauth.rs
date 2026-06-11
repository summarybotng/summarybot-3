//! OAuth 2.0 authorization-code flow — host I/O (PRD §6.1; ADR-126 credential
//! seam). Feature-gated (`oauth`) so the default build stays network-free.
//!
//! Provides the side-effecting half of [`domain::oauth`]: PKCE (S256), a
//! **stateless signed `state`** (HMAC over the request context, so no
//! server-side session store is needed and it survives restarts), and the
//! network exchanges (code→tokens, refresh, userinfo). The pure authorize-URL
//! construction lives in the domain.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use domain::OAuthProvider;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// Tokens returned by the provider's token endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthTokens {
    pub access_token: String,
    /// Present only when the provider issues one (e.g. Google with
    /// `access_type=offline` on first consent).
    pub refresh_token: Option<String>,
    /// Lifetime of the access token in seconds, if reported.
    pub expires_in: Option<i64>,
}

/// A URL-safe random token (`n_bytes` of entropy) — used for PKCE verifiers and
/// state nonces.
pub fn random_url_token(n_bytes: usize) -> Result<String, String> {
    let mut buf = vec![0u8; n_bytes];
    getrandom::getrandom(&mut buf).map_err(|e| e.to_string())?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

/// PKCE S256 challenge for a verifier (RFC 7636): `base64url(sha256(verifier))`.
pub fn pkce_challenge(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hasher.finalize())
}

/// Sign an opaque `state` payload with the server key so the callback can trust
/// it without a server-side store: `base64url(payload).base64url(hmac(payload))`.
pub fn sign_state(key: &[u8], payload: &str) -> String {
    let p = URL_SAFE_NO_PAD.encode(payload.as_bytes());
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac key");
    mac.update(p.as_bytes());
    let sig = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    format!("{p}.{sig}")
}

/// Verify a signed `state` and return its payload, or `None` if tampered.
/// Constant-time signature check via `verify_slice`.
pub fn verify_state(key: &[u8], token: &str) -> Option<String> {
    let (p, sig) = token.split_once('.')?;
    let expected = URL_SAFE_NO_PAD.decode(sig).ok()?;
    let mut mac = HmacSha256::new_from_slice(key).ok()?;
    mac.update(p.as_bytes());
    mac.verify_slice(&expected).ok()?;
    let payload = URL_SAFE_NO_PAD.decode(p).ok()?;
    String::from_utf8(payload).ok()
}

/// Exchange an authorization `code` for tokens (RFC 6749 §4.1.3) with PKCE.
pub fn exchange_code(
    provider: &OAuthProvider,
    client_secret: &str,
    redirect_uri: &str,
    code: &str,
    code_verifier: &str,
) -> Result<OAuthTokens, String> {
    post_token(
        &provider.token_url,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", &provider.client_id),
            ("client_secret", client_secret),
            ("code_verifier", code_verifier),
        ],
    )
}

/// Refresh an access token (RFC 6749 §6).
pub fn refresh(
    provider: &OAuthProvider,
    client_secret: &str,
    refresh_token: &str,
) -> Result<OAuthTokens, String> {
    post_token(
        &provider.token_url,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", &provider.client_id),
            ("client_secret", client_secret),
        ],
    )
}

fn post_token(token_url: &str, form: &[(&str, &str)]) -> Result<OAuthTokens, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(20))
        .build();
    let resp = match agent
        .post(token_url)
        .set("Accept", "application/json")
        .send_form(form)
    {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let body = r.into_string().unwrap_or_default();
            return Err(format!(
                "token endpoint http {code}: {}",
                body.chars().take(200).collect::<String>()
            ));
        }
        Err(ureq::Error::Transport(t)) => return Err(format!("token transport error: {t}")),
    };
    let v: serde_json::Value = resp.into_json().map_err(|e| format!("token json: {e}"))?;
    let access_token = v
        .get("access_token")
        .and_then(|x| x.as_str())
        .ok_or("token response missing access_token")?
        .to_string();
    Ok(OAuthTokens {
        access_token,
        refresh_token: v
            .get("refresh_token")
            .and_then(|x| x.as_str())
            .map(str::to_string),
        expires_in: v.get("expires_in").and_then(|x| x.as_i64()),
    })
}

/// Fetch the signed-in user's profile (OIDC userinfo / provider `/users/@me`).
pub fn fetch_userinfo(userinfo_url: &str, access_token: &str) -> Result<serde_json::Value, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(20))
        .build();
    match agent
        .get(userinfo_url)
        .set("Authorization", &format!("Bearer {access_token}"))
        .call()
    {
        Ok(r) => r.into_json().map_err(|e| format!("userinfo json: {e}")),
        Err(ureq::Error::Status(code, _)) => Err(format!("userinfo http {code}")),
        Err(ureq::Error::Transport(t)) => Err(format!("userinfo transport error: {t}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_rfc7636_vector() {
        // RFC 7636 Appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            pkce_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn state_round_trips_and_rejects_tamper() {
        let key = b"server-key";
        let payload = r#"{"ws":"ws-1","verifier":"abc","provider":"google"}"#;
        let token = sign_state(key, payload);
        assert_eq!(verify_state(key, &token).as_deref(), Some(payload));
        // Wrong key rejected.
        assert_eq!(verify_state(b"other-key", &token), None);
        // Tampered payload rejected.
        let mut bad = token.clone();
        bad.insert(0, 'x');
        assert_eq!(verify_state(key, &bad), None);
    }

    #[test]
    fn random_tokens_are_unique_and_urlsafe() {
        let a = random_url_token(32).unwrap();
        let b = random_url_token(32).unwrap();
        assert_ne!(a, b);
        assert!(!a.contains('+') && !a.contains('/') && !a.contains('='));
    }
}
