# Coverage map — legacy summarybot-ng vs. the rewrite

At-a-glance: how much of the original Python **summarybot-ng** the Rust/WASM
rewrite covers, and what's left. **Keep this current** — see
[conventions/keep-coverage-map-current](conventions/keep-coverage-map-current.md).

- **As of:** 2026-06-13 — two-layer delivery plugins (per-tenant enable + connect + config; Google Drive OAuth connect flow); membership-derived workspace grants (token filtered to entitlement); Discord/Slack channel send-back sinks; Docker + compose + Fly deploy config; WhatsApp coverage complete (WHA-014..019: timeline + classified gaps, contributor tracking, persisted auto-fulfilled scoped import invitations); Tenants/Members admin tab (RBAC); rolling-ingest dedup Layers 1–3; rolling wired end-to-end (ADR-101)
- **Legend:** ✅ done & tested · 🟡 partial · 🔩 seam only (trait/config/policy, no live impl) · ⛔ not started · ➖ out of scope / dropped
- **Legacy** column = does the old product have it. **Rewrite** = our status.

## Scorecard (where the work is)

| Area | Rewrite vs legacy | Headline gap |
|---|---|---|
| Core summarization | ✅ **at/above parity** | map-reduce, citations, cost cap, budgets — done |
| WhatsApp ingestion | ✅ **at parity** | full pipeline, anonymized, deduped |
| Scheduling & rolling digests | ✅ **at parity** | recurrence + per-tenant LLM/budget + live-source fetch + **rolling-period accumulation wired end-to-end** (ADR-101), Append + Hybrid/Resummarize merges; rolling-ingest dedup Layer 4 is a refinement |
| Multi-tenancy & roles | ✅ **at/above parity** | tenants, members, invites, routing |
| Dashboard / web UI | ✅ **at parity** | 11 tabs + live SSE; incl. Knowledge, Spend, Audit, Tenants/Members admin |
| Delivery | ✅ **~95%** | plugin seam ✅, webhook/Confluence/email/Google Drive ✅; Discord/Slack channel **send** ✅; DM send + per-destination templates remain |
| Discord / Slack ingestion | ✅ **both live + scheduled** | Discord + Slack REST fetch → message store (generic `connections/:platform` API); a schedule can fetch fresh messages before each run (ADR-128). Channel **send-back** now shipped (Delivery sinks); DM send + an independent background poller remain |
| Auth / OAuth | 🟡 **mostly there** | sessions/roles ✅; real OAuth login ✅ (Google/Discord); workspace grants now membership-derived (token filtered to entitlement) ✅ |
| Knowledge (wiki / vector search) | ✅ **v1 done** | semantic search + coherence gate + wiki synthesis live (ADR-127); AI curator deferred |
| External integrations | ⛔ **mostly absent** | Confluence, Google Drive, voice transcription |
| Ops (Docker, migrations, metrics) | 🟡 **deployable** | container + compose + Fly config ✅; migration ledger ✅; `/metrics` gauges ✅; request-rate counters + structured logs remain |

**Rough read:** the *summarize → schedule → deliver-to-dashboard/webhook* spine is
done and multi-tenant, and the knowledge subsystem (units + semantic search +
coherence + wiki, ADR-127) now ships. The remaining work clusters in three
buckets: (1) **live platform I/O** (Discord/Slack **send**, email/Confluence
delivery, real OAuth — fetch is done), (2) **production hardening** (Docker/deploy;
migrations + basic metrics + audit done), and (3) **refinements** (rolling now
wired end-to-end with Append + Hybrid/Resummarize merges — remaining:
rolling-ingest dedup ADR-129 Layer 4, per-destination rolling delivery).

---

## Ingestion / message sources

