//! LLM failure taxonomy (LEG-002) — the pure classification shared by the
//! host's retry/limiter machinery and the domain's model-fallback policy.
//!
//! Mapping an HTTP status/response to a class is host-side (it's I/O-shaped);
//! the *taxonomy itself* and its retry semantics are pure, so they live here and
//! both layers agree on them.

/// Stable classification of an LLM request failure (LEG-002 #1). Drives whether
/// a job auto-retries and what `failure_reason` the API surfaces (#3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// 429 — too many requests; transient, retryable.
    RateLimited,
    /// Out of credits / billing cap reached; permanent until topped up.
    QuotaExceeded,
    /// Malformed request, unknown model, etc.; retrying won't help.
    InvalidRequest,
    /// 5xx / provider outage; transient, retryable.
    ServiceUnavailable,
    /// Anything unclassified; treated as permanent (don't hammer blindly).
    Unknown,
}

impl FailureClass {
    /// Stable string for API responses / telemetry (LEG-002 #3).
    pub fn as_str(self) -> &'static str {
        match self {
            FailureClass::RateLimited => "rate_limited",
            FailureClass::QuotaExceeded => "quota_exceeded",
            FailureClass::InvalidRequest => "invalid_request",
            FailureClass::ServiceUnavailable => "service_unavailable",
            FailureClass::Unknown => "unknown",
        }
    }

    /// Transient classes auto-retry (LEG-002 #2); permanent ones do not.
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            FailureClass::RateLimited | FailureClass::ServiceUnavailable
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_transient_classes_retry() {
        assert!(FailureClass::RateLimited.is_retryable());
        assert!(FailureClass::ServiceUnavailable.is_retryable());
        assert!(!FailureClass::QuotaExceeded.is_retryable());
        assert!(!FailureClass::InvalidRequest.is_retryable());
        assert!(!FailureClass::Unknown.is_retryable());
    }

    #[test]
    fn as_str_is_stable() {
        assert_eq!(FailureClass::RateLimited.as_str(), "rate_limited");
        assert_eq!(
            FailureClass::ServiceUnavailable.as_str(),
            "service_unavailable"
        );
    }
}
