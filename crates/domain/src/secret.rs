//! Secret wrapper (PRD §12.2 item 3: secrets handling).
//!
//! Wraps a sensitive value (signing key, OAuth client secret, refresh token)
//! so it cannot leak into logs or error messages: `Debug` is redacted and there
//! is deliberately **no** `Display`. The inner value is reachable only through
//! the explicit, greppable [`Secret::expose_secret`] call site.

/// A value that must never be logged. Construct with [`Secret::new`]; read the
/// inner value only via [`Secret::expose_secret`] (named so leaks are easy to
/// audit by grep). `Debug` prints a fixed redaction marker, never the value.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret<T>(T);

impl<T> Secret<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// Reach the wrapped secret. Every use is an explicit, auditable decision —
    /// keep these call sites minimal and never pass the result to a logger.
    pub fn expose_secret(&self) -> &T {
        &self.0
    }

    /// Consume the wrapper, returning the inner secret.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> std::fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret([REDACTED])")
    }
}

impl<T> From<T> for Secret<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_the_value() {
        let key = Secret::new("super-secret-signing-key".to_string());
        let rendered = format!("{key:?}");
        assert_eq!(rendered, "Secret([REDACTED])");
        assert!(!rendered.contains("super-secret"));
    }

    #[test]
    fn debug_redacts_inside_a_struct() {
        // The common leak path: a config struct derives Debug. The secret field
        // must still be hidden.
        // Fields are exercised through the derived `Debug` only.
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Config {
            name: String,
            client_secret: Secret<String>,
        }
        let cfg = Config {
            name: "discord".to_string(),
            client_secret: Secret::new("hunter2".to_string()),
        };
        let rendered = format!("{cfg:?}");
        assert!(rendered.contains("discord"));
        assert!(!rendered.contains("hunter2"));
        assert!(rendered.contains("REDACTED"));
    }

    #[test]
    fn expose_secret_returns_the_value() {
        let s = Secret::new(42u32);
        assert_eq!(*s.expose_secret(), 42);
        assert_eq!(s.into_inner(), 42);
    }
}
