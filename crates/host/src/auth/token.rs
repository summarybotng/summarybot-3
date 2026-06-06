//! Token primitives for the auth seam: random generation, SHA-256 hashing, and
//! HS256 JWT signing/verification.
//!
//! All crypto is delegated to vetted pure-Rust crates (`sha2`, `hmac`); the only
//! hand-written parts are the JWT *envelope* (base64url framing) and hex
//! encoding, neither of which is a cryptographic primitive. Domain claims are
//! mapped to/from a private JSON shape here so the domain stays
//! serialization-free.

use super::AuthError;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use domain::{AccessClaims, CorrelationId, RefreshTokenHash, SessionId, UserId, WorkspaceId};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// Bytes of entropy in a refresh token (256 bits) and in a generated id.
const TOKEN_BYTES: usize = 32;
const ID_BYTES: usize = 16;

/// Fixed HS256 header, pre-encoded (`{"alg":"HS256","typ":"JWT"}`).
const JWT_HEADER_B64: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9";

/// JWT payload (private). Mirrors [`AccessClaims`] using the compact claim names
/// a JWT conventionally uses; mapped to/from the domain type at the edges.
#[derive(Serialize, Deserialize)]
struct Claims {
    sub: String,
    sid: String,
    ws: Vec<String>,
    iat: i64,
    exp: i64,
}

/// Generate a new opaque, URL-safe token (256 bits of entropy). Shared by
/// refresh tokens and invite tokens — both are bearer secrets stored only as a
/// hash.
pub(crate) fn generate_token() -> Result<String, AuthError> {
    let mut buf = [0u8; TOKEN_BYTES];
    getrandom::getrandom(&mut buf).map_err(|e| AuthError::Crypto(e.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

/// SHA-256 of an opaque token, hex-encoded — what gets stored and matched. The
/// raw token never touches the database. Shared by refresh + invite hashing.
pub(crate) fn sha256_hex(raw: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    hex(&hasher.finalize())
}

/// Refresh-token-specific hash: [`sha256_hex`] wrapped in the validated newtype.
pub(super) fn hash_token(raw: &str) -> Result<RefreshTokenHash, AuthError> {
    RefreshTokenHash::parse(sha256_hex(raw)).map_err(|e| AuthError::Crypto(e.to_string()))
}

pub(super) fn new_user_id() -> Result<UserId, AuthError> {
    UserId::parse(random_id("u_")?).map_err(|e| AuthError::Crypto(e.to_string()))
}

pub(super) fn new_session_id() -> Result<SessionId, AuthError> {
    SessionId::parse(random_id("sess_")?).map_err(|e| AuthError::Crypto(e.to_string()))
}

/// A fresh correlation id (PRD §12.2 item 3). The per-request middleware that
/// stamps and threads this lands with the HTTP layer in Phase 5; the generator
/// lives here so both share one implementation.
pub fn new_correlation_id() -> Result<CorrelationId, AuthError> {
    CorrelationId::parse(random_id("cid_")?).map_err(|e| AuthError::Crypto(e.to_string()))
}

fn random_id(prefix: &str) -> Result<String, AuthError> {
    let mut buf = [0u8; ID_BYTES];
    getrandom::getrandom(&mut buf).map_err(|e| AuthError::Crypto(e.to_string()))?;
    Ok(format!("{prefix}{}", hex(&buf)))
}

/// Sign domain claims into an HS256 JWT.
pub(super) fn sign_access(claims: &AccessClaims, key: &[u8]) -> Result<String, AuthError> {
    let payload = Claims {
        sub: claims.sub.as_str().to_string(),
        sid: claims.session_id.as_str().to_string(),
        ws: claims
            .workspaces
            .iter()
            .map(|w| w.as_str().to_string())
            .collect(),
        iat: claims.issued_at,
        exp: claims.expires_at,
    };
    let payload_json =
        serde_json::to_vec(&payload).map_err(|e| AuthError::Crypto(e.to_string()))?;
    let signing_input = format!("{JWT_HEADER_B64}.{}", URL_SAFE_NO_PAD.encode(payload_json));
    let sig = sign_hmac(key, signing_input.as_bytes())?;
    Ok(format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(sig)))
}

/// Verify an HS256 JWT and reconstruct the domain claims. The signature is
/// checked in constant time (via `Mac::verify_slice`) before the payload is
/// trusted, and expiry is enforced against `now`.
pub(super) fn verify_access(token: &str, key: &[u8], now: i64) -> Result<AccessClaims, AuthError> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(AuthError::InvalidToken("malformed token"));
    }
    if parts[0] != JWT_HEADER_B64 {
        return Err(AuthError::InvalidToken("unexpected header/alg"));
    }
    let signing_input = format!("{}.{}", parts[0], parts[1]);
    let sig = URL_SAFE_NO_PAD
        .decode(parts[2])
        .map_err(|_| AuthError::InvalidToken("bad signature encoding"))?;
    let mut mac = HmacSha256::new_from_slice(key).map_err(|e| AuthError::Crypto(e.to_string()))?;
    mac.update(signing_input.as_bytes());
    mac.verify_slice(&sig)
        .map_err(|_| AuthError::InvalidToken("signature mismatch"))?;

    let payload = URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|_| AuthError::InvalidToken("bad payload encoding"))?;
    let parsed: Claims =
        serde_json::from_slice(&payload).map_err(|_| AuthError::InvalidToken("bad claims json"))?;

    let mut workspaces = Vec::with_capacity(parsed.ws.len());
    for w in parsed.ws {
        workspaces
            .push(WorkspaceId::parse(w).map_err(|_| AuthError::InvalidToken("bad workspace id"))?);
    }
    let claims = AccessClaims {
        sub: UserId::parse(parsed.sub).map_err(|_| AuthError::InvalidToken("bad subject"))?,
        session_id: SessionId::parse(parsed.sid)
            .map_err(|_| AuthError::InvalidToken("bad session id"))?,
        workspaces,
        issued_at: parsed.iat,
        expires_at: parsed.exp,
    };
    if !claims.is_valid_at(now) {
        return Err(AuthError::InvalidToken("expired"));
    }
    Ok(claims)
}

