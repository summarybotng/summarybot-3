//! Resilient LLM call path (ADR-024) — the Phase 3 wiring that ties the
//! LEG-001/002/003 cores into one orchestrated request.
//!
//! [`ResilientLlm`] wraps any [`LlmClient`]: it acquires from the process-wide
//! [`GlobalRateLimiter`] (LEG-001), calls the client, and on failure classifies
//! the error (LEG-002), records a 429 back to the limiter, and retries per the
//! [`RetryPolicy`] with backoff — or gives up on a permanent error.
//!
//! Synchronous, with the clock and sleeper **injected**, so the whole retry/
//! backoff/circuit dance is deterministic under test with no real time or
//! network. It flips to `async` with the Phase 5 server runtime; the concrete
//! network client (OpenRouter over HTTP) is the remaining last mile and slots in
//! as another [`LlmClient`].

use super::{
    AcquireDecision, FailureClass, GlobalRateLimiter, LlmProvider, RequestPriority, RetryDecision,
    RetryPolicy,
};
use domain::summarize::FinishReason;
use std::sync::Arc;

/// A bounded LLM request (simplified; the real shape carries messages/params).
#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub provider: LlmProvider,
    pub priority: RequestPriority,
    pub model: String,
    pub prompt: String,
}

/// Actual token usage reported by the provider (OpenAI/OpenRouter `usage`
/// block). When present, the pipeline records these *real* counts for spend +
/// the metadata panel (ADR-106/134) instead of character-based estimates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenUsage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    /// The provider's *real* charge for this call in micro-dollars, when it
    /// reports one (OpenRouter's `usage.cost`, USD → µ$). `None` → the caller
    /// falls back to its configured per-token price estimate.
    pub cost_micros: Option<i64>,
}

/// A successful completion. `finish_reason` is the structural signal the
/// summarizer's quality gate uses (Q#6) to detect truncation. `usage` carries
/// the provider's real token counts when it reports them (None for the demo
/// client / providers that omit it → the caller falls back to estimates).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmResponse {
    pub model: String,
    pub text: String,
    pub finish_reason: FinishReason,
    pub usage: Option<TokenUsage>,
}

/// A classified LLM failure (LEG-002). `retry_after_secs` is the server hint, if
/// any. Carries enough for the engine to decide retry vs give-up and to surface
/// `failure_reason` upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmError {
    pub class: FailureClass,
    pub retry_after_secs: Option<i64>,
    pub detail: String,
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "llm {}: {}", self.class.as_str(), self.detail)
    }
}

impl std::error::Error for LlmError {}

/// A backend that performs one LLM completion (no retry/limiting — that is the
/// engine's job). Concrete impls: the network OpenRouter client; a fake for
/// tests.
pub trait LlmClient {
    fn complete(&self, request: &LlmRequest) -> Result<LlmResponse, LlmError>;
}

/// Blanket impl so a shared `Arc<dyn LlmClient + Send + Sync>` (held in
/// `AppState`) is itself an [`LlmClient`] — one process-wide client can back
/// both the on-demand endpoint and the scheduler without cloning the backend.
impl<T: LlmClient + ?Sized> LlmClient for Arc<T> {
    fn complete(&self, request: &LlmRequest) -> Result<LlmResponse, LlmError> {
        (**self).complete(request)
    }
}

/// Monotonic-ish millisecond clock (injected for determinism).
pub trait Clock {
    fn now_ms(&self) -> i64;
}

/// Blocking wait (injected so tests don't actually sleep).
pub trait Sleeper {
    fn sleep_ms(&self, ms: i64);
}

/// Wall-clock from the system time.
pub struct SystemClock;
impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }
}

/// Real thread sleep.
pub struct ThreadSleeper;
impl Sleeper for ThreadSleeper {
    fn sleep_ms(&self, ms: i64) {
        if ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(ms as u64));
        }
    }
}

