//! At-rest encryption for stored secrets (ADR-125 Phase 2b).
//!
//! Reversible encryption — needed because the server must recover a tenant's
//! LLM API key to *use* it (unlike auth tokens, which are only hashed). Uses
//! AES-256-GCM with an operator **master key** (32 bytes, from config/KMS).
//! The wire form is `base64(nonce ‖ ciphertext+tag)` with a fresh random 96-bit
//! nonce per encryption. Keys are never logged; only ciphertext is persisted.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;

const NONCE_LEN: usize = 12;

/// Parse a 32-byte master key from 64 hex chars (e.g. `openssl rand -hex 32`).
/// Returns `None` if the input isn't exactly 64 hex digits.
pub fn parse_master_key(raw: &str) -> Option<[u8; 32]> {
    let raw = raw.trim();
    if raw.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&raw[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

/// Encrypt `plaintext` under `master`, returning `base64(nonce ‖ ciphertext)`.
pub fn encrypt_secret(master: &[u8; 32], plaintext: &str) -> Result<String> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(master));
    let mut nonce_bytes = [0u8; NONCE_LEN];
    getrandom::getrandom(&mut nonce_bytes).context("nonce entropy")?;
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plaintext.as_bytes())
        .map_err(|_| anyhow!("encryption failed"))?;
    let mut framed = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    framed.extend_from_slice(&nonce_bytes);
    framed.extend_from_slice(&ciphertext);
    Ok(STANDARD.encode(framed))
}

/// Decrypt a value produced by [`encrypt_secret`] under the same `master`.
pub fn decrypt_secret(master: &[u8; 32], encoded: &str) -> Result<String> {
    let framed = STANDARD.decode(encoded).context("base64 decode")?;
    if framed.len() <= NONCE_LEN {
        return Err(anyhow!("ciphertext too short"));
    }
    let (nonce, ciphertext) = framed.split_at(NONCE_LEN);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(master));
    let plaintext = cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|_| anyhow!("decryption failed (wrong key or tampered ciphertext)"))?;
    String::from_utf8(plaintext).context("decrypted bytes are not UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(b: u8) -> [u8; 32] {
        [b; 32]
    }

    #[test]
    fn round_trips() {
        let k = key(7);
        let enc = encrypt_secret(&k, "sk-secret-123").unwrap();
        assert_ne!(enc, "sk-secret-123"); // actually encrypted
        assert_eq!(decrypt_secret(&k, &enc).unwrap(), "sk-secret-123");
    }

    #[test]
    fn nonce_makes_ciphertext_non_deterministic() {
        let k = key(1);
        assert_ne!(
            encrypt_secret(&k, "same").unwrap(),
            encrypt_secret(&k, "same").unwrap()
        );
    }

    #[test]
    fn wrong_key_fails_to_decrypt() {
        let enc = encrypt_secret(&key(1), "secret").unwrap();
        assert!(decrypt_secret(&key(2), &enc).is_err());
    }

    #[test]
    fn master_key_parse() {
        assert_eq!(parse_master_key(&"ab".repeat(32)), Some([0xab; 32]));
        assert_eq!(parse_master_key("tooshort"), None);
        assert_eq!(parse_master_key(&"zz".repeat(32)), None); // not hex
    }

    #[test]
    fn garbage_ciphertext_is_rejected() {
        assert!(decrypt_secret(&key(1), "not-base64!!").is_err());
        assert!(decrypt_secret(&key(1), "AAAA").is_err()); // too short
    }
}
