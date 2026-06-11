# Coverage map — legacy summarybot-ng vs. the rewrite

At-a-glance: how much of the original Python **summarybot-ng** the Rust/WASM
rewrite covers, and what's left. **Keep this current** — see
[conventions/keep-coverage-map-current](conventions/keep-coverage-map-current.md).

- **As of:** 2026-06-08 — ADR-126 sink plugins (webhook/Confluence/email) + OAuth login
- **Legend:** ✅ done & tested · 🟡 partial · 🔩 seam only (trait/config/policy, no live impl) · ⛔ not started · ➖ out of scope / dropped
- **Legacy** column = does the old product have it. **Rewrite** = our status.

## Scorecard (where the work is)

| Area | Rewrite vs legacy | Headline gap |
|---|---|---|
| Core summarization | ✅ **at/above parity** | map-reduce, citations, cost cap, budgets — done |
| WhatsApp ingestion | ✅ **at parity** | full pipeline, anonymized, deduped |
| Scheduling & rolling digests | ✅ **at parity** | per-tenant LLM+budget now wired |
| Multi-tenancy & roles | ✅ **at/above parity** | tenants, members, invites, routing |
| Dashboard / web UI | ✅ **core parity** | 5 tabs + live SSE; missing legacy's extra pages |
| Delivery | 🟡 **~70%** | plugin seam ✅, webhook/Confluence/email ✅; platform channel/DM send 🔩 |
| Discord / Slack ingestion | 🔩 **seam only** | trait exists, no live fetcher or bot |
| Auth / OAuth | 🟡 **mostly there** | sessions/roles ✅; real OAuth login ✅ (Google/Discord); workspace grants not yet membership-derived |
| Knowledge (wiki / vector search) | ⛔ **not started** | whole legacy subsystem absent (Phase 7) |
| External integrations | ⛔ **mostly absent** | Confluence, Google Drive, voice transcription |
| Ops (Docker, migrations, metrics) | 🟡 **thin** | single binary; ad-hoc schema; minimal telemetry |

**Rough read:** the *summarize → schedule → deliver-to-dashboard/webhook* spine is
done and multi-tenant. The remaining work clusters in three buckets: (1) **live
platform I/O** (Discord/Slack fetch + send, email/Confluence delivery, real
OAuth), (2) **the knowledge subsystem** (wiki + vector search — a Phase-7 effort
the rewrite hasn't begun), and (3) **production hardening** (migrations, Docker,
metrics).

---

## Ingestion / message sources

| Feature | Legacy | Rewrite | Notes |
|---|---|---|---|
| WhatsApp import (zip / `_chat.txt`) | ✅ (REST ingest) | ✅ | `host/whatsapp.rs`, `api/whatsapp.rs`; iOS/Android parse, TZ + date-order inference (WHA-001/020) |
| PII anonymization at ingest | ✅ | ✅ | HMAC phone→pseudonym before storage (WHA-006) |
| Dedup (file + message level) | ✅ | ✅ | SHA-256 + synthetic fingerprint (WHA-010/012) |
| Discord message fetch | ✅ (discord.py) | 🔩 | `PlatformFetcher` trait + `FakeFetcher`; no live client |
| Slack message fetch | ✅ (OAuth + history) | 🔩 | trait only; no Slack SDK / OAuth |
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
| Custom prompts per workspace | ✅ | ⛔ | prompt assembled inline; no template service (SUM-007) |
| Retry / resilience / rate limiting | ✅ | ✅ | `ResilientLlm` + token-bucket limiter (ADR-123/124) |
| Summary caching (memory/Redis) | ✅ | ➖ | not ported; not currently needed |

## Cost controls

| Feature | Legacy | Rewrite | Notes |
|---|---|---|---|
| Per-request hard cost cap | ✅ | ✅ | `CostGuard`, degrade-not-overspend (ADR-095) |
| Per-tenant budget (rolling window) | partial | ✅ | `domain/budget.rs` (ADR-125 Phase 3) |
| BYO LLM key, encrypted at rest | n/a | ✅ | AES-256-GCM, `LLM_CONFIG_KEY` (ADR-125 Phase 2b) |
| Operator-lent key + budget | n/a | ✅ | owner-set grants |
| Cost analytics / spend dashboards | partial | ⛔ | spend recorded, not visualized |