/// Wraps a client with rate limiting + classified retry (ADR-024).
pub struct ResilientLlm<C: LlmClient> {
    client: C,
    limiter: Arc<GlobalRateLimiter>,
    policy: RetryPolicy,
    // Send + Sync so a ResilientLlm can live in a tokio task (the scheduler driver).
    clock: Box<dyn Clock + Send + Sync>,
    sleeper: Box<dyn Sleeper + Send + Sync>,
    /// Max time to wait for a rate-limit/circuit slot before failing the attempt.
    acquire_timeout_ms: i64,
}

impl<C: LlmClient> ResilientLlm<C> {
    pub fn new(client: C, limiter: Arc<GlobalRateLimiter>) -> Self {
        Self {
            client,
            limiter,
            policy: RetryPolicy::default(),
            clock: Box::new(SystemClock),
            sleeper: Box::new(ThreadSleeper),
            acquire_timeout_ms: 30_000,
        }
    }

    pub fn with_policy(mut self, policy: RetryPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Override clock + sleeper (tests inject deterministic ones).
    pub fn with_time(
        mut self,
        clock: Box<dyn Clock + Send + Sync>,
        sleeper: Box<dyn Sleeper + Send + Sync>,
    ) -> Self {
        self.clock = clock;
        self.sleeper = sleeper;
        self
    }

    /// Run a request through the resilient path: acquire → call → on failure,
    /// record + retry per policy, else return the classified error.
    pub fn complete(&self, request: &LlmRequest) -> Result<LlmResponse, LlmError> {
        let mut attempts: u32 = 0;
        loop {
            self.acquire(request.provider, request.priority)?;
            match self.client.complete(request) {
                Ok(response) => {
                    self.limiter.record_success();
                    return Ok(response);
                }
                Err(err) => {
                    attempts += 1;
                    if err.class == FailureClass::RateLimited {
                        self.limiter.record_rate_limit(
                            request.provider,
                            err.retry_after_secs.map(|s| s * 1000),
                            self.clock.now_ms(),
                        );
                    }
                    match self
                        .policy
                        .decide(err.class, attempts, err.retry_after_secs)
                    {
                        RetryDecision::Retry { after_secs } => {
                            self.sleeper.sleep_ms(after_secs * 1000)
                        }
                        RetryDecision::GiveUp => return Err(err),
                    }
                }
            }
        }
    }

    /// Block (via the injected sleeper) until the limiter grants a slot, or fail
    /// the attempt once the acquire timeout is exceeded.
    fn acquire(&self, provider: LlmProvider, priority: RequestPriority) -> Result<(), LlmError> {
        let deadline = self.clock.now_ms() + self.acquire_timeout_ms;
        loop {
            match self
                .limiter
                .try_acquire(provider, priority, self.clock.now_ms())
            {
                AcquireDecision::Granted => return Ok(()),
                AcquireDecision::RetryAfterMs(ms) | AcquireDecision::CircuitOpenMs(ms) => {
                    let now = self.clock.now_ms();
                    if now >= deadline {
                        return Err(LlmError {
                            class: FailureClass::ServiceUnavailable,
                            retry_after_secs: Some((ms / 1000).max(1)),
                            detail: "rate-limit acquire timed out".to_string(),
                        });
                    }
                    self.sleeper.sleep_ms(ms.min(deadline - now).max(1));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::RateLimitConfig;
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicI64, Ordering};

    /// Clock + sleeper sharing one counter: sleeping advances time, so bucket
    /// refills and backoff are exercised deterministically with no real waiting.
    struct FakeTime {
        now: Arc<AtomicI64>,
    }
    impl Clock for FakeTime {
        fn now_ms(&self) -> i64 {
            self.now.load(Ordering::SeqCst)
        }
    }
    impl Sleeper for FakeTime {
        fn sleep_ms(&self, ms: i64) {
            self.now.fetch_add(ms.max(0), Ordering::SeqCst);
        }
    }
    fn fake_time(now: &Arc<AtomicI64>) -> Box<FakeTime> {
        Box::new(FakeTime { now: now.clone() })
    }

    /// A client that returns a scripted sequence of outcomes, one per call.
    struct ScriptedClient {
        script: RefCell<Vec<Result<LlmResponse, LlmError>>>,
        calls: RefCell<u32>,
    }
    impl ScriptedClient {
        fn new(script: Vec<Result<LlmResponse, LlmError>>) -> Self {
            Self {
                script: RefCell::new(script),
                calls: RefCell::new(0),
            }
        }
    }
    impl LlmClient for ScriptedClient {
        fn complete(&self, _req: &LlmRequest) -> Result<LlmResponse, LlmError> {
            *self.calls.borrow_mut() += 1;
            if self.script.borrow().is_empty() {
                panic!("client called more times than scripted");
            }
            self.script.borrow_mut().remove(0)
        }
    }

    fn ok(text: &str) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            model: "m".into(),
            text: text.into(),
            finish_reason: FinishReason::Stop,
            usage: None,
        })
    }
    fn err(class: FailureClass, retry_after: Option<i64>) -> Result<LlmResponse, LlmError> {
        Err(LlmError {
            class,
            retry_after_secs: retry_after,
            detail: "scripted".into(),
        })
    }

    fn req() -> LlmRequest {
        LlmRequest {
            provider: LlmProvider::OpenRouter,
            priority: RequestPriority::Manual,
            model: "m".into(),
            prompt: "hi".into(),
        }
    }

    fn engine(client: ScriptedClient, now: &Arc<AtomicI64>) -> ResilientLlm<ScriptedClient> {
        let limiter = Arc::new(GlobalRateLimiter::new(RateLimitConfig::default()));
        ResilientLlm::new(client, limiter).with_time(fake_time(now), fake_time(now))
    }

    #[test]
    fn success_on_first_try() {
        let now = Arc::new(AtomicI64::new(0));
        let e = engine(ScriptedClient::new(vec![ok("done")]), &now);
        assert_eq!(e.complete(&req()).unwrap().text, "done");
    }

    #[test]
    fn retries_a_rate_limit_then_succeeds() {
        let now = Arc::new(AtomicI64::new(0));
        let client = ScriptedClient::new(vec![err(FailureClass::RateLimited, Some(2)), ok("ok")]);
        let e = engine(client, &now);
        assert_eq!(e.complete(&req()).unwrap().text, "ok");
        // It waited (clock advanced past the server's 2s hint).
        assert!(now.load(Ordering::SeqCst) >= 2_000);
    }

    #[test]
    fn permanent_error_is_not_retried() {
        let now = Arc::new(AtomicI64::new(0));
        // Only one outcome scripted; a retry would panic ("more than scripted").
        let e = engine(
            ScriptedClient::new(vec![err(FailureClass::InvalidRequest, None)]),
            &now,
        );
        let out = e.complete(&req()).unwrap_err();
        assert_eq!(out.class, FailureClass::InvalidRequest);
    }

    #[test]
    fn gives_up_after_max_attempts() {
        let now = Arc::new(AtomicI64::new(0));
        // 5 service-unavailable; default policy max_attempts = 5 → gives up.
        let script = vec![err(FailureClass::ServiceUnavailable, None); 5];
        let e = engine(ScriptedClient::new(script), &now);
        assert_eq!(
            e.complete(&req()).unwrap_err().class,
            FailureClass::ServiceUnavailable
        );
    }

    #[test]
    fn waits_for_rate_limiter_when_bucket_empty() {
        // Drain the shared limiter first, then a request must wait (clock
        // advances) before the bucket refills and the call goes through.
        let now = Arc::new(AtomicI64::new(0));
        let limiter = Arc::new(GlobalRateLimiter::new(RateLimitConfig {
            default_rpm: 60, // 1/sec
            ..RateLimitConfig::default()
        }));
        for _ in 0..60 {
            limiter.try_acquire(LlmProvider::OpenRouter, RequestPriority::Manual, 0);
        }
        let e = ResilientLlm::new(ScriptedClient::new(vec![ok("late")]), limiter)
            .with_time(fake_time(&now), fake_time(&now));
        assert_eq!(e.complete(&req()).unwrap().text, "late");
        assert!(now.load(Ordering::SeqCst) >= 1_000); // waited ~1s for a token
    }
}
