//! Process-wide LLM rate-limit coordinator (LEG-001; SPARC `03-rate-limiter`).
//!
//! The fix for the V1 failure mode: V1 created a `ClaudeClient` per job, each
//! throttling independently, so concurrent jobs competed for quota blind to one
//! another and tripped 429s. [`GlobalRateLimiter`] is **one** instance shared
//! across the process (hold it in an `Arc`), funneling every LLM request through
//! a shared circuit breaker and per-provider [`TokenBucket`]s.
//!
//! It is synchronous and thread-safe (a single `Mutex`), so it composes with the
//! current codebase. The blocking `async acquire(...).await` wrapper that waits
//! out [`AcquireDecision::RetryAfterMs`], and the OpenRouter HTTP adapter that
//! calls `record_*`, land with the Phase 3 pipeline (ADR-024). Priority queuing
//! is realized here as **reserve headroom** (lower priorities are refused once
//! the bucket is low), which makes scheduled work yield to manual deterministically
//! without an async queue.

use super::{CircuitBreaker, CircuitState, LlmProvider, RequestPriority, TokenBucket};
use std::collections::HashMap;
use std::sync::Mutex;

/// Tunables (env-driven in production: `OPENROUTER_RPM`,
/// `CIRCUIT_FAILURE_THRESHOLD`, `CIRCUIT_OPEN_TIMEOUT`).
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    pub default_rpm: u32,
    pub circuit_failure_threshold: u32,
    pub circuit_success_threshold: u32,
    pub circuit_open_ms: i64,
    /// Fraction of capacity held back from `Normal` requests (reserved for
    /// `Manual`). `Low` reserves twice this.
    pub normal_reserve_fraction: f64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            default_rpm: 50,
            circuit_failure_threshold: 5,
            circuit_success_threshold: 2,
            circuit_open_ms: 60_000,
            normal_reserve_fraction: 0.10,
        }
    }
}

/// The coordinator's verdict for one request attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquireDecision {
    /// Proceed with the request.
    Granted,
    /// Bucket exhausted (or reserved for higher priority); retry after this many ms.
    RetryAfterMs(i64),
    /// Circuit is open; retry after this many ms.
    CircuitOpenMs(i64),
}

/// Read-only snapshot for monitoring/telemetry (LEG-001 #5).
#[derive(Debug, Clone, PartialEq)]
pub struct RateLimitStatus {
    pub circuit_state: CircuitState,
    pub circuit_retry_after_ms: i64,
    /// (provider, available tokens, capacity).
    pub buckets: Vec<(&'static str, f64, f64)>,
}

struct Inner {
    buckets: HashMap<&'static str, TokenBucket>,
    circuit: CircuitBreaker,
    config: RateLimitConfig,
}

/// Process-wide singleton (share via `Arc<GlobalRateLimiter>`).
pub struct GlobalRateLimiter {
    inner: Mutex<Inner>,
}

impl GlobalRateLimiter {
    pub fn new(config: RateLimitConfig) -> Self {
        let circuit = CircuitBreaker::new(config.circuit_failure_threshold, config.circuit_open_ms)
            .with_success_threshold(config.circuit_success_threshold);
        Self {
            inner: Mutex::new(Inner {
                buckets: HashMap::new(),
                circuit,
                config,
            }),
        }
    }

    /// Seed a provider with an explicit per-minute limit (else `default_rpm` is
    /// used lazily on first use).
    pub fn with_provider(self, provider: LlmProvider, rpm: u32, now_ms: i64) -> Self {
        self.inner
            .lock()
            .unwrap()
            .buckets
            .insert(provider.as_str(), TokenBucket::per_minute(rpm, now_ms));
        self
    }

    /// Attempt to acquire permission for one request. Checks the circuit first,
    /// then the provider bucket with priority-appropriate reserve. Non-blocking:
    /// a refusal carries the ms to wait, which the async wrapper sleeps out.
    pub fn try_acquire(
        &self,
        provider: LlmProvider,
        priority: RequestPriority,
        now_ms: i64,
    ) -> AcquireDecision {
        let mut inner = self.inner.lock().unwrap();
        if !inner.circuit.allows(now_ms) {
            return AcquireDecision::CircuitOpenMs(inner.circuit.retry_after_ms(now_ms));
        }
        let default_rpm = inner.config.default_rpm;
        let reserve = reserve_for(priority, inner.config.normal_reserve_fraction);
        let bucket = inner
            .buckets
            .entry(provider.as_str())
            .or_insert_with(|| TokenBucket::per_minute(default_rpm, now_ms));
        let reserve_tokens = reserve * bucket.capacity();
        if bucket.try_acquire_reserving(reserve_tokens, now_ms) {
            AcquireDecision::Granted
        } else {
            AcquireDecision::RetryAfterMs(bucket.time_until_ms(1.0 + reserve_tokens, now_ms))
        }
    }

    /// Record a successful call — feeds circuit recovery.
    pub fn record_success(&self) {
        self.inner.lock().unwrap().circuit.on_success();
    }

    /// Record a rate-limit (429) from `provider`. Trips the shared circuit and
    /// drains that provider's bucket as a proactive slowdown (LEG-001 #2).
    pub fn record_rate_limit(
        &self,
        provider: LlmProvider,
        retry_after_ms: Option<i64>,
        now_ms: i64,
    ) {
        let mut inner = self.inner.lock().unwrap();
        inner.circuit.on_rate_limit(now_ms, retry_after_ms);
        if let Some(bucket) = inner.buckets.get_mut(provider.as_str()) {
            bucket.drain(now_ms);
        }
    }