fn sign_hmac(key: &[u8], msg: &[u8]) -> Result<Vec<u8>, AuthError> {
    let mut mac = HmacSha256::new_from_slice(key).map_err(|e| AuthError::Crypto(e.to_string()))?;
    mac.update(msg);
    Ok(mac.finalize().into_bytes().to_vec())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims(now: i64, ttl: i64) -> AccessClaims {
        AccessClaims::issue(
            UserId::parse("u-1").unwrap(),
            SessionId::parse("s-1").unwrap(),
            vec![WorkspaceId::parse("ws-1").unwrap()],
            now,
            ttl,
        )
    }

    #[test]
    fn sign_then_verify_roundtrips_claims() {
        let key = b"k";
        let c = claims(1_000, 900);
        let jwt = sign_access(&c, key).unwrap();
        let back = verify_access(&jwt, key, 1_500).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn verify_rejects_expiry() {
        let key = b"k";
        let jwt = sign_access(&claims(0, 900), key).unwrap();
        assert!(matches!(
            verify_access(&jwt, key, 900),
            Err(AuthError::InvalidToken("expired"))
        ));
    }

    #[test]
    fn verify_rejects_wrong_key() {
        let jwt = sign_access(&claims(0, 900), b"key-a").unwrap();
        assert!(matches!(
            verify_access(&jwt, b"key-b", 1),
            Err(AuthError::InvalidToken("signature mismatch"))
        ));
    }

    #[test]
    fn verify_rejects_tampered_signature() {
        let key = b"k";
        let mut jwt = sign_access(&claims(0, 900), key).unwrap();
        let last = jwt.pop().unwrap();
        jwt.push(if last == 'A' { 'B' } else { 'A' });
        assert!(matches!(
            verify_access(&jwt, key, 1),
            Err(AuthError::InvalidToken(_))
        ));
    }

    #[test]
    fn verify_rejects_malformed() {
        assert!(matches!(
            verify_access("not.a.jwt.token", b"k", 0),
            Err(AuthError::InvalidToken("malformed token"))
        ));
    }

    #[test]
    fn hashing_is_stable_and_token_generation_is_unique() {
        assert_eq!(hash_token("abc").unwrap(), hash_token("abc").unwrap());
        assert_ne!(generate_token().unwrap(), generate_token().unwrap());
    }

    #[test]
    fn correlation_ids_are_unique_and_prefixed() {
        let a = new_correlation_id().unwrap();
        let b = new_correlation_id().unwrap();
        assert_ne!(a.as_str(), b.as_str());
        assert!(a.as_str().starts_with("cid_"));
    }
}