| Feature | Legacy | Rewrite | Notes |
|---|---|---|---|
| WhatsApp import (zip / `_chat.txt`) | ✅ (REST ingest) | ✅ | `host/whatsapp.rs`, `api/whatsapp.rs`; iOS/Android parse, TZ + date-order inference (WHA-001/020) |
| PII anonymization at ingest | ✅ | ✅ | HMAC phone→pseudonym before storage (WHA-006) |
| Dedup (file + message level) | ✅ | ✅ | SHA-256 + synthetic fingerprint (WHA-010/012) |
| WhatsApp coverage + history gaps | ✅ | ✅ | per-chat import spans merged → classified `before_join`/`between_imports`/`after_last` gaps; group-creation anchor detected from chat events (WHA-014/015/016); `host/coverage.rs`, `GET /workspaces/:ws/whatsapp/chats[/:chat/coverage]`. Each import now recorded in `whatsapp_imports` (was never populated before) |
| Coverage timeline + "ask members to export X" | ➖ | ✅ | `Whatsapp.tsx` covered-vs-gaps bar, coverage %, copy-ready scoped export instructions per fillable gap (WHA-017) |
| Contributor tracking | ✅ | ✅ | per-chat rollup of who supplied which date range / how many imports + messages, from the attributed import records; `host::contributors_for`, surfaced on the coverage card (WHA-018) |
| Scoped import invitations (persisted, auto-fulfilled) | ✅ | ✅ | `whatsapp_import_invitations` table; open a tracked ask against a gap, auto-marked `fulfilled` (credited to the contributor) when a covering import lands, or `cancelled`; `host/coverage.rs` reconcile wired into ingest; `POST .../chats/:chat/invitations[/:id/cancel]`; Request/Cancel in `Whatsapp.tsx` (WHA-019) |
| Discord message fetch | ✅ (discord.py) | ✅ | `DiscordFetcher` over Discord REST v10 (blocking `ureq`, `--features discord`); pure normalization unit-tested; bot token encrypted; fetch persists to the message store (ADR-128). Verified live (real 401 on a bogus token). Gateway/streaming + polling scheduler deferred |
| Slack message fetch | ✅ (OAuth + history) | ✅ | `SlackFetcher` over the Slack Web API (`conversations.list`/`.history`, blocking `ureq`, `--features slack`); pure normalization unit-tested; bot token encrypted; fetch persists to the message store (ADR-128). Verified live (real `invalid_auth` on a bogus token). Bot must be a channel member; name resolution + Slack OAuth install flow deferred |
| Google Drive sync (as a source) | ✅ | ➖ | not carried as an ingestion source; v3 uses Drive only as a publish sink (see Delivery, ADR-126) |
| Voice-note transcription (Whisper) | ✅ (optional) | ⛔ | — |
| Message normalization + triviality filter | ✅ | ✅ | `domain/message.rs` `is_substantial()` (MSG-008) |

## Summarization

| Feature | Legacy | Rewrite | Notes |
|---|---|---|---|
| LLM providers | Claude direct + OpenRouter | ✅ | demo / OpenRouter / any OpenAI-compatible (local Ollama) — ADR-125 |
| Model ladder + failure fallback | ✅ | ✅ | `domain/summarize/model.rs` (ADR-024) |
| Long-history chunking (map-reduce) | ✅ (smart chunking) | ✅ | `host/summarize.rs` recursive reduce (ADR-095) |
| Structured output (key points, action items, terms, participants) | ✅ | ✅ | `domain/summarize/extract.rs` (ADR-004) |
| Grounded citations | ✅ | ✅ | message-index → id resolution; carried through reduce |
| Summary lengths (brief/detailed/comprehensive) | ✅ | ✅ | — |
| Per-perspective prompts (dev/marketing/exec…) | ✅ | ⛔ | single prompt strategy; perspectives not ported |
| Custom prompts per workspace | ✅ | 🟡 | free-text per-workspace summary instructions appended to the prompt (SUM-007, Settings UI); named per-perspective presets not ported |
| Retry / resilience / rate limiting | ✅ | ✅ | `ResilientLlm` + token-bucket limiter (ADR-123/124) |
| Summary caching (memory/Redis) | ✅ | ➖ | not ported; not currently needed |

## Cost controls

