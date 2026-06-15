# ADR-133: v2 dashboard parity — the screens we're still missing

> Rewrite-era ADR. Numbering continues from the local ADR set (…, ADR-132).

- **Status**: Accepted (2026-06-14) — **A1 Coverage**, **A2 Prompts/Perspectives**,
  **A3 Errors**, and **all of §B** shipped (Jobs record+buckets/filters/details &
  `Regenerate`; Schedules prompt-template/perspective/title/continuity; Summaries
  perspective+kind facets & Calendar view; Channels bot-accessibility). A4 Feeds,
  A5 Webhooks-manager, A6 Overview, A7 RuVector, A8 Populate remain. Live-only
  follow-ups: Discord channel-permission resolution; per-summary Length filtering;
  host-side error recording at scheduled-run/delivery sites.
- **Deciders**: Martin Cleaver
- **Related**: ADR-013/040 (jobs), ADR-072/112/121 (coverage), ADR-067/077/090/127
  (wiki/knowledge), ADR-088/089/101 (retrospective/rolling), ADR-126 (delivery
  plugins), ADR-122 (in-chat commands). Supersedes nothing; it *adds* surface.

## Context

We walked the **live v2 production app** (`https://summarybot.app`, guild
`1283874310720716890` — "Agentics Foundation") with an authenticated session and
its `/api/v1` surface. v2 is a Vite SPA whose auth lives in a persisted zustand
store (`localStorage["summarybot-auth"] = {state:{token,user,guilds}}`); the
guards key off `!!token`. (Seeding only `localStorage["token"]` — what the fetch
layer reads — is *not* enough; that's why an earlier pass only saw the API and
the logged-out landing page. Seeding the real store renders every screen. We
also learned not to persist `isAuthenticated`: it's a store **method**
(`()=>!!token`), and overwriting it with a boolean crashes the app.)

v2's **left-nav screen inventory** (authoritative):

```
Overview · Summaries · Jobs
SOURCES     All Sources · Channels · WhatsApp
AUTOMATION  Schedules · Feeds · Webhooks
KNOWLEDGE   Wiki · RuVector Explorer · Populate · Coverage
HISTORY     Retrospective · Audit Log
SETTINGS    Prompts · Errors · Settings
```

Our rewrite's nav: `Create · Summaries · Schedules · Knowledge · Spend · Jobs`,
`Sources(WhatsApp · Discord · Slack)`, `Admin(Delivery · Plugins · Members ·
Audit · Settings)`.

Mapping the two, several **whole screens v2 ships are absent** from the rewrite,
and several screens we *do* share are markedly thinner. This ADR records the gap
and decides to close it. **It is acceptable — expected — for the rewrite to
exceed v2** (we already do in several places, §"Where we already exceed v2");
parity is a floor, not a ceiling.

### Live evidence (this guild, pulled 2026-06-14)

- **Coverage**: `124.6%` total coverage, `166` gaps, `71/71` channels with
  summaries, content range `2024-09-26 → 2026-04-28`, plus a per-channel
  breakdown (`💼︱job-board` 11 summaries, 21/60 days, 3 gaps, 35% …).
- **Jobs**: status buckets (Running/Pending/Completed-24h/Failed/Paused) and
  job types `scheduled, manual, retrospective, regenerate, wiki_backfill`. Each
  job row carries scope, channel count, schedule name, and continuity/rolling
  flags (e.g. "Weekly Sat 1d continuity rolling-weekly").
- **Feeds**: RSS/Atom output of summaries; one public feed has **11,484
  accesses**; private/public, per-channel, access counts.
- **Prompts**: "Custom Perspectives" — named templates *beyond* the built-ins
  **General, Developer, Marketing, Executive, Support**; each shows how many
  schedules use it.
