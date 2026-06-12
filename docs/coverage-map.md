# Coverage map — legacy summarybot-ng vs. the rewrite

At-a-glance: how much of the original Python **summarybot-ng** the Rust/WASM
rewrite covers, and what's left. **Keep this current** — see
[conventions/keep-coverage-map-current](conventions/keep-coverage-map-current.md).

- **As of:** 2026-06-12 — audit-log surfacing (WSP-014); corrected rolling-period status to 🔩 pure-policy-only (ADR-101 not wired)
- **Legend:** ✅ done & tested · 🟡 partial · 🔩 seam only (trait/config/policy, no live impl) · ⛔ not started · ➖ out of scope / dropped
- **Legacy** column = does the old product have it. **Rewrite** = our status.

## Scorecard (where the work is)

| Area | Rewrite vs legacy | Headline gap |
|---|---|---|
| Core summarization | ✅ **at/above parity** | map-reduce, citations, cost cap, budgets — done |
| WhatsApp ingestion | ✅ **at parity** | full pipeline, anonymized, deduped |
| Scheduling & rolling digests | 🟡 **scheduling at parity** | recurrence + per-tenant LLM/budget + live-source fetch done; **rolling-period accumulation (ADR-101) is pure-policy-only, not wired** |
| Multi-tenancy & roles | ✅ **at/above parity** | tenants, members, invites, routing |
| Dashboard / web UI | ✅ **core parity** | 5 tabs + live SSE; missing legacy's extra pages |
| Delivery | 🟡 **~80%** | plugin seam ✅, webhook/Confluence/email/Google Drive ✅; platform channel/DM send 🔩 |
| Discord / Slack ingestion | ✅ **both live + scheduled** | Discord + Slack REST fetch → message store (generic `connections/:platform` API); a schedule can fetch fresh messages before each run (ADR-128). Channel/DM **send** + an independent background poller remain |
| Auth / OAuth | 🟡 **mostly there** | sessions/roles ✅; real OAuth login ✅ (Google/Discord); workspace grants not yet membership-derived |
| Knowledge (wiki / vector search) | ✅ **v1 done** | semantic search + coherence gate + wiki synthesis live (ADR-127); AI curator deferred |
| External integrations | ⛔ **mostly absent** | Confluence, Google Drive, voice transcription |
| Ops (Docker, migrations, metrics) | 🟡 **thin** | single binary; ad-hoc schema; minimal telemetry |

**Rough read:** the *summarize → schedule → deliver-to-dashboard/webhook* spine is
done and multi-tenant, and the knowledge subsystem (units + semantic search +
coherence + wiki, ADR-127) now ships. The remaining work clusters in three
buckets: (1) **live platform I/O** (Discord/Slack **send**, email/Confluence
delivery, real OAuth — fetch is done), (2) **rolling-period summaries** (ADR-101
pure policy exists, end-to-end accumulation/finalization not wired), and (3)
**production hardening** (Docker/deploy; migrations + basic metrics + audit done).

---

## Ingestion / message sources

| Feature | Legacy | Rewrite | Notes |
|---|---|---|---|
| WhatsApp import (zip / `_chat.txt`) | ✅ (REST ingest) | ✅ | `host/whatsapp.rs`, `api/whatsapp.rs`; iOS/Android parse, TZ + date-order inference (WHA-001/020) |
| PII anonymization at ingest | ✅ | ✅ | HMAC phone→pseudonym before storage (WHA-006) |
| Dedup (file + message level) | ✅ | ✅ | SHA-256 + synthetic fingerprint (WHA-010/012) |
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
| Rolling-period summaries | ✅ | 🔩 | **pure policy only** (`domain/rolling.rs`, ADR-101): `RollingPeriod` window + `decide_rolling` state machine (StartNew/Accumulate/Finalize) + strategies, unit-tested. **Not wired**: no rolling-summary storage / one-active-per-schedule invariant, the runner doesn't accumulate or finalize, no merge execution, no UI. See ROL-001..006 in remaining work |
| Lookback windows | ✅ | ✅ | per-schedule `lookback_secs` |
| Manage via API (create/list/pause/trigger/history) | ✅ | ✅ | `api/schedules.rs` |
| Manage via Discord `/schedule` commands | ✅ | ⛔ | no in-chat command surface |

## Delivery / output

