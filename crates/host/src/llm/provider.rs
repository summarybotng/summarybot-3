//! LLM provider abstraction + rate-limit header parsing (LEG-003).
//!
//! Different providers report their rate-limit budget with different headers;
//! this normalizes them into one [`RateLimitSnapshot`] so the shared coordinator
//! (LEG-001) is provider-agnostic. Header *parsing* is pure (testable with a
//! fake getter); the live HTTP fetch, credit-balance check and model-availability
//! pre-check (LEG-003 #3/#4) are host I/O, layered on top in Phase 3.

/// A point-in-time view of a provider's rate-limit budget, normalized across
/// providers (LEG-003 #1/#2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RateLimitSnapshot {
    /// Requests remaining in the current window, if the provider reports it.
    pub remaining: Option<u32>,
    /// Unix-seconds reset time, if the provider reports it as such.
    pub reset_at_unix: Option<i64>,
}

/// The LLM backends the rewrite speaks to. The abstraction (vs. hardcoding one)
/// is the LEG-003 requirement — adding a provider is adding a match arm here,
/// not branching through the coordinator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmProvider {
    Anthropic,
    OpenRouter,
}

impl LlmProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            LlmProvider::Anthropic => "anthropic",
            LlmProvider::OpenRouter => "openrouter",
        }
    }

    /// Parse this provider's rate-limit headers into a normalized snapshot.
    /// `get` is a **case-insensitive** header lookup the host supplies from the
    /// real response; keeping it a closure is what lets this stay pure.
    ///
    /// - **OpenRouter** uses `x-ratelimit-remaining` / `x-ratelimit-reset`
    ///   (LEG-003 #2), the latter as unix seconds.
    /// - **Anthropic** reports `anthropic-ratelimit-requests-remaining`; its
    ///   reset is an RFC-3339 timestamp, so unix conversion is deferred to the
    ///   host (left `None` here rather than mis-parsed).
    pub fn parse_rate_limit<'a>(self, get: impl Fn(&str) -> Option<&'a str>) -> RateLimitSnapshot {
        match self {
            LlmProvider::OpenRouter => RateLimitSnapshot {
                remaining: get("x-ratelimit-remaining").and_then(|v| v.trim().parse().ok()),
                reset_at_unix: get("x-ratelimit-reset").and_then(|v| v.trim().parse().ok()),
            },
            LlmProvider::Anthropic => RateLimitSnapshot {
                remaining: get("anthropic-ratelimit-requests-remaining")
                    .and_then(|v| v.trim().parse().ok()),
                reset_at_unix: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Build a case-insensitive getter over a fixed header map.
    fn getter(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.to_string()))
            .collect()
    }

    #[test]
    fn openrouter_headers_parse_remaining_and_reset() {
        let h = getter(&[
            ("X-RateLimit-Remaining", "42"),
            ("x-ratelimit-reset", "1717600000"),
        ]);
        let snap = LlmProvider::OpenRouter
            .parse_rate_limit(|k| h.get(&k.to_ascii_lowercase()).map(String::as_str));
        assert_eq!(snap.remaining, Some(42));
        assert_eq!(snap.reset_at_unix, Some(1_717_600_000));
    }

    #[test]
    fn anthropic_parses_remaining_defers_reset() {
        let h = getter(&[
            ("anthropic-ratelimit-requests-remaining", "7"),
            ("anthropic-ratelimit-requests-reset", "2026-06-05T12:00:00Z"),
        ]);
        let snap = LlmProvider::Anthropic
            .parse_rate_limit(|k| h.get(&k.to_ascii_lowercase()).map(String::as_str));
        assert_eq!(snap.remaining, Some(7));
        // RFC-3339 reset is host-converted; not mis-parsed as unix here.
        assert_eq!(snap.reset_at_unix, None);
    }

    #[test]
    fn missing_or_garbage_headers_yield_none() {
        let empty = getter(&[]);
        let snap = LlmProvider::OpenRouter
            .parse_rate_limit(|k| empty.get(&k.to_ascii_lowercase()).map(String::as_str));
        assert_eq!(snap, RateLimitSnapshot::default());

        let garbage = getter(&[("x-ratelimit-remaining", "lots")]);
        let snap = LlmProvider::OpenRouter
            .parse_rate_limit(|k| garbage.get(&k.to_ascii_lowercase()).map(String::as_str));
        assert_eq!(snap.remaining, None);
    }
}
