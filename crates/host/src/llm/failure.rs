//! LLM retry policy + HTTP failure classification (LEG-002).
//!
//! The taxonomy itself ([`FailureClass`]) is pure and shared, so it lives in the
//! domain; this host module maps provider/HTTP signals onto it and owns the
//! retry-timing policy. Pure (no I/O/clock) — the host adapter applies the
//! decision (the actual waiting/requeue is its job).

// FailureClass is the shared pure taxonomy (domain); re-exported so existing
// `llm::FailureClass` call sites keep working.
pub use domain::FailureClass;

/// Classify from an HTTP status code. A coarse first pass; a provider adapter
/// may refine using the response body (e.g. distinguishing a 429 that is really
/// a quota/billing problem). `quota_exceeded` is matched on 402 (Payment
/// Required) which is what credit-metered providers return when out of funds.
pub fn classify_http_status(status: u16) -> FailureClass {
    match status {
        429 => FailureClass::RateLimited,
        402 => FailureClass::QuotaExceeded,
        400 | 404 | 422 => FailureClass::InvalidRequest,
        500..=599 => FailureClass::ServiceUnavailable,
        _ => FailureClass::Unknown,
    }
}

/// Exponential-backoff retry policy (LEG-002 #4). Pure value type; the host
/// schedules the actual delay/requeue.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub base_secs: i64,
    pub max_secs: i64,
    pub max_attempts: u32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            base_secs: 1,
            max_secs: 60,
            max_attempts: 5,
        }
    }
}

/// What to do after a failed attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    /// Retry after this many seconds.
    Retry { after_secs: i64 },
    /// Stop: permanent failure or attempts exhausted.
    GiveUp,
}

impl RetryPolicy {
    /// Backoff (secs) after `attempts_made` failures: `base * 2^(attempts_made-1)`,
    /// capped at `max_secs`. `attempts_made` is 1-based (1 = first failure).
    fn backoff_secs(&self, attempts_made: u32) -> i64 {
        let shift = attempts_made.saturating_sub(1).min(62);
        let factor = 1i64.checked_shl(shift).unwrap_or(i64::MAX);
        self.base_secs.saturating_mul(factor).min(self.max_secs)
    }

    /// Decide whether to retry, given the failure class, how many attempts have
    /// already failed, and any server `Retry-After` hint (secs). The chosen delay
    /// is the larger of the computed backoff and the server hint, so we never
    /// retry sooner than the provider asked.
    pub fn decide(
        &self,
        class: FailureClass,
        attempts_made: u32,
        retry_after_hint: Option<i64>,
    ) -> RetryDecision {
        if !class.is_retryable() || attempts_made >= self.max_attempts {
            return RetryDecision::GiveUp;
        }
        let backoff = self.backoff_secs(attempts_made);
        let after = retry_after_hint.map_or(backoff, |h| h.max(backoff));
        RetryDecision::Retry { after_secs: after }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_codes_map_to_classes() {
        assert_eq!(classify_http_status(429), FailureClass::RateLimited);
        assert_eq!(classify_http_status(402), FailureClass::QuotaExceeded);
        assert_eq!(classify_http_status(400), FailureClass::InvalidRequest);
        assert_eq!(classify_http_status(503), FailureClass::ServiceUnavailable);
        assert_eq!(classify_http_status(418), FailureClass::Unknown);
    }

    #[test]
    fn only_transient_classes_retry() {
        assert!(FailureClass::RateLimited.is_retryable());
        assert!(FailureClass::ServiceUnavailable.is_retryable());
        assert!(!FailureClass::QuotaExceeded.is_retryable());
        assert!(!FailureClass::InvalidRequest.is_retryable());
        assert!(!FailureClass::Unknown.is_retryable());
    }

    #[test]
    fn permanent_failures_give_up_immediately() {
        let p = RetryPolicy::default();
        assert_eq!(
            p.decide(FailureClass::InvalidRequest, 1, None),
            RetryDecision::GiveUp
        );
    }

    #[test]
    fn backoff_grows_exponentially_and_caps() {
        let p = RetryPolicy {
            base_secs: 1,
            max_secs: 60,
            max_attempts: 20,
        };
        assert_eq!(
            p.decide(FailureClass::RateLimited, 1, None),
            RetryDecision::Retry { after_secs: 1 }
        );
        assert_eq!(
            p.decide(FailureClass::RateLimited, 2, None),
            RetryDecision::Retry { after_secs: 2 }
        );
        assert_eq!(
            p.decide(FailureClass::RateLimited, 3, None),
            RetryDecision::Retry { after_secs: 4 }
        );
        // 2^9 = 512, capped at 60.
        assert_eq!(
            p.decide(FailureClass::RateLimited, 10, None),
            RetryDecision::Retry { after_secs: 60 }
        );
    }

    #[test]
    fn exhausting_attempts_gives_up() {
        let p = RetryPolicy::default(); // max_attempts = 5
        assert_eq!(
            p.decide(FailureClass::RateLimited, 5, None),
            RetryDecision::GiveUp
        );
    }

    #[test]
    fn server_retry_after_hint_wins_when_larger() {
        let p = RetryPolicy::default();
        // backoff for attempt 1 = 1s, but server said wait 30s.
        assert_eq!(
            p.decide(FailureClass::RateLimited, 1, Some(30)),
            RetryDecision::Retry { after_secs: 30 }
        );
        // server hint smaller than backoff → use backoff.
        assert_eq!(
            p.decide(FailureClass::RateLimited, 4, Some(2)),
            RetryDecision::Retry { after_secs: 8 }
        );
    }
}
