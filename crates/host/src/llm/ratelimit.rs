//! Global rate-limit coordination primitives (LEG-001).
//!
//! Pure accounting with the clock **injected** (`now_ms`), so the logic is
//! deterministic and unit-testable; the host wraps these in a process-wide,
//! `async` coordinator that actually waits and threads real time. One shared
//! [`TokenBucket`] + [`CircuitBreaker`] per process replaces V1's per-job
//! clients that competed for quota blind to each other.

/// Relative request priority (LEG-001 #4): manual/on-demand requests outrank
/// scheduled ones, so scheduled work yields under pressure. The actual priority
/// queue lives in the host coordinator; this is the shared vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RequestPriority {
    Scheduled,
    Manual,
}

/// A classic token bucket. `capacity` tokens, refilled continuously at
/// `refill_per_ms`; each request costs one token. Fractional tokens accrue
/// between calls so the long-run rate is exact.
#[derive(Debug, Clone)]
pub struct TokenBucket {
    capacity: f64,
    tokens: f64,
    refill_per_ms: f64,
    last_refill_ms: i64,
}

impl TokenBucket {
    /// A bucket sized to `rpm` requests per minute, starting full at `now_ms`.
    pub fn per_minute(rpm: u32, now_ms: i64) -> Self {
        let capacity = f64::from(rpm.max(1));
        Self {
            capacity,
            tokens: capacity,
            refill_per_ms: capacity / 60_000.0,
            last_refill_ms: now_ms,
        }
    }

    fn refill(&mut self, now_ms: i64) {
        if now_ms > self.last_refill_ms {
            let elapsed = (now_ms - self.last_refill_ms) as f64;
            self.tokens = (self.tokens + elapsed * self.refill_per_ms).min(self.capacity);
            self.last_refill_ms = now_ms;
        }
    }

    /// Try to spend one token at `now_ms`. Returns `true` if granted.
    pub fn try_acquire(&mut self, now_ms: i64) -> bool {
        self.refill(now_ms);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Milliseconds until the next token is available (0 if one is available
    /// now). The host uses this to schedule the wait instead of busy-polling.
    pub fn time_until_available_ms(&self, now_ms: i64) -> i64 {
        let mut probe = self.clone();
        probe.refill(now_ms);
        if probe.tokens >= 1.0 {
            0
        } else {
            ((1.0 - probe.tokens) / probe.refill_per_ms).ceil() as i64
        }
    }
}

/// Circuit-breaker state (LEG-001 #3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Normal operation.
    Closed,
    /// Tripped: requests are refused until the cooldown elapses.
    Open,
    /// Cooldown elapsed: a single probe is allowed to test recovery.
    HalfOpen,
}

/// Opens after a run of consecutive rate limits and pauses **all** requests for
/// a cooldown, so the process backs off as a whole instead of each job retrying
/// into the same wall (LEG-001 #3).
#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    failure_threshold: u32,
    cooldown_ms: i64,
    consecutive_failures: u32,
    open_until_ms: i64,
    state: CircuitState,
}

impl CircuitBreaker {
    /// Open after `failure_threshold` consecutive rate limits; stay open for
    /// `cooldown_ms`.
    pub fn new(failure_threshold: u32, cooldown_ms: i64) -> Self {
        Self {
            failure_threshold: failure_threshold.max(1),
            cooldown_ms,
            consecutive_failures: 0,
            open_until_ms: 0,
            state: CircuitState::Closed,
        }
    }

    pub fn state(&self) -> CircuitState {
        self.state
    }

    /// Whether a request may proceed at `now_ms`. An open circuit transitions to
    /// half-open (and allows a single probe) once its cooldown has elapsed.
    pub fn allows(&mut self, now_ms: i64) -> bool {
        match self.state {
            CircuitState::Closed | CircuitState::HalfOpen => true,
            CircuitState::Open => {
                if now_ms >= self.open_until_ms {
                    self.state = CircuitState::HalfOpen;
                    true
                } else {
                    false
                }
            }
        }
    }