- **Webhooks**: typed (Discord/…), with `last delivery` + `last_status`.
- **Channels**: bot-permission awareness ("11 channels not accessible … grant
  Read Message History"; restricted/🔒 channels flagged).
- **Summaries** filters: Source, Scope, Platform, Granularity, **Continuity**,
  **Rolling**, Schedule, Length, **Perspective**, plus **List/Calendar** views
  and an Archived toggle.

## Decision

Build the missing v2 screens in the rewrite's Rust(`domain→repository→host→api`)
+ React stack, prioritized below. Each lands as a tested, browser-verified
increment with `docs/coverage-map.md` + `docs/product/` kept current, behind the
existing feature-flag conventions where it needs new I/O.

### A. Whole screens to add

| # | Screen | What it is (from v2) | Rewrite home | Backend work |
|---|--------|----------------------|--------------|--------------|
| A1 | **Coverage** | Server-wide + per-channel summarization coverage: % covered, gap periods, channels-with-summaries, content date range. Generalizes our WhatsApp-only coverage (ADR-072/112/121) to all sources. | new `Coverage` tab | `host` coverage service over the message+summary stores; `GET /workspaces/:ws/coverage` (totals + per-channel). Compute on demand, cache `computed_at`. |
| A2 | **Prompts / Perspectives** | Named, reusable prompt templates + built-in perspectives (General/Developer/Marketing/Executive/Support). Selectable per schedule and on-demand. Today we have a single per-workspace `summary_instructions`. | new `Prompts` tab | `prompt_templates` table (`id, workspace_id, name, content, based_on_default, usage_count`); `domain` perspective enum; CRUD API; thread `prompt_template_id`/`perspective` into the summarize request. |
| A3 | **Errors** ✅ *shipped* | Operational error log (fetch/LLM/delivery failures) with operation, severity, class, message, scope, resolved-flag + bulk-resolve. Distinct from the human Audit log. | `Errors` tab | `operational_errors` table + `OperationalErrorRepository`; `FailureClass::severity()`; recorded at the sync soft-fail (per-channel + top-level, ADR-041/097) and on-demand-summarize failure sites; `GET /errors`, `POST /errors/:id/resolve`, `POST /errors/resolve-all`. Host-side scheduled-run/delivery recording is a follow-up. |
| A4 | **Feeds** | RSS/Atom output of a workspace's summaries; public or private; per-channel or all; access counts. | new `Feeds` tab | `feeds` table (`id, channel_id, type, is_public, url_token, access_count`); a public `GET /feeds/:token` renderer (Atom/RSS); CRUD API. |
| A5 | **Webhooks** | Dedicated outbound-webhook manager: typed, enabled, last delivery + status. Today webhooks are one row-type inside Delivery. | fold into `Delivery` (a Webhooks section) or a sibling tab | surface `last_delivery`/`last_status` on `workspace_destinations`; no new table. |
| A6 | **Overview** | Per-workspace dashboard home: counts (summaries, schedules, members), recent activity, config status. | new default tab | read-only aggregate endpoint; mostly composition of existing data. |
| A7 | **RuVector Explorer** *(optional)* | Browse/inspect the vector store (embeddings) for debugging knowledge. | under `Knowledge` | read-only `GET /workspaces/:ws/knowledge/vectors` projection over stored units/embeddings. |
| A8 | **Populate** *(optional)* | Explicit "backfill/ingest history now" trigger with progress. Our Create→Past + retrospective covers most of this; Populate is the always-available manual ingest. | under `Sources` or `Create` | reuses the `Sync`/`Backfill` jobs (ADR-013/040) — mostly a UI affordance. |

### B. Enrich screens we already share — ✅ shipped

- **Jobs** — we just wired job tracking (ADR-013/040). Align with v2:
  - **Add `JobType::Regenerate`** and a regenerate flow (re-run a stored summary
    with changed params/perspective). v2 ships this; our last increment
    **wrongly deferred** it.
  - Keep our **`Sync`** job type as a deliberate superset (v2 doesn't track sync
    as a job; observing it is an improvement — recorded as an intentional
    divergence, not an accident).
  - Enrich the `Job` record toward v2's shape: `schedule_name`, `scope`,
    `channel_ids`/count, `summary_ids`, `date_range`, `creation_source`,
    `pause_reason`; add status **buckets** + type/status filters in the view.
- **Schedules** — add `prompt_template_id`, `title_template`, `enable_continuity`,
  and `perspective` (we already have scope/rolling/destinations).
- **Summaries** — add **Perspective**, **Continuity**, **Rolling**, and
  **Granularity** filters and a **Calendar** view (we added text/participant/tag/
  archived in ADR-035/037; this extends them).
- **Channels** — surface **bot-permission/accessibility** state (inaccessible +
  restricted channels) in the Discord/Slack channel browser.

### C. Where we already exceed v2 (do not regress)

- **Per-claim grounded references** (ADR-004) — each key point cites its sources;
  v2 only flags a summary "Grounded".
- **Latency + token counts** in summary metadata (ADR-106) and a **Spend**
  analytics dashboard — v2 surfaces cost only per-job.
- **Tenancy/RBAC** (Tenants/Members), **two-layer delivery plugins** + OAuth
  **Connect** wizards (Drive/Confluence, ADR-126/132), **RVF export** (ADR-117),
  **operator plugin veto** (ADR-131).

These stay. Parity work must not flatten them back to v2's level.

## Priority / sequencing

1. **Coverage (A1)** — high user value, read-only over existing data, no secrets.
2. **Prompts/Perspectives (A2)** — unlocks richer Schedules/Summaries (B) and is
   referenced by many v2 schedules.
3. **Jobs Regenerate + enrichment (B)** — small, corrects the known divergence.
4. **Errors (A3)** — observability; reuses `FailureClass` + existing fail sites.
5. **Feeds (A4)** — netnew output channel; public endpoint needs care.
6. **Webhooks polish (A5)** + **Overview (A6)** — mostly composition.
7. **RuVector / Populate (A7/A8)** — optional/debug; do last.

## Consequences

- **Good**: closes the visible-surface gap a migrating v2 user would notice
  first (Coverage, Prompts, Errors, Feeds), on a typed/tested core; several land
  as read-only projections (low risk).
- **Cost**: A2/A3/A4 add tables + migrations and new API/web surface; this is
  multi-increment work, not one commit.
- **Risk**: Feeds adds a *public* unauthenticated endpoint (`/feeds/:token`) —
  must use unguessable tokens, honor `is_public`, and never leak private content.
- **Non-goals here**: RuVector/Populate are explicitly optional; the rewrite may
  choose better equivalents rather than copy them.

## How we verified v2 (reproducible)

Headless Playwright with the auth store seeded before app scripts run:

```js
// build user from JWT claims; fetch real guilds; seed BOTH keys
localStorage.setItem('summarybot-auth',
  JSON.stringify({ state: { token, user, guilds }, version: 0 })) // NOT isAuthenticated
localStorage.setItem('token', token)                              // fetch layer
// then navigate /guilds/:id/{summaries,coverage,jobs,feeds,prompt-templates,errors,...}
```