| Feature | Legacy | Rewrite | Notes |
|---|---|---|---|
| Per-request hard cost cap | ✅ | ✅ | `CostGuard`, degrade-not-overspend (ADR-095) |
| Per-tenant budget (rolling window) | partial | ✅ | `domain/budget.rs` (ADR-125 Phase 3) |
| BYO LLM key, encrypted at rest | n/a | ✅ | AES-256-GCM, `LLM_CONFIG_KEY` (ADR-125 Phase 2b) |
| Operator-lent key + budget | n/a | ✅ | owner-set grants |
| Cost analytics / spend dashboards | partial | ✅ | `GET /workspaces/:ws/spend` aggregates per-model spend from stored summaries (total / recent window / by-model); `Spend.tsx` dashboard. Per-tenant rollups are a refinement |

## Scheduling / digests

| Feature | Legacy | Rewrite | Notes |
|---|---|---|---|
| Recurrence (once/hourly/daily/weekly/monthly/custom) | ✅ | ✅ | `domain/schedule.rs` |
| Persistent background scheduler | ✅ (APScheduler) | ✅ | `api/scheduler_driver.rs`; grace + auto-disable |
| Per-tenant LLM + budget on scheduled runs | ✅ | ✅ | `TenantAwareRunner` (cfc9fd9) |
| Live source fetch on scheduled runs | ✅ | ✅ | a schedule can bind a Discord/Slack source and fetch fresh messages before summarizing (`schedule_sources`, best-effort; ADR-128) |
| Rolling-period summaries | ✅ | ✅ | **wired end-to-end** (ADR-101): `decide_rolling` policy + `rolling_schedules`/`rolling_summaries` storage (one-active-per-schedule via PK) + runner accumulate/finalize (folds the tail, publishes one digest, clears the accumulator) + schedule API/web config. Merge strategies implemented: **Append** (dated sections verbatim) and **Hybrid/Resummarize** (a synthesis LLM pass at finalize folds the accumulated sections into one coherent digest with merged structured fields) |
| Lookback windows | ✅ | ✅ | per-schedule `lookback_secs` |
| Manage via API (create/list/pause/trigger/history) | ✅ | ✅ | `api/schedules.rs` |
| Manage via Discord `/schedule` commands | ✅ | ⛔ | no in-chat command surface |

## Delivery / output

| Feature | Legacy | Rewrite | Notes |
|---|---|---|---|
| Dashboard store (always-on) | ✅ | ✅ | `repository/summary_store.rs` |
| Sink **plugin seam** (open kind + encrypted JSON config + schema-driven API/UI) | n/a | ✅ | ADR-126; `host/delivery.rs` registry + descriptors |
| Two-layer plugin model: per-tenant enable + connect + config | n/a | ✅ | tenant admin enables a plugin and configures/connects its account credentials once (`tenant_plugins` table, `Plugins` admin tab, `/tenants/:t/plugins`); workspaces pick only the non-secret target. `FieldScope::{Tenant,Workspace}` splits each descriptor; delivery merges tenant creds under the workspace target; an explicitly-disabled plugin is refused (ADR-126). Google Drive uses a **Connect** OAuth flow (`drive.file`) to capture the refresh token instead of pasting it |
| Webhook (generic + Slack/Discord incoming) | ✅ | ✅ | reference **sink plugin** (`--features http-llm`); encrypted config, test-send (verified live) |
| Confluence publishing | ✅ | ✅ | **sink plugin** (`--features confluence`); Cloud REST, API token; schema-driven UI verified live (real publish not yet tested against a live instance) |
| Email (SMTP) | ✅ | ✅ | **sink plugin** (`--features email`); lettre blocking SMTP, STARTTLS/TLS; schema-driven UI verified live (real send not yet tested against a live SMTP server) |
| Discord / Slack channel send | ✅ | ✅ | **sink plugins** (`--features discord`/`slack`): post a summary back to a channel via the platform REST API, reusing the workspace's stored bot token (injected at delivery time, `host::inject_platform_token`) — the same credential used to fetch (ADR-128). Schema-driven UI (channel id) + test-send verified live (clear "no bot token" / transport result without a real bot). DM send + per-destination templates deferred |
| Google Drive (publish summaries) | ✅ | ✅ | **sink plugin** (`--features gdrive`); publishes HTML as a Google Doc via Drive multipart upload. Refresh token now captured per **tenant** via a **Connect** OAuth flow (`drive.file`, signed-state callback) instead of pasting; workspaces set only the folder. Real upload not yet tested against live Drive |
| Output formats (markdown/html/json/text) | ✅ | ✅ | markdown + plain + html ✅ (email HTML, Confluence HTML); webhook payload carries a structured **`data`** JSON object |
| Push templates per destination | ✅ | ⛔ | — |

