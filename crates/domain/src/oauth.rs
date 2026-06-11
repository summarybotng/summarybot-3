//! OAuth 2.0 authorization-code flow — pure parts (PRD §6.1; ADR-126 credential
//! seam).
//!
//! This module is provider-agnostic and side-effect-free: it builds the
//! authorize URL (with PKCE + state) and models the provider's endpoints/scopes.
//! The network exchange (code→tokens, refresh, userinfo) and the crypto (PKCE
//! S256, signed state) are host I/O and live in `host::oauth`. Keeping URL
//! construction here means it's unit-tested without a network.

/// A configured OAuth provider (Google, Discord, …). Built from operator config
/// (client id/secret + the provider's well-known endpoints).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthProvider {
    /// Short id used in routes, e.g. `google`.
    pub name: String,
    /// Authorization endpoint (where the user is sent).
    pub auth_url: String,
    /// Token endpoint (code→tokens, refresh).
    pub token_url: String,
    /// Optional userinfo endpoint (to resolve the signed-in identity).
    pub userinfo_url: Option<String>,
    pub client_id: String,
    pub scopes: Vec<String>,
    /// Extra authorize-URL params (e.g. Google's `access_type=offline`,
    /// `prompt=consent` to obtain a refresh token).
    pub extra_auth_params: Vec<(String, String)>,
}

impl OAuthProvider {
    /// Build the authorization-request URL the user is redirected to. `state`
    /// guards against CSRF (verified on callback) and `code_challenge` is the
    /// PKCE S256 challenge (RFC 7636). Scopes are space-joined per RFC 6749.
    pub fn authorize_url(&self, redirect_uri: &str, state: &str, code_challenge: &str) -> String {
        let mut params: Vec<(&str, String)> = vec![
            ("response_type", "code".to_string()),
            ("client_id", self.client_id.clone()),
            ("redirect_uri", redirect_uri.to_string()),
            ("scope", self.scopes.join(" ")),
            ("state", state.to_string()),
            ("code_challenge", code_challenge.to_string()),
            ("code_challenge_method", "S256".to_string()),
        ];
        for (k, v) in &self.extra_auth_params {
            params.push((k.as_str(), v.clone()));
        }
        let query = params
            .iter()
            .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let sep = if self.auth_url.contains('?') {
            '&'
        } else {
            '?'
        };
        format!("{}{sep}{query}", self.auth_url)
    }
}

/// Percent-encode per RFC 3986 unreserved set: everything except
/// `A-Z a-z 0-9 - _ . ~` is `%XX`-escaped. Sufficient for query components.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn google() -> OAuthProvider {
        OAuthProvider {
            name: "google".into(),
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            userinfo_url: Some("https://openidconnect.googleapis.com/v1/userinfo".into()),
            client_id: "client-123.apps".into(),
            scopes: vec!["openid".into(), "email".into()],
            extra_auth_params: vec![("access_type".into(), "offline".into())],
        }
    }

    #[test]
    fn percent_encode_escapes_reserved_keeps_unreserved() {
        assert_eq!(percent_encode("aZ0-_.~"), "aZ0-_.~");
        assert_eq!(percent_encode("a b/c?d=e&f"), "a%20b%2Fc%3Fd%3De%26f");
        assert_eq!(percent_encode("https://x/y"), "https%3A%2F%2Fx%2Fy");
    }

    #[test]
    fn authorize_url_has_all_required_params_encoded() {
        let url = google().authorize_url("https://app/cb", "st-1", "chal-1");
        assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("client_id=client-123.apps"));
        assert!(url.contains("redirect_uri=https%3A%2F%2Fapp%2Fcb"));
        assert!(url.contains("scope=openid%20email"));
        assert!(url.contains("state=st-1"));
        assert!(url.contains("code_challenge=chal-1"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("access_type=offline"));
    }

    #[test]
    fn authorize_url_appends_with_amp_when_base_has_query() {
        let mut p = google();
        p.auth_url = "https://x/auth?foo=bar".into();
        let url = p.authorize_url("https://app/cb", "s", "c");
        assert!(url.contains("https://x/auth?foo=bar&response_type=code"));
    }
}
