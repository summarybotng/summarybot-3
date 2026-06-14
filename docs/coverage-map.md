# Coverage map — legacy summarybot-ng vs. the rewrite

At-a-glance: how much of the original Python **summarybot-ng** the Rust/WASM
rewrite covers, and what's left. **Keep this current** — see
[conventions/keep-coverage-map-current](conventions/keep-coverage-map-current.md).

- **As of:** 2026-06-14 — **per-claim grounded references** (ADR-004: each key point carries its own source references — message id, author, position, snippet — + confidence, shown inline); **RVF knowledge export** (ADR-117: download knowledge units as an `.rvf` binary or JSON from the Knowledge tab; magic `RVF1`, optional embeddings); **Confluence Atlassian OAuth Connect wizard** (ADR-132: one-click Connect captures a refresh token + cloudId; the deliverer prefers OAuth and falls back to the legacy API-token Basic path); **WhatsApp chat name auto-detected on import** (v2 ADR-081 parity — no need to type a channel id for a group export); **platform-operator per-tenant plugin disable** (ADR-131: an operator veto, default-on, that a tenant can't override; operator controls in the Plugins tab); **per-destination rolling delivery** (ADR-108: a "deliver on each run" flag on a destination → rolling schedules push the in-progress digest to opted-in destinations every run and the finalized digest to all); **Discord-category schedule scope** (ADR-011: a `category:<id>` scope the runner resolves to the category's live channels and summarizes across; selectable in the Create wizard once a Discord source is connected); **unified "Create Summary" wizard** (ADR-088/089: a Create tab with a What→When flow over Now/Recurring/Past, dispatching to the existing summarize-now/schedule/retrospective endpoints); **per-schedule delivery destinations** (ADR-014: a destination picker pins a schedule to a chosen subset; `schedule_destinations` + runner filter); **tenant discovery** ("Your tenants" picker via `GET /tenants` — no more typing a tenant id, TEN-001); **retrospective weekly summaries** of an imported chat ("Summarize by week", ADR-088/089/101); v2 ADRs brought in as first-class spec (`docs/reference/v2-adr/`); UX-reachability matrix audited against the ADRs (per-schedule destinations, schedule scope, jobs/progress, create-summary wizard called out); Discord server + channel browser (pick a server/channels, no pasted ids); a new **[UX reachability](#ux-reachability)** matrix tracking whether each capability is actually usable in the dashboard (not just built); HTTP request metrics + structured access logs; Hybrid/Resummarize rolling merge; rolling-ingest dedup Layer 4 (provenance-merge); AI wiki curator (advisory report); two-layer delivery plugins (per-tenant enable + connect + config; Google Drive OAuth connect flow); membership-derived workspace grants; Discord/Slack channel send-back sinks; Docker + compose + Fly deploy config; WhatsApp coverage complete (WHA-014..019); Tenants/Members admin tab (RBAC); rolling wired end-to-end (ADR-101)
- **Legend:** ✅ done & tested · 🟡 partial · 🔩 seam only (trait/config/policy, no live impl) · ⛔ not started · ➖ out of scope / dropped
- **Legacy** column = does the old product have it. **Rewrite** = our status.

## Scorecard (where the work is)

| Area | Rewrite vs legacy | Headline gap |
|---|---|---|
| Core summarization | ✅ **at/above parity** | map-reduce, citations, cost cap, budgets — done |
| WhatsApp ingestion | ✅ **at parity** | full pipeline, anonymized, deduped |
| Scheduling & rolling digests | ✅ **at parity** | recurrence + per-tenant LLM/budget + live-source fetch + **rolling-period accumulation wired end-to-end** (ADR-101), Append + Hybrid/Resummarize merges + per-destination intermediate-vs-finalize delivery (ADR-108) |
| Multi-tenancy & roles | ✅ **at/above parity** | tenants, members, invites, routing |
| Dashboard / web UI | ✅ **at parity** | 11 tabs + live SSE; incl. Knowledge, Spend, Audit, Tenants/Members admin |
| Delivery | ✅ **~95%** | plugin seam ✅, webhook/Confluence/email/Google Drive ✅; Discord/Slack channel **send** ✅; DM send + per-destination templates remain |
| Discord / Slack ingestion | ✅ **both live + scheduled** | Discord + Slack REST fetch → message store (generic `connections/:platform` API), with a **server + channel browser** (pick the Discord guild from the bot's servers, then its channels grouped by category — no id-pasting); a schedule can fetch fresh messages before each run (ADR-128). Channel **send-back** shipped; DM send + an independent background poller remain |
| Auth / OAuth | 🟡 **mostly there** | sessions/roles ✅; real OAuth login ✅ (Google/Discord); workspace grants now membership-derived (token filtered to entitlement) ✅ |
| Knowledge (wiki / vector search) | ✅ **v1 done** | semantic search + coherence gate + wiki synthesis live (ADR-127); rolling-ingest dedup all 4 layers; AI curator (advisory report) shipped |
| External integrations | ⛔ **mostly absent** | Confluence, Google Drive, voice transcription |
| Ops (Docker, migrations, metrics) | ✅ **deployable** | container + compose + Fly config ✅; migration ledger ✅; `/metrics` DB gauges + HTTP request counters ✅; structured JSON access logs ✅ |

**Rough read:** the *summarize → schedule → deliver-to-dashboard/webhook* spine is
done and multi-tenant, and the knowledge subsystem (units + semantic search +
coherence + wiki, ADR-127) now ships. The remaining work clusters in three
buckets: (1) **live platform I/O** (Discord/Slack **send**, email/Confluence
delivery, real OAuth — fetch is done), (2) **production hardening** (Docker/deploy;
migrations + basic metrics + audit done), and (3) **refinements** (rolling now
wired end-to-end with Append + Hybrid/Resummarize merges + per-destination
intermediate delivery (ADR-108); rolling-ingest dedup ADR-129 fully shipped).

---

## UX reachability

The feature tables answer *"is it built?"* — but a feature can be built and still
be **unreachable or painful in the dashboard**, and the status map won't show it.
That blind spot is exactly what let the Discord case slip: the backend could list
a server's channels, yet the UI made you paste a guild id + comma-separated
channel ids, and couldn't list servers at all. The feature row said ✅; the
*experience* was 🟡/🔌.

This matrix tracks **UX reachability**: can a typical user accomplish the job
**through the dashboard, end to end**, without pasting raw ids, dropping to the
API, or rebuilding the server? A feature is only **✅ self-serve** when the answer
is yes. Maintain it alongside the feature rows (see
[conventions/keep-coverage-map-current](conventions/keep-coverage-map-current.md)) —
when you ship UI for a capability, move its row up; when a capability lands
backend-first, record it as 🔌 so the gap is visible until the UI catches up.

**Legend:** ✅ self-serve · 🟡 reachable but rough (raw ids / manual steps /
server config or feature flag needed) · 🔌 backend only (works via API, no UI
surfaces it) · ⛔ not built · ➖ operator/CLI surface by design.

| User-facing job | Reachable | Where | Notes |
|---|---|---|---|
| Sign in (dev) | ✅ | Login | workspace name + Dev sign-in |
| Sign in via Google/Discord OAuth | 🟡 | Login | needs `--features oauth` + provider keys; button present otherwise errors |
| Discover / switch my workspaces | ✅ | nav | a workspace switcher in the sidebar over the session's granted set (membership-derived at login); dev sign-in accepts several comma-separated |
| Provision a tenant | ✅ | Settings | a "Your tenants" picker lists the tenants you belong to with your role (`GET /tenants`); pick one to load it, or type an id to join/create (TEN-001) |
| Manage members + invites | ✅ | Members | |
| Import a WhatsApp chat + see coverage/gaps | ✅ | WhatsApp | timeline, contributors, classified gaps |
| Request a scoped import to fill a gap | ✅ | WhatsApp | auto-fulfilling invitation |
| Connect a Discord/Slack bot token | ✅ | Discord / Slack | |
| Pick a Discord **server** | ✅ | Discord | Load servers → dropdown (**was 🔌** before this work) |
| Pick **channels** by category | ✅ | Discord / Slack | Browse channels (**was 🟡** — pasted ids) |
| Sync source messages | ✅ | Discord / Slack | selected channels or all |
| Summarize a channel / pasted messages | ✅ | Source / Summaries | |
| Search / filter / pin / archive / tag summaries | ✅ | Summaries | |
| Set per-workspace summary instructions | ✅ | Settings | |
| Create / edit / pause / run a schedule | ✅ | Schedules | |
| Configure a rolling digest (period + merge) | ✅ | Schedules | Append / Hybrid |
| **Retrospective** weekly summaries of an imported chat | ✅ | WhatsApp | "Summarize by week" on the coverage card → one summary per non-empty week of history (ADR-088/089 retrospective, ADR-101/048) |
| Forward weekly *rolling* digest of an imported chat | ✅ | WhatsApp | one-click "Schedule weekly digest" on the coverage card (rolling weekly scoped to the chat) |
| Generate-now with a time-range picker | ✅ | WhatsApp | "summarize last 24h / 7d / 30d" presets on the coverage card (ADR-089 Now); Source tab has a lookback-days field |
| Unified "create summary" wizard (now / schedule / retrospective) | ✅ | Create | a 2-step wizard (What → When) on a new **Create** tab unifies all three flows: pick platform + channel/chat, then Now (range presets) / Recurring (schedule + rolling) / Past (by-week retrospective). Dispatches to the existing summarize-now, createSchedule, and summarize-weeks endpoints (ADR-088/089) |
| View summary detail (key points, action items) | ✅ | Summaries | expandable card |
| Per-destination rolling delivery control | ✅ | Delivery | a per-destination "deliver on each run" flag (ADR-108): rolling schedules push the in-progress digest to flagged destinations every run (no dashboard spam), and the finalized digest to all. Stored on `workspace_destinations`; the runner filters intermediate vs finalize delivery |
| Choose delivery destinations *per schedule* | ✅ | Schedules | a destination picker pins a schedule to a chosen subset; empty = all enabled (ADR-014). Stored in `schedule_destinations`; the runner filters delivery to the selection. Pairs with the per-destination intermediate-vs-finalize flag (ADR-108) |
| Schedule scope = all-channels (workspace) | ✅ | Schedules | "all channels" toggle → a workspace-wide digest across every channel (ADR-011) |
| Schedule scope = Discord category | ✅ | Create | a `category:<id>` scope: the runner resolves the category's current channels from the bound Discord source at run time (reusing the tested `resolve_channels`), syncs them, and summarizes across them (tagged `category:<id>`). The Create wizard offers a per-category option once a Discord source is connected (ADR-011). Live HTTP resolution is exercised only with a real Discord token; the resolve→merge→summarize chain is unit-tested |
| Jobs view (long-running work + progress) | ✅ | Jobs | a Jobs tab lists background work with status/progress/cost; the retrospective by-week run records a job (ADR-040). Live streaming progress mid-run is a refinement |
| Wiki raw units + provenance | ✅ | Knowledge | "Knowledge units (raw)" lists every fact with its source-message count (ADR-063) |
| Export knowledge units (RVF / JSON) | ✅ | Knowledge | "Export .rvf" / "JSON" download all units (ADR-117): `RVF1` binary header + records (id, type, content, source, date, confidence, optional embedding) with a CRC32, or a JSON fallback. `source_channel`/`confidence` default and `source_date` derives from unit creation until those are tracked per unit |
| Add a delivery destination (target) | ✅ | Delivery | workspace-target fields only |
| Enable + configure a tenant plugin | ✅ | Plugins | tenant credentials, once |
| Operator disables a plugin per tenant | ✅ | Plugins | a config-based platform operator (ADR-119) can veto a plugin for a tenant; the veto beats tenant enablement and survives a reconnect/clear; plugins default on (ADR-131). Operator controls appear in the Plugins tab; tenant admins see a read-only "disabled by operator" advisory |
| Connect Google Drive (OAuth) | 🟡 | Plugins | Connect button present; needs server `GOOGLE_CLIENT_ID/SECRET` |
| Connect Confluence (OAuth) | 🟡 | Plugins | Connect button (Atlassian 3LO; ADR-132) captures a refresh token + cloudId; deliverer prefers OAuth, falls back to legacy API-token. Needs server `ATLASSIAN_CLIENT_ID/SECRET` |
| Test a destination | ✅ | Delivery | sample send |
| Semantic knowledge search | ✅ | Knowledge | |
| (Re)generate the wiki page | ✅ | Knowledge | |
| Curator health report (duplicates/stale) | ✅ | Knowledge | advisory |
| Apply curator suggestions (prune duplicates) | ✅ | Knowledge | "Prune duplicates" removes redundant units, folding their provenance into the survivor; audit-logged (CUR-*). One-click undo is the remaining refinement |
| BYO LLM + budget | ✅ | Settings | |
| Spend analytics | ✅ | Spend | |
| Audit log | ✅ | Audit | Admin+ |
| Deploy / metrics / structured logs | ➖ | (ops) | Docker/compose/Fly; `/metrics`, JSON access logs |

**Reading the gaps (audited against the v2 ADRs):** there are **no ⛔ rows left**.
The only remaining 🟡 rows are **OAuth sign-in** and **Google Drive Connect**,
both of which work but need server config (`--features oauth` + provider keys)
rather than more code. Everything this matrix opened as a gap — per-schedule
destinations, all-channel **and category** schedule scope, **per-destination
rolling delivery** (ADR-108), jobs/progress, the curator apply step, the wiki
raw-provenance tab, tenant discovery, and the unified create-summary wizard — is
now closed and tested. None are 🔌 (shipped backend, no UI). The two 🟡 rows are
config-dependent, not code debt; keep closing against the spec, not by guessing.

---

## Ingestion / message sources

| Feature | Legacy | Rewrite | Notes |
|---|---|---|---|
| WhatsApp import (zip / `_chat.txt`) | ✅ (REST ingest) | ✅ | `host/whatsapp.rs`, `api/whatsapp.rs`; iOS/Android parse, TZ + date-order inference (WHA-001/020); **chat id auto-detected from the group name** (creation / subject-change system lines) so it needn't be typed — v2 ADR-081 parity |
| PII anonymization at ingest | ✅ | ✅ | HMAC phone→pseudonym before storage (WHA-006) |
| Dedup (file + message level) | ✅ | ✅ | SHA-256 + synthetic fingerprint (WHA-010/012) |
| WhatsApp coverage + history gaps | ✅ | ✅ | per-chat import spans merged → classified `before_join`/`between_imports`/`after_last` gaps; group-creation anchor detected from chat events (WHA-014/015/016); `host/coverage.rs`, `GET /workspaces/:ws/whatsapp/chats[/:chat/coverage]`. Each import now recorded in `whatsapp_imports` (was never populated before) |
| Coverage timeline + "ask members to export X" | ➖ | ✅ | `Whatsapp.tsx` covered-vs-gaps bar, coverage %, copy-ready scoped export instructions per fillable gap (WHA-017) |
| Contributor tracking | ✅ | ✅ | per-chat rollup of who supplied which date range / how many imports + messages, from the attributed import records; `host::contributors_for`, surfaced on the coverage card (WHA-018) |
| Scoped import invitations (persisted, auto-fulfilled) | ✅ | ✅ | `whatsapp_import_invitations` table; open a tracked ask against a gap, auto-marked `fulfilled` (credited to the contributor) when a covering import lands, or `cancelled`; `host/coverage.rs` reconcile wired into ingest; `POST .../chats/:chat/invitations[/:id/cancel]`; Request/Cancel in `Whatsapp.tsx` (WHA-019) |
| Discord message fetch | ✅ (discord.py) | ✅ | `DiscordFetcher` over Discord REST v10 (blocking `ureq`, `--features discord`); pure normalization unit-tested; bot token encrypted; fetch persists to the message store (ADR-128). Verified live (real 401 on a bogus token). **Server + channel browser:** `GET /connections/discord/servers` lists the guilds the bot is in, and `…/channels?scope_id=<guild>` lists that server's text channels grouped by category — pick both point-and-click instead of pasting ids (WSP-006). Gateway/streaming + polling scheduler deferred |
| Slack message fetch | ✅ (OAuth + history) | ✅ | `SlackFetcher` over the Slack Web API (`conversations.list`/`.history`, blocking `ureq`, `--features slack`); pure normalization unit-tested; bot token encrypted; fetch persists to the message store (ADR-128). Verified live (real `invalid_auth` on a bogus token). **Channel browser:** `GET /connections/slack/channels` lists conversations (flat — Slack has no categories) for selection (WSP-006). Bot must be a channel member; Slack OAuth install flow deferred |
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
| Grounded citations (per-claim, ADR-004) | ✅ | ✅ | **each key point is a `ReferencedClaim`** carrying its own source references (message id, author, 1-based position, snippet) + a confidence; shown inline on summaries as "(sources: #N author)". Resolved from per-claim citation indices; stored in the `summary_key_points` child table; the summary-level citation union is retained for provenance. Legacy rows fall back to text-only |
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
| Confluence publishing | ✅ | ✅ | **sink plugin** (`--features confluence`); Cloud REST. **Atlassian OAuth Connect** (ADR-132) is the primary auth — refresh→Bearer→`/ex/confluence/{cloudId}`; legacy API-token Basic path retained. Connect wizard verified (returns an auth.atlassian.com consent URL); real publish needs a live Atlassian app |
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
| AI wiki curator (CUR-*) | ✅ | 🟡 | **advisory health report shipped**: `host::CuratorService` flags duplicate clusters (cosine ≥ 0.95 over stored embeddings, oldest = canonical) + stale units; `POST /workspaces/:ws/wiki/curate`, Curator panel in `Knowledge.tsx`. Read-only (reversible by construction) + audit-logged. Auto-apply (merge/prune with undo) + LLM topic re-org are the remaining refinement |
| Rolling-ingest dedup (SUM-010) | ✅ | ✅ | **All 4 layers shipped (ADR-129)**: content-addressed unit ids (exact repeats collapse on upsert), a **semantic near-dup gate** (cosine ≥ threshold, default 0.93), **delta-only ingest** (the rolling runner feeds each period's delta into the knowledge base as it accumulates), and **provenance-merge (Layer 4)** — a re-stated/paraphrased fact merges its source message ids into the matched existing unit (strengthening grounding, COH-005) instead of being dropped. All unit-tested |

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
4. **Rolling-period summaries (ADR-101)** — ✅ wired end-to-end (storage, runner accumulate/finalize, API/web config, Append + Hybrid/Resummarize merges, per-destination intermediate-vs-finalize delivery ADR-108, rolling-ingest dedup Layer 4 ADR-129).
5. **Production hardening** — ✅ container + compose + Fly config (migration runner ✅, basic `/metrics` ✅, audit-log surfacing ✅). Remaining: request-rate counters + structured logs.
6. **Knowledge subsystem (Phase 7)** — ✅ done: units + semantic search + coherence gate + wiki synthesis (ADR-127); AI curator + rolling-ingest dedup (below) deferred.
7. **Nice-to-haves** — per-perspective & custom prompts, push templates, summary caching, extra dashboard pages, Google Drive, voice transcription.

Items intentionally **not** carried over unless a need appears: summary caching, some legacy dashboard pages, guild-era constructs (the rewrite is workspace-native by design).