## Web UI / dashboard

| Feature | Legacy | Rewrite | Notes |
|---|---|---|---|
| Summaries (search/filter/pin/archive/tag) | ✅ | ✅ | `web/views/Summaries.tsx` |
| Schedules | ✅ | ✅ | `Schedules.tsx` |
| WhatsApp import | ✅ | ✅ | `Whatsapp.tsx` + summarize-now |
| Discord / Slack ingestion | ✅ | ✅ | one generic `Source.tsx` behind Discord + Slack tabs (bot token + sync + per-channel summarize; `--features discord`/`slack`) |
| Delivery destinations | ✅ | ✅ | `Delivery.tsx` (webhook add/test/remove) |
| Settings (LLM config + budget) | ✅ | ✅ | `Settings.tsx` |
| Tenant members admin (roles + invites) | ✅ | ✅ | `Members.tsx` — list/role/remove members, issue/revoke invites over the tenancy API (RBAC, TEN-005) |
| Cost / spend dashboard | partial | ✅ | `Spend.tsx` — total + recent-window + per-model breakdown |
| Audit log | ✅ | ✅ | `Audit.tsx` — admin/security events, newest first (Admin+) |
| Live updates | ✅ | ✅ | SSE (`summary.created/deleted`) |
| Per-tenant branding | ✅ | 🟡 | accent + name; no logo/full theme |
| Extra legacy pages (Jobs, Archive) | ✅ | ⛔ | not built (Wiki, Slack, Audit, Tenants/Members admin now have tabs) |

## Auth / identity / multi-tenancy

| Feature | Legacy | Rewrite | Notes |
|---|---|---|---|
| Sessions / JWT / refresh / logout | ✅ | ✅ | HS256, revocation (`host/auth`) |
| OAuth redirect (Google/Discord) | ✅ | ✅ | authorization-code + PKCE + signed state (`--features oauth`); needs provider keys; live flow not yet run end-to-end |
| OAuth (Slack) | ✅ | ⛔ | not added |
| Email magic-link | ✅ | 🔩 | dev provider stub (no real link delivery) |
| Workspace grants from membership | ✅ | ✅ | the access token is filtered to entitlement at issue time (`AuthService::entitle`): a requested workspace is granted only if the user owns it or is a member of its tenant; unclaimed workspaces (no row) stay open for dev/first-run, denials are audit-logged. Enforced on login + refresh |
| Tenants: provision / update / route by domain | partial | ✅ | `api/tenancy.rs`, `host/tenant_routing.rs` |
| Members + roles (Owner/Admin/Member/Guest) | ✅ (RBAC) | ✅ | `domain/membership.rs` |
| Invitations | partial | ✅ | `host/invite.rs` |
| Tenant isolation at storage | ✅ | ✅ | every query workspace/tenant-scoped (TEN-007) |
| Shared platform sources across tenants | ✅ | ⛔ | ADR-120, future |

## Knowledge subsystem