    /// Snapshot for dashboards/telemetry (LEG-001 #5).
    pub fn status(&self, now_ms: i64) -> RateLimitStatus {
        let inner = self.inner.lock().unwrap();
        let mut buckets: Vec<(&'static str, f64, f64)> = inner
            .buckets
            .iter()
            .map(|(name, b)| (*name, b.available(now_ms), b.capacity()))
            .collect();
        buckets.sort_by_key(|(name, _, _)| *name);
        RateLimitStatus {
            circuit_state: inner.circuit.state(),
            circuit_retry_after_ms: inner.circuit.retry_after_ms(now_ms),
            buckets,
        }
    }
}

/// Reserve fraction by priority: manual takes the last token; normal holds back
/// `f`; low holds back `2f`.
fn reserve_for(priority: RequestPriority, f: f64) -> f64 {
    match priority {
        RequestPriority::Manual => 0.0,
        RequestPriority::Normal => f,
        RequestPriority::Low => f * 2.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    fn limiter(rpm: u32) -> GlobalRateLimiter {
        GlobalRateLimiter::new(RateLimitConfig {
            default_rpm: rpm,
            ..RateLimitConfig::default()
        })
    }

    #[test]
    fn grants_until_exhausted_then_reports_wait() {
        let rl = limiter(60); // 60/min = 1/sec
        for _ in 0..60 {
            assert_eq!(
                rl.try_acquire(LlmProvider::OpenRouter, RequestPriority::Manual, 0),
                AcquireDecision::Granted
            );
        }
        match rl.try_acquire(LlmProvider::OpenRouter, RequestPriority::Manual, 0) {
            AcquireDecision::RetryAfterMs(ms) => assert_eq!(ms, 1_000),
            other => panic!("expected RetryAfterMs, got {other:?}"),
        }
    }

    #[test]
    fn low_priority_yields_headroom_to_manual() {
        // 100 rpm, normal reserve 10% → low reserves 20 tokens. Drain to ~15
        // tokens: low is refused (needs >20), manual still granted.
        let rl = limiter(100);
        for _ in 0..85 {
            assert_eq!(
                rl.try_acquire(LlmProvider::OpenRouter, RequestPriority::Manual, 0),
                AcquireDecision::Granted
            );
        }
        // ~15 tokens left. Low (reserve 20) refused; Manual (reserve 0) granted.
        assert!(matches!(
            rl.try_acquire(LlmProvider::OpenRouter, RequestPriority::Low, 0),
            AcquireDecision::RetryAfterMs(_)
        ));
        assert_eq!(
            rl.try_acquire(LlmProvider::OpenRouter, RequestPriority::Manual, 0),
            AcquireDecision::Granted
        );
    }

    #[test]
    fn open_circuit_refuses_all_until_cooldown() {
        let rl = GlobalRateLimiter::new(RateLimitConfig {
            default_rpm: 100,
            circuit_failure_threshold: 1,
            circuit_open_ms: 30_000,
            ..RateLimitConfig::default()
        });
        rl.record_rate_limit(LlmProvider::OpenRouter, None, 0); // trips
        match rl.try_acquire(LlmProvider::OpenRouter, RequestPriority::Manual, 1_000) {
            AcquireDecision::CircuitOpenMs(ms) => assert_eq!(ms, 29_000),
            other => panic!("expected CircuitOpenMs, got {other:?}"),
        }
        // After cooldown, a probe is admitted.
        assert_eq!(
            rl.try_acquire(LlmProvider::OpenRouter, RequestPriority::Manual, 30_000),
            AcquireDecision::Granted
        );
    }

    #[test]
    fn rate_limit_drains_the_offending_bucket() {
        let rl = limiter(100);
        rl.try_acquire(LlmProvider::OpenRouter, RequestPriority::Manual, 0);
        rl.record_rate_limit(LlmProvider::OpenRouter, None, 0);
        // Bucket drained → next manual request must wait.
        assert!(matches!(
            rl.try_acquire(LlmProvider::OpenRouter, RequestPriority::Manual, 0),
            AcquireDecision::RetryAfterMs(_)
        ));
    }

    #[test]
    fn status_reports_circuit_and_buckets() {
        let rl = limiter(50);
        rl.try_acquire(LlmProvider::OpenRouter, RequestPriority::Manual, 0);
        let s = rl.status(0);
        assert_eq!(s.circuit_state, CircuitState::Closed);
        assert_eq!(s.buckets.len(), 1);
        let (name, available, capacity) = s.buckets[0];
        assert_eq!(name, "openrouter");
        assert_eq!(capacity, 50.0);
        assert_eq!(available, 49.0); // one taken
    }

    #[test]
    fn shared_across_threads_coordinates_one_budget() {
        // The core LEG-001 property: one shared limiter, many concurrent callers,
        // total grants bounded by capacity — not capacity-per-thread.
        let rl = Arc::new(limiter(40));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let rl = Arc::clone(&rl);
            handles.push(thread::spawn(move || {
                let mut granted = 0;
                for _ in 0..20 {
                    if rl.try_acquire(LlmProvider::OpenRouter, RequestPriority::Normal, 0)
                        == AcquireDecision::Granted
                    {
                        granted += 1;
                    }
                }
                granted
            }));
        }
        let total: u32 = handles.into_iter().map(|h| h.join().unwrap()).sum();
        // 40 capacity, normal reserve 10% (=4) → at most 36 granted at t=0,
        // shared across all threads (never 8×something).
        assert!(total <= 36, "granted {total} exceeded shared budget");
        assert!(total >= 30, "granted {total} unexpectedly low");
    }
}
