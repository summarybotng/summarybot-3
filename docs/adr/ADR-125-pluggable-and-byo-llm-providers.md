# ADR-125: Pluggable, per-tenant LLM providers (BYO-LLM + operator-lent key)

> Rewrite-era ADR. Numbering continues from the reference set (119–124 are ours).

- **Status**: Accepted (2026-06-07)
- **Deciders**: Martin Cleaver
- **Builds on**: ADR-123 (LLM resilience pure core), ADR-124 (global rate
  limiter), §12.0 (host owns LLM I/O), SUM-016 (per-workspace cost cap).
- **Supersedes (in part)**: the working assumption that the platform always
  calls OpenRouter with a single platform key.

## Context

Earlier work standardized on **OpenRouter with one platform-held key** as the
only network LLM backend (`OpenRouterClient`, feature `openrouter`). Two
realities make that too rigid:

1. **Cost during development.** A locally-hosted, OpenAI-compatible endpoint
   (Ollama / LM Studio on a Mac mini on the operator's LAN) is effectively free
   to run and fast enough for dev/demo. Forcing OpenRouter for every call burns
   credits to exercise plumbing.
2. **Who pays, in production.** A multi-tenant platform shouldn't silently spend
   the operator's LLM budget for every tenant. Tenants should be able to **bring
   their own LLM** (their key, or their own endpoint), and where the operator
   *does* lend the platform key, it must be **bounded by a budget** per tenant.

The LLM call path is already abstracted behind the `LlmClient` trait, and
OpenRouter's API is just the OpenAI chat-completions shape — the same shape
Ollama and LM Studio expose. So "OpenRouter" was never special; it's one
*OpenAI-compatible HTTP endpoint* among many.

## Decision

**1. The network LLM client is provider-neutral: an OpenAI-compatible HTTP
client** parameterized by `base_url` + an *optional* bearer key. OpenRouter,
OpenAI, and a local Ollama/LM Studio endpoint are all just different
`(base_url, key?)` pairs. The cargo feature is renamed `openrouter → http-llm`
to reflect this; `HttpLlmClient::openrouter(key)` remains a convenience ctor.

**2. The model is configuration, not a constant.** The summarization model name
(and, later, its price for cost accounting) comes from config rather than the
hard-coded `"demo"`. Dev points the model at whatever the Mac mini serves
(e.g. `llama3.1`); OpenRouter points it at a hosted model id.

**3. LLM provider configuration is resolved per tenant, with a defined
precedence** (the "who pays" model):

   a. **Tenant BYO** — the tenant configured its own endpoint/key → use it; the
      tenant owns the cost, no platform budget applies.
   b. **Operator-lent platform key with a budget** — the tenant has no own
      provider but the operator granted them use of the platform key up to a
      **per-tenant budget** (a cost ceiling per period). Calls draw down the
      budget (reusing the `cost_micros` the pipeline already records); once
      exhausted, summarization is refused with a clear "budget exceeded" error
      rather than spending more.
   c. **Process default** — the env-configured backend (dev: the Mac mini;
      otherwise demo). Used when neither (a) nor (b) is set, primarily for
      single-tenant/dev.

   Cost-cap-per-request (SUM-016) still applies *within* whichever path is
   chosen; the budget (b) is the *cumulative* ceiling on the lent key.

## Phased implementation

- **Phase 1 (this increment).** Provider-neutral `HttpLlmClient` (base_url +
  optional key); process-wide model + backend selected from env
  (`LLM_BASE_URL`, `LLM_API_KEY`, `LLM_MODEL`, falling back to
  `OPENROUTER_API_KEY`, then demo). This makes the Mac mini usable now and is
  the (c) "process default" rung.
- **Phase 2a (this increment).** Per-tenant **keyless** LLM config
  (`tenant_llm_config`: base_url + model) + resolution at request time, managed
  via the tenancy API + a dashboard settings screen. Covers "bring your own
  *endpoint*" (a tenant's self-hosted Ollama/vLLM/LM Studio — the common case
  given the local-LLM direction). No secret-at-rest, so it ships without the
  encryption decision below.
- **Phase 2b (pending Open Q1).** Add an encrypted BYO **key** column so a
  tenant can point at a hosted provider with their own key. Blocked on the
  key-encryption-at-rest decision (Open Q1) — deliberately not shipped with
  plaintext keys.
- **Phase 3.** Operator-lent key + budget: a per-tenant budget (micros/period),
  spend accounting from `cost_micros`, and refusal when exhausted → the (b)
  rung. Operator-granted out-of-band (consistent with PRM-008 / ADR-119, where
  cross-tenant authority is operator-only).

## Consequences

- **Good.** Dev is free/fast (local model); tenants control their own spend;
  the operator's budget is never silently drained; one client covers every
  OpenAI-compatible provider; the abstraction (`LlmClient` + model ladder) is
  unchanged, so the resilience/retry/cost machinery is reused as-is.
- **Cost.** Secret storage for tenant keys (Phase 2) must be encrypted at rest,
  not just `Secret<T>`-wrapped in memory — flagged as an open item. A local
  endpoint has no rate-limit headers, so LEG-001/003 parsing is a no-op there
  (harmless). Output quality now varies by configured model — pin per tenant.
- **Security.** Tenant keys are bearer secrets: stored only as ciphertext,
  never logged, never returned by the API (write-only, like invite tokens).

## Open questions (for Phase 2/3)

1. ~~**Key encryption at rest**~~ — **RESOLVED 2026-06-07: operator master key +
   AES-256-GCM per record** (random 96-bit nonce, `base64(nonce‖ciphertext)`).
   Master key is 32 bytes from `LLM_CONFIG_KEY` (64 hex). Implemented in Phase 2b
   (`host::secretbox`). A deployment may still source that key from a KMS/secret
   store externally.
2. **Budget period & reset** — calendar month vs. rolling window; per-tenant vs.
   per-workspace. Leaning: per-tenant, calendar month, operator-configurable.
3. **Provider health/fallback** — if a tenant's BYO endpoint is down, fail their
   request (don't silently spend the platform key). Confirm.