    /// A request succeeded: reset failures and close the circuit.
    pub fn on_success(&mut self) {
        self.consecutive_failures = 0;
        self.state = CircuitState::Closed;
    }

    /// A request was rate-limited. Trips the circuit when the threshold is hit,
    /// or immediately if a half-open probe failed. Honors a server-supplied
    /// cooldown hint when it is longer than the configured one.
    pub fn on_rate_limit(&mut self, now_ms: i64, retry_after_ms: Option<i64>) {
        self.consecutive_failures += 1;
        let trip = self.state == CircuitState::HalfOpen
            || self.consecutive_failures >= self.failure_threshold;
        if trip {
            let cooldown = retry_after_ms.map_or(self.cooldown_ms, |h| h.max(self.cooldown_ms));
            self.open_until_ms = now_ms.saturating_add(cooldown);
            self.state = CircuitState::Open;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_grants_up_to_capacity_then_refuses() {
        let mut b = TokenBucket::per_minute(60, 0); // 60/min = 1/sec, cap 60
                                                    // 60 immediate grants from a full bucket.
        for _ in 0..60 {
            assert!(b.try_acquire(0));
        }
        assert!(!b.try_acquire(0));
    }

    #[test]
    fn bucket_refills_over_time() {
        let mut b = TokenBucket::per_minute(60, 0); // 1 token/sec
        for _ in 0..60 {
            assert!(b.try_acquire(0));
        }
        assert!(!b.try_acquire(0));
        // One second later, ~1 token has refilled.
        assert!(b.try_acquire(1_000));
        assert!(!b.try_acquire(1_000));
    }

    #[test]
    fn time_until_available_is_zero_when_tokens_present() {
        let b = TokenBucket::per_minute(60, 0);
        assert_eq!(b.time_until_available_ms(0), 0);
    }

    #[test]
    fn time_until_available_reports_wait_when_empty() {
        let mut b = TokenBucket::per_minute(60, 0); // 1 token/sec
        for _ in 0..60 {
            b.try_acquire(0);
        }
        // Empty → ~1000ms until the next whole token.
        assert_eq!(b.time_until_available_ms(0), 1_000);
    }

    #[test]
    fn priority_orders_manual_above_scheduled() {
        assert!(RequestPriority::Manual > RequestPriority::Scheduled);
    }

    #[test]
    fn circuit_opens_after_threshold_consecutive_rate_limits() {
        let mut cb = CircuitBreaker::new(5, 10_000);
        for _ in 0..4 {
            cb.on_rate_limit(0, None);
            assert_eq!(cb.state(), CircuitState::Closed);
            assert!(cb.allows(0));
        }
        cb.on_rate_limit(0, None); // 5th
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(!cb.allows(0));
    }

    #[test]
    fn success_resets_the_failure_run() {
        let mut cb = CircuitBreaker::new(5, 10_000);
        for _ in 0..4 {
            cb.on_rate_limit(0, None);
        }
        cb.on_success();
        // Run reset: four more failures must not trip it.
        for _ in 0..4 {
            cb.on_rate_limit(0, None);
        }
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn open_circuit_half_opens_after_cooldown_then_reopens_on_failed_probe() {
        let mut cb = CircuitBreaker::new(1, 10_000);
        cb.on_rate_limit(0, None); // trips immediately (threshold 1)
        assert!(!cb.allows(5_000)); // still within cooldown
        assert!(cb.allows(10_000)); // cooldown elapsed → half-open probe allowed
        assert_eq!(cb.state(), CircuitState::HalfOpen);
        // Probe fails → straight back to open.
        cb.on_rate_limit(10_000, None);
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(!cb.allows(10_001));
    }

    #[test]
    fn server_cooldown_hint_extends_open_window() {
        let mut cb = CircuitBreaker::new(1, 1_000);
        cb.on_rate_limit(0, Some(30_000)); // server says wait 30s, longer than 1s
        assert!(!cb.allows(20_000));
        assert!(cb.allows(30_000));
    }
}
