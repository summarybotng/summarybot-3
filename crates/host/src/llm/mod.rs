//! LLM orchestration core — resilience primitives carried over from V1
//! maintenance (LEG-001/002/003).
//!
//! The host owns all LLM I/O (§12.0), so this lives host-side. Everything here
//! is **pure and clock-injected** so it is deterministic and unit-testable; the
//! async coordinator that actually waits, makes HTTP calls, checks credit
//! balance and model availability (LEG-003 #3/#4), runs the priority queue
//! (LEG-001 #4) and emits telemetry (LEG-001 #5) is layered on top with the
//! summarization pipeline in Phase 3 (ADR-024 resilient multi-model retry).
//!
//! - [`ratelimit`] — process-wide token bucket + circuit breaker (LEG-001).
//! - [`failure`] — failure taxonomy + retry policy (LEG-002).
//! - [`provider`] — provider abstraction + rate-limit header parsing (LEG-003).

pub mod failure;
pub mod provider;
pub mod ratelimit;

pub use failure::{classify_http_status, FailureClass, RetryDecision, RetryPolicy};
pub use provider::{LlmProvider, RateLimitSnapshot};
pub use ratelimit::{CircuitBreaker, CircuitState, RequestPriority, TokenBucket};
