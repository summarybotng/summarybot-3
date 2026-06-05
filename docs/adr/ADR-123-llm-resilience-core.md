# ADR-123: LLM resilience core (from V1 maintenance LEG-001/002/003)

> Rewrite-era ADR. Numbering continues from the reference set (ended at ADR-118;
> rewrite has added 119–122).

- **Status**: Accepted (2026-06-05)
- **Deciders**: Martin Cleaver
- **Source**: `docs/v1-legacy-requirements.md` (V1 maintenance repo) — LEG-001
  (global rate-limit coordination), LEG-002 (failure classification), LEG-003
  (OpenRouter-specific handling)
- **Related**: ADR-024 (resilient multi-model retry — Phase 3), ADR-095
  (fixed-point cost math), §12.0 (host owns all I/O incl. LLM), §12.4 Phase 3

## Context

V1 maintenance surfaced a class of production failures (e.g. job `job_8db618fb`)
caused by **per-job LLM clients competing for quota blind to each other**: each
`ClaudeClient` had independent throttling, so concurrent jobs hammered the
provider into 429s and surfaced them as generic "failed". The LEG requirements
ask the rewrite for: process-wide rate coordination + circuit breaker (LEG-001),
a real failure taxonomy with retry semantics (LEG-002), and a provider
abstraction that understands provider-specific rate-limit signals (LEG-003).

These are **LLM-orchestration** concerns. Per §12.0 the native host owns all LLM
I/O, so they belong host-side, not in the WASM compute guest.

## Decision

Implement the **pure core now**, in `crates/host/src/llm/`, with the clock and
all I/O injected so every rule is deterministic and unit-tested — the same
pure-core / host-seam split used for identity, sessions and JWT in Phase 1:

1. **`ratelimit`** (LEG-001) — a `TokenBucket` (continuous refill, fractional
   accrual) and a `CircuitBreaker` FSM (Closed/Open/HalfOpen) that opens after N
   consecutive rate limits and pauses the whole process for a cooldown, honoring
   a server cooldown hint. `RequestPriority` (Manual > Scheduled) is the shared
   vocabulary for LEG-001 #4.
2. **`failure`** (LEG-002) — a `FailureClass` taxonomy (`rate_limited` /
   `quota_exceeded` / `invalid_request` / `service_unavailable` / `unknown`) with
   `is_retryable`, an `as_str` for the API `failure_reason` field, HTTP-status
   classification, and a `RetryPolicy` with exponential backoff that respects a
   server `Retry-After` hint.
3. **`provider`** (LEG-003) — an `LlmProvider` enum that parses each backend's
   rate-limit headers (OpenRouter `x-ratelimit-*`, Anthropic
   `anthropic-ratelimit-*`) into a normalized `RateLimitSnapshot`.

### Deferred to the Phase 3 LLM adapter (host I/O seam)

These need a runtime/network and land with ADR-024's retry engine, not now:

- the **async coordinator** that actually awaits token availability and threads
  real time into the bucket/breaker;
- the **priority queue** that makes scheduled jobs yield to manual ones
  (LEG-001 #4) and the **auto-retry queue** for transient failures (LEG-002 #4);
- **credit-balance** and **model-availability** pre-checks (LEG-003 #3/#4);
- **rate-limit telemetry** sink + dashboard surfacing (LEG-001 #5).

The pure core is shaped so these are thin wrappers over it (e.g. the async
`acquire()` loops on `try_acquire` + `time_until_available_ms`).

## Consequences

- **Positive**: the load-bearing, error-prone *decisions* (when to back off, trip,
  retry, give up; how to read a provider's budget) are isolated and exhaustively
  tested without a live API; the Phase 3 adapter becomes mechanical wiring.
- **Negative / trade-offs**: the behaviour isn't end-to-end exercised until the
  async coordinator exists in Phase 3 — accepted, since the seam is the same one
  already proven for auth.
- **Traceability**: code comments and the implementing commit reference the
  `LEG-XXX` ids, per the V1 legacy-requirements convention.