| Feature | Legacy | Rewrite | Notes |
|---|---|---|---|
| Dashboard store (always-on) | ✅ | ✅ | `repository/summary_store.rs` |
| Sink **plugin seam** (open kind + encrypted JSON config + schema-driven API/UI) | n/a | ✅ | ADR-126; `host/delivery.rs` registry + descriptors |
| Webhook (generic + Slack/Discord incoming) | ✅ | ✅ | reference **sink plugin** (`--features http-llm`); encrypted config, test-send (verified live) |
| Confluence publishing | ✅ | ✅ | **sink plugin** (`--features confluence`); Cloud REST, API token; schema-driven UI verified live (real publish not yet tested against a live instance) |
| Email (SMTP) | ✅ | ✅ | **sink plugin** (`--features email`); lettre blocking SMTP, STARTTLS/TLS; schema-driven UI verified live (real send not yet tested against a live SMTP server) |
| Discord channel / DM send | ✅ | 🔩 | gating only; needs platform adapter |
| Google Drive (publish summaries) | ✅ | ✅ | **sink plugin** (`--features gdrive`); publishes HTML as a Google Doc via Drive multipart upload, OAuth refresh-token per workspace. Real upload not yet tested against live Drive; obtaining the refresh token still needs a connect-flow UX (token pasted for now) |
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
| Cost / spend dashboard | partial | ✅ | `Spend.tsx` — total + recent-window + per-model breakdown |
| Audit log | ✅ | ✅ | `Audit.tsx` — admin/security events, newest first (Admin+) |
| Live updates | ✅ | ✅ | SSE (`summary.created/deleted`) |
| Per-tenant branding | ✅ | 🟡 | accent + name; no logo/full theme |
| Extra legacy pages (Tenants admin, Jobs, Archive) | ✅ | ⛔ | not built (Wiki, Slack, Audit log now have tabs) |

## Auth / identity / multi-tenancy

| Feature | Legacy | Rewrite | Notes |
|---|---|---|---|
| Sessions / JWT / refresh / logout | ✅ | ✅ | HS256, revocation (`host/auth`) |
| OAuth redirect (Google/Discord) | ✅ | ✅ | authorization-code + PKCE + signed state (`--features oauth`); needs provider keys; live flow not yet run end-to-end |
| OAuth (Slack) | ✅ | ⛔ | not added |
| Email magic-link | ✅ | 🔩 | dev provider stub (no real link delivery) |
| Workspace grants from membership | ✅ | ⛔ | login still grants requested workspaces; should derive from memberships |
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
| Rolling-ingest dedup (SUM-010) | ✅ | 🔩 | **planned, ADR-129**: content-hash unit ids (upsert) + semantic near-dup gate + delta-only ingest + replace-set updates, so rolling accumulation/re-synthesis/edits don't duplicate units in the vector store. Lands with ADR-101 wiring (Layer 1 can ship sooner) |

## Storage / ops

| Feature | Legacy | Rewrite | Notes |
|---|---|---|---|
| SQLite persistence + repository pattern | ✅ | ✅ | `repository/` traits + `SqliteRepository` |
| Migration framework | ✅ (58+ tracked) | ✅ | `schema_migrations` ledger + ordered runner; idempotent baseline, future changes append as `(id, sql)` |
| Audit log | ✅ | ✅ | `audit_log` ledger surfaced: `GET /workspaces/:ws/audit` (Admin+, tenant-member-scoped) + `Audit.tsx` tab; member role/remove + invite issuance now write entries (WSP-014). Per-tenant column + more instrumented events are a refinement |
| Docker / Fly / Render deploy configs | ✅ | ⛔ | single binary; no container/deploy config |
| Monitoring / metrics | ✅ | 🟡 | `GET /metrics` Prometheus gauges (tenants/workspaces/summaries/schedules/spend) + stderr logs; request-rate counters are a follow-up |
| WASM sandbox boundary | n/a | 🟡 | architecture proven; only WhatsApp parse runs in WASM |

---

## Remaining work to reach legacy parity (rough priority)

1. **Live platform I/O** — Discord + Slack fetch are live, and a schedule can fetch fresh messages before each run (ADR-128). Remaining: channel/DM **send** (deliverer side) and a background poller that syncs on its own cadence (independent of summary schedules).
2. **Delivery completion** — email (SMTP) deliverer; Confluence publishing; output formats beyond markdown.
3. **Real OAuth** — replace the dev login seam with Discord/Google/Slack redirect flows.
4. **Rolling-period summaries (ADR-101)** — wire the pure `decide_rolling` policy end-to-end: rolling-summary storage + the one-active-per-schedule invariant, runner accumulate/finalize, the Hybrid merge, rolling dedup (ADR-118), per-destination delivery (ADR-108), and the period/strategy UI. The state machine is done; storage + orchestration + UI are not.
5. **Production hardening** — Docker/deploy configs (migration runner ✅, basic `/metrics` ✅, audit-log surfacing ✅).
6. **Knowledge subsystem (Phase 7)** — ✅ done: units + semantic search + coherence gate + wiki synthesis (ADR-127); AI curator + rolling-ingest dedup (below) deferred.
7. **Nice-to-haves** — per-perspective & custom prompts, push templates, summary caching, extra dashboard pages, Google Drive, voice transcription.

Items intentionally **not** carried over unless a need appears: summary caching, some legacy dashboard pages, guild-era constructs (the rewrite is workspace-native by design).