## Scheduling / digests

| Feature | Legacy | Rewrite | Notes |
|---|---|---|---|
| Recurrence (once/hourly/daily/weekly/monthly/custom) | ✅ | ✅ | `domain/schedule.rs` |
| Persistent background scheduler | ✅ (APScheduler) | ✅ | `api/scheduler_driver.rs`; grace + auto-disable |
| Per-tenant LLM + budget on scheduled runs | ✅ | ✅ | `TenantAwareRunner` (cfc9fd9) |
| Rolling-period summaries | ✅ | ✅ | `domain/rolling.rs` (ADR-101) |
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
| Google Drive (publish summaries) | ✅ | ⛔ | planned as optional **sink plugin** (ADR-126); blocked on real OAuth |
| Output formats (markdown/html/json/text) | ✅ | 🟡 | markdown `render()`; others not ported |
| Push templates per destination | ✅ | ⛔ | — |

## Web UI / dashboard

| Feature | Legacy | Rewrite | Notes |
|---|---|---|---|
| Summaries (search/filter/pin/archive/tag) | ✅ | ✅ | `web/views/Summaries.tsx` |
| Schedules | ✅ | ✅ | `Schedules.tsx` |
| WhatsApp import | ✅ | ✅ | `Whatsapp.tsx` + summarize-now |
| Delivery destinations | ✅ | ✅ | `Delivery.tsx` (webhook add/test/remove) |
| Settings (LLM config + budget) | ✅ | ✅ | `Settings.tsx` |
| Live updates | ✅ | ✅ | SSE (`summary.created/deleted`) |
| Per-tenant branding | ✅ | 🟡 | accent + name; no logo/full theme |
| Extra legacy pages (Wiki, Slack, Tenants admin, Audit log, Jobs, Archive) | ✅ | ⛔ | not built |

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
| Wiki / knowledge base (pages, versioning, cross-link) | ✅ (extensive) | ⛔ | Phase 7, not begun |
| Vector store / semantic search (RuVector + embeddings) | ✅ | ⛔ | Phase 7 |
| Knowledge synthesis from summaries | ✅ | ⛔ | Phase 7 |
| Coherence / hallucination gate | ✅ | ⛔ | Phase 7 |

## Storage / ops

| Feature | Legacy | Rewrite | Notes |
|---|---|---|---|
| SQLite persistence + repository pattern | ✅ | ✅ | `repository/` traits + `SqliteRepository` |
| Migration framework | ✅ (58+ tracked) | 🔩 | schema created ad-hoc in repo init; no runner |
| Audit log | ✅ | 🟡 | `audit_log` table exists; limited surfacing |
| Docker / Fly / Render deploy configs | ✅ | ⛔ | single binary; no container/deploy config |
| Monitoring / metrics | ✅ | ⛔ | stderr logs only |
| WASM sandbox boundary | n/a | 🟡 | architecture proven; only WhatsApp parse runs in WASM |

---

## Remaining work to reach legacy parity (rough priority)

1. **Live platform I/O** — Discord + Slack fetchers and channel/DM send (turns the `PlatformFetcher`/deliverer seams into real adapters). Largest single bucket.
2. **Delivery completion** — email (SMTP) deliverer; Confluence publishing; output formats beyond markdown.
3. **Real OAuth** — replace the dev login seam with Discord/Google/Slack redirect flows.
4. **Knowledge subsystem (Phase 7)** — wiki + vector search + synthesis + coherence gate. Big, self-contained; legacy's most distinctive feature set.
5. **Production hardening** — migration runner, Docker/deploy configs, metrics/monitoring, audit-log surfacing.
6. **Nice-to-haves** — per-perspective & custom prompts, push templates, summary caching, cost analytics, extra dashboard pages, Google Drive, voice transcription.

Items intentionally **not** carried over unless a need appears: summary caching, some legacy dashboard pages, guild-era constructs (the rewrite is workspace-native by design).
