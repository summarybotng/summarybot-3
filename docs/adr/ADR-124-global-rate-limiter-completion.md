# ADR-124: Global rate-limiter coordinator (completes LEG-001)

> Rewrite-era ADR. Numbering continues from the reference set (119–123 are ours).

- **Status**: Accepted (2026-06-06)
- **Deciders**: Martin Cleaver
- **Source**: V1 maintenance handoff — `docs/V3-IMPLEMENTATION-GUIDE.md`,
  `docs/sparc/pseudocode/03-rate-limiter.md` (both in the reference repo);
  LEG-001.
- **Builds on**: ADR-123 (LLM resilience pure core). **Related**: ADR-024
  (Phase 3 retry engine), §12.0 (host owns LLM I/O).

## Context

The V1 maintenance session handed over a guide and SPARC phase-1–3 artifacts
(Specification, Pseudocode, Architecture) and asked V3 to do phases 4–5
(Refinement/TDD, Completion), with **LEG-001 as the critical week-2 item**: V1
created a `ClaudeClient` per job, each throttling independently, so concurrent
jobs competed for quota blind to one another and tripped 429s.

Two mismatches had to be reconciled, since the guide predates knowledge of our
stack:

1. **Stack.** The guide assumes a **Python** rebuild (`uv`, FastAPI,
   `src/adapters/llm/rate_limiter.py`, async). Our V3 is the **Rust/WASM** build
   (PRD §12). We implement the guide's *intent and algorithm*, not its language.
2. **Phase 1–3 already covered the design.** ADR-123 had already shipped the
   pure core (TokenBucket, CircuitBreaker, taxonomy, provider parsing). This ADR
   is the **refinement + completion**: align the core to the SPARC pseudocode and
   build the process-wide coordinator.

## Decision

### 1. Refine the pure core to the SPARC design (TDD)

- **`RequestPriority`** → three levels `Low < Normal < Manual` (was two), matching
  `MANUAL > NORMAL > LOW`.
- **`CircuitBreaker`** gains a half-open **success threshold** (SPARC default 2):
  one lucky probe no longer declares recovery. Default stays 1 (non-breaking);
  the coordinator sets 2.
- **`TokenBucket`** gains `try_acquire_reserving` (priority headroom),
  `available`/`capacity` (status), `drain` (adaptive slowdown), `time_until_ms`.

### 2. `GlobalRateLimiter` — one process-wide, thread-safe coordinator

`crates/host/src/llm/limiter.rs`. A single instance shared via `Arc` (the actual
fix): per-provider buckets + a shared circuit breaker behind one `Mutex`.

- **`try_acquire(provider, priority, now)` → `AcquireDecision`** (`Granted` /
  `RetryAfterMs` / `CircuitOpenMs`). Checks the circuit first, then the bucket
  with a priority reserve.
- **Priority as reserve headroom.** Rather than an async wait-queue, lower
  priorities are refused once the bucket drops below a reserve (`Normal` holds
  back 10%, `Low` 20%, `Manual` 0). This makes scheduled work yield to a user
  waiting on a summary — deterministically and synchronously (LEG-001 #4).
- **Adaptive slowdown** (LEG-001 #2): a recorded 429 trips the shared circuit
  *and* drains that provider's bucket, so the process backs off as a whole.
- **`status()`** snapshot for dashboards/telemetry (LEG-001 #5).
- A cross-thread test asserts the core property: 8 threads share **one** budget
  (total grants bounded by capacity, not capacity-per-thread).

### 3. Synchronous now; async wrapper deferred

The coordinator is sync (matches the current codebase; no runtime yet). The
blocking `async acquire(...).await` that sleeps out a `RetryAfterMs`, and the
**OpenRouter HTTP adapter** that calls `record_success`/`record_rate_limit` from
parsed response headers (LEG-003 `parse_rate_limit`), land with the Phase 3
pipeline (ADR-024) — they are thin wrappers over this core.

## Reconciling the guide's house rules

The guide lists CI rules from the (Python) V1 plan that differ from this repo's:

- **"No file > 300 lines / no function > 30 lines."** This repo's governing rule
  (CLAUDE.md) is **≤ 500 lines**. We keep aiming small (most modules are well
  under 300; `ratelimit.rs` is 326 after refinement). Treat 300 as a target, 500
  as the hard limit, pending a decision to tighten CLAUDE.md.
- **"Domain has zero external dependencies."** The domain crate now depends on
  `chrono`/`chrono-tz` (WhatsApp timestamp normalization, WHA-020). These are
  **pure, I/O-free, wasm-compatible** computation libraries; the rule's intent —
  no infrastructure/I/O coupling in the domain — is satisfied. Documented as a
  deliberate, bounded exception (the alternative, moving parsing to the host,
  conflicts with WHA-011's "parse in the WASM guest").
- **80% coverage** — consistent with our intent; every module here is unit-tested.

These are flagged for the maintainer; CLAUDE.md remains authoritative for this
repo unless changed.

## Consequences

- **Positive**: LEG-001's load-bearing behaviour (shared budget, circuit, priority,
  adaptive backoff, telemetry) is real, thread-safe and exhaustively tested
  without a network. The V1 failure mode is structurally prevented.
- **Negative / trade-offs**: priority is reserve-based, not a true FIFO-within-
  priority queue — adequate for yield-to-manual; a real queue can land with the
  async wrapper if needed. End-to-end behaviour awaits the Phase 3 OpenRouter
  adapter.
