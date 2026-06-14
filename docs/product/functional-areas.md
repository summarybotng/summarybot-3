# Functional areas

SummaryBot is organized into a small number of subsystems with clean seams. This
maps the product's functional areas to what they own, their key boundaries, and
the ADRs that define them.

## Architecture at a glance

The codebase is a Rust workspace with a deliberate dependency direction:

```
domain        pure policy + types (no I/O): summaries, citations, rolling state,
              roles/permissions, delivery gating, coverage gaps — all unit-tested
   ▲
repository    storage boundary (SQLite); every read/write is workspace/tenant-scoped
   ▲
host          orchestration + all I/O: LLM calls, fetchers, crypto, delivery,
              knowledge, scheduler — wires domain policy to repositories
   ▲
api           thin axum HTTP layer (OpenAPI-shaped) + the dashboard SPA it serves
              ┌ wasm-summarize: the summarize step runs in a sandboxed component
```

Pure decisions live in `domain` and are exhaustively tested; everything that
touches the network, disk, a key, or the clock lives in `host`; `api` handlers
stay thin. No file exceeds ~500 lines by convention.

## The end-to-end flow

```
sources ──▶ ingest/normalize ──▶ message store ──▶ summarize (LLM, grounded)
                                                        │
                          ┌─────────────────────────────┼───────────────┐
                          ▼                             ▼               ▼
                    dashboard store              knowledge units   delivery sinks
                    (always-on)                  (search + wiki)   (channel/email/…)
```

Scheduling drives this loop on a cadence; rolling digests accumulate it over a
period.

---

## 1. Ingestion & sources

Normalizes conversation from every supported platform into one `NormalizedMessage`
model, so the rest of the system is platform-agnostic.

- **WhatsApp (import-only)** — zip/text upload → parse (iOS/Android, timezone +
  date-order inference) → anonymize phone numbers (HMAC) → fingerprint-dedup →
  store. Import spans are recorded for coverage analysis. *(ADR-121)*
- **Discord & Slack (live fetch)** — `PlatformFetcher` adapters pull recent
  history over REST; bot tokens are stored encrypted per workspace. A
  **server + channel browser** (`list_servers` → `/connections/:platform/servers`
  for the bot's Discord guilds, then `channel_directory()` →
  `/connections/:platform/channels` for that guild's channels grouped by category)
  lets the dashboard offer point-and-click selection instead of pasted ids.
  *(ADR-128; WSP-006)*
- **Coverage & contribution** — merges multi-contributor import spans into a
  coverage timeline with classified, fillable gaps; tracks who contributed what;
  persists auto-fulfilling import invitations. *(ADR-121; WHA-014..019)*

Boundary: ingestion only normalizes and stores; it never summarizes.

## 2. Summarization

Turns a bounded set of messages into a structured, grounded summary.

- **Resilient pipeline** — model ladder + failure-classified retry +
  process-wide rate limiter, under a hard per-request cost cap that degrades to a
  flagged partial. *(ADR-123/124, ADR-095)*
- **Structured extraction + citations** — key points, action items, technical
  terms, participants, and `[N]` citations resolved to real messages. *(ADR-004)*
- **Map-reduce** — long histories are chunked and reduced within the context
  window. *(ADR-095)*
- **Coherence gate** — grounding score persisted with the summary. *(COH-001)*
- **Sandbox** — the pure summarize step runs in a `wasm32-wasip2` component.

Boundary: summarization is pure compute over messages it's handed; the host owns
fetching the messages and persisting the result.

## 3. Scheduling & rolling digests

Runs summarization on a cadence and accumulates rolling periods.

- **Scheduler** — persistent schedules (once/hourly/daily/weekly/monthly/custom),
  a background tick loop with grace and auto-disable, manual trigger, run history.
- **Rolling state machine** — `decide_rolling` (StartNew / Accumulate / Finalize)
  with the one-active-period-per-schedule invariant; catch-up across missed days;
  idempotent finalize. Merge strategies: Append and Hybrid/Resummarize. *(ADR-101/130)*
- **Live fetch on run** — optionally pull fresh source messages before each run.
- **Retrospective (past dates)** — summarize a chat's *history* by week
  (`POST .../whatsapp/chats/:chat/summarize-weeks`), one digest per non-empty week
  over the imported range — the "Past dates" mode of the v2 create-summary wizard
  (ADR-088/089), applied per week (ADR-101) and skipping empty weeks (ADR-048).

Boundary: the runner composes ingestion + summarization + delivery; it owns no
new policy beyond the rolling state machine (which lives in `domain`).

## 4. Delivery

Fans a produced summary out to its destinations.

- **Always-on dashboard store** — the core, unconditional destination.
- **Sink plugins** — webhook, Confluence, email, Google Drive, Discord/Slack
  channel send-back. Each is an open `kind` with a schema-driven, encrypted
  config and a `Deliverer`. *(ADR-126)*
- **Two-layer model** — tenant-level enablement + account credentials/connect;
  workspace-level target. Effective config = tenant credentials merged under the
  workspace target. *(ADR-126)*
- **Gating** — the pure `resolve_delivery` policy decides allow/reject from
  workspace capabilities before any send. *(DEL-010/011)*

Boundary: delivery is the only subsystem that talks to external sink services;
the gating decision is pure and testable.

## 5. Knowledge

Builds durable, searchable memory from summaries.

- **Knowledge units** — extracted inline during summarization with provenance.
- **Vector search** — local embeddings + cosine ranking behind a swap-in seam.
- **Dedup (4 layers)** — content-id collapse, semantic near-dup gate, delta-only
  rolling ingest, and provenance-merge. *(ADR-129)*
- **Wiki synthesis** — an emergent topic page regenerable on demand. *(ADR-127)*
- **Curator** — advisory health report (duplicate clusters + stale units),
  read-only and audit-logged. *(CUR-*)*

Boundary: knowledge ingestion is best-effort and never fails the summary that
produced it.

## 6. Multi-tenancy, identity & access

The security and organization spine.

- **Tenancy** — tenants contain workspaces; host→tenant routing by subdomain /
  custom domain; per-tenant LLM config and budgets. *(ADR-066, ADR-125)*
- **Identity & sessions** — pluggable providers, JWT access + revocable refresh
  tokens, real OAuth (Google/Discord). Workspace grants are derived from
  membership entitlement. *(WSP-001/010)*
- **RBAC** — Owner/Admin/Member/Guest + a platform-operator role; permission
  checks at the API boundary, isolation enforced at the repository. *(ADR-119; TEN-005/007)*
- **Audit** — append-only ledger of security/admin events, surfaced to admins.

Boundary: tenant isolation is enforced in the repository layer — every query is
scoped, so it can't be forgotten in a handler.

## 7. Cost controls

- Per-request hard cap (`CostGuard`), per-tenant rolling-window budgets, BYO LLM
  keys encrypted at rest, and a spend-analytics dashboard. *(ADR-125)*

## 8. Operations & observability

- Single binary serving the API + SPA; SQLite with a tracked migration ledger;
  Prometheus `/metrics` (DB gauges + HTTP counters); structured JSON access logs
  with correlation ids; Docker/compose/Fly deploy config.

## 9. Web dashboard

The React/TypeScript SPA the binary serves: a left-nav of the areas above, with
live updates over SSE. See the [user guide](user-guide.md) for the tab-by-tab
walkthrough.

---

For who operates each area and the jobs they do, see
[Roles & user stories](roles-and-user-stories.md).