| Feature | Legacy | Rewrite | Notes |
|---|---|---|---|
| Knowledge units extracted from summaries (KNO-001) | ✅ | ✅ | headline + key points + action items, with provenance (ADR-127) |
| Vector store / semantic search (KNO-002/003/005) | ✅ | ✅ | local embeddings (Mac-mini `nomic-embed-text`) + SQLite brute-force cosine behind a swap-in seam; verified live; HNSW/RuVector deferred |
| Embeddings from a local model (KNO-007) | ✅ | ✅ | OpenAI-compatible `/v1/embeddings`; demo embedder offline; model pinned per unit |
| Coherence / hallucination gate (COH-001) | ✅ | ✅ | lexical grounding check; grounded score persisted + shown on summaries (LLM-judge is a stronger follow-up) |
| Wiki synthesis (pages, regenerate) (WIK-001/002/003) | ✅ (extensive) | ✅ | LLM organizes a workspace's units into one topic-grouped `knowledge-base` page; regenerable on demand; per-tenant LLM + budget (ADR-125); verified live + browser-checked. Multi-page emergent structure is a refinement |
| AI wiki curator (CUR-*) | ✅ | ⛔ | deferred (ADR-127) |
| Rolling-ingest dedup (SUM-010) | ✅ | 🟡 | **Layers 1–3 shipped (ADR-129)**: content-addressed unit ids (exact repeats collapse on upsert), a **semantic near-dup gate** (cosine ≥ threshold, default 0.93), and **delta-only ingest** — the rolling runner feeds each period's delta into the knowledge base as it accumulates (`with_knowledge`), so finalize needn't re-ingest and dedup handles overlap. All unit-tested. Remaining: replace-set/provenance-merge (Layer 4) |

## Storage / ops

| Feature | Legacy | Rewrite | Notes |
|---|---|---|---|
| SQLite persistence + repository pattern | ✅ | ✅ | `repository/` traits + `SqliteRepository` |
| Migration framework | ✅ (58+ tracked) | ✅ | `schema_migrations` ledger + ordered runner; idempotent baseline, future changes append as `(id, sql)` |
| Audit log | ✅ | ✅ | `audit_log` ledger surfaced: `GET /workspaces/:ws/audit` (Admin+, tenant-member-scoped) + `Audit.tsx` tab; member role/remove + invite issuance now write entries (WSP-014). Per-tenant column + more instrumented events are a refinement |
| Docker / Fly / Render deploy configs | ✅ | ✅ | multi-stage `Dockerfile` (web SPA → release API binary with the production feature set → slim non-root runtime, rustls so no OpenSSL, healthcheck), `.dockerignore`, `docker-compose.yml` (named volume for the SQLite DB) and `fly.toml`. SECRET_KEY required at runtime (never baked); DB on a `/data` volume |
| Monitoring / metrics | ✅ | ✅ | `GET /metrics` Prometheus DB gauges + **HTTP request counters** (total, by status class, cumulative duration) from the correlation-id middleware; one **structured JSON access log** per request carrying the correlation id (infra probes excluded) |
| WASM sandbox boundary | n/a | 🟡 | architecture proven; only WhatsApp parse runs in WASM |

---

## Remaining work to reach legacy parity (rough priority)

1. **Live platform I/O** — Discord + Slack fetch are live, a schedule can fetch fresh messages before each run, and channel **send-back** now ships as delivery sinks (ADR-128). Remaining: **DM** send and a background poller that syncs on its own cadence (independent of summary schedules).
2. **Delivery completion** — email (SMTP) deliverer; Confluence publishing; output formats beyond markdown.
3. **Real OAuth** — Discord/Google redirect flows exist (`--features oauth`); remaining: run them end-to-end + Slack. Login workspace grants are now entitlement-filtered (membership-derived) ✅.
4. **Rolling-period summaries (ADR-101)** — ✅ wired end-to-end (storage, runner accumulate/finalize, API/web config, Append merge). Remaining refinements: rolling-ingest dedup Layer 4 (ADR-129) and per-destination rolling delivery control (ADR-108); Append + Hybrid/Resummarize merges done.
5. **Production hardening** — ✅ container + compose + Fly config (migration runner ✅, basic `/metrics` ✅, audit-log surfacing ✅). Remaining: request-rate counters + structured logs.
6. **Knowledge subsystem (Phase 7)** — ✅ done: units + semantic search + coherence gate + wiki synthesis (ADR-127); AI curator + rolling-ingest dedup (below) deferred.
7. **Nice-to-haves** — per-perspective & custom prompts, push templates, summary caching, extra dashboard pages, Google Drive, voice transcription.

Items intentionally **not** carried over unless a need appears: summary caching, some legacy dashboard pages, guild-era constructs (the rewrite is workspace-native by design).
