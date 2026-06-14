# Capabilities

What SummaryBot can do **today**. Each capability below is shipped and tested
(cross-referenced to the [coverage map](../coverage-map.md)). Items still on the
roadmap are listed at the end so the boundary is explicit.

## Conversation ingestion

- **WhatsApp import** — upload a `.zip` (or `_chat.txt`) export; iOS and Android
  dialects are detected, timezone + date order resolved (the file carries
  neither), and messages parsed into the normalized model. Re-uploading the same
  export is idempotent.
- **PII anonymization at ingest** — phone numbers are HMAC-pseudonymized before
  storage; raw numbers never land in the database.
- **Multi-contributor dedup** — the same message exported by two people collapses
  to one stored row (content fingerprint); identical files are skipped.
- **WhatsApp coverage & history gaps** — per chat, SummaryBot merges everyone's
  import spans and shows a coverage timeline with classified gaps
  (`before_join` / `between_imports` / `after_last`), anchored to the detected
  group-creation date.
- **Contributor tracking** — who supplied which date range, and how much.
- **Scoped import invitations** — open a tracked "please export this date range"
  ask against a gap; it auto-resolves (credited to the contributor) when a
  covering import arrives.
- **Discord & Slack fetch** — connect a bot token and pull recent channel history
  into the message store over the platforms' REST APIs; a scheduled run can fetch
  fresh messages before summarizing.
- **Server & channel browser** — pick the Discord **server** from the guilds the
  bot is in, then its **channels** (grouped by category; Slack is flat), all
  point-and-click — no pasting guild or channel ids.

## AI summarization

- **Structured summaries** — each summary has prose plus key points, action items,
  technical terms, and participants — not just a blob of text.
- **Grounded citations** — claims link back to the source messages that support
  them, resolved post-parse from compact position indices.
- **Three lengths** — brief, detailed, comprehensive.
- **Long-history map-reduce** — conversations beyond the model's context window
  are chunked, summarized per chunk, then reduced, with citations carried through.
- **Resilience & rate limiting** — a model ladder with failure-classified retry,
  plus a process-wide token-bucket limiter.
- **Hard cost cap** — a per-request ceiling degrades to a best-effort partial
  (flagged degraded) rather than overspending.
- **Coherence gate** — a grounding check scores summaries against their source and
  the score is persisted and shown, guarding against hallucination.
- **Per-workspace custom instructions** — free-text guidance appended to the
  prompt (e.g. "emphasize decisions; product-management perspective").

## Scheduling & rolling digests

- **Recurrence** — once, hourly, daily, weekly, monthly, or a custom interval, in
  the schedule's timezone, run by a persistent background scheduler with grace +
  auto-disable on repeated failure.
- **Live-source fetch on run** — a schedule can bind a Discord/Slack source and
  pull fresh messages before each summarization.
- **Rolling-period summaries** — daily accumulation toward a weekly / biweekly /
  monthly digest, with exactly one active period per schedule. Merge strategies:
  **Append** (dated sections) and **Hybrid / Resummarize** (a synthesis pass at
  finalize folds the period into one coherent digest with merged structured
  fields).
- **Manual trigger + run history** — fire a schedule on demand; every fire / skip
  / failure is recorded.
- **Retrospective weekly summaries** — "Summarize by week" walks an imported
  chat's full history and produces one summary per week that has messages
  (skipping empty weeks), each dated to the week it covers — the way to get weekly
  digests out of a historical WhatsApp export.

## Delivery

- **Always-on dashboard** — every summary is stored and visible in the web UI; no
  configuration required.
- **Sink plugins** (open, schema-driven, encrypted config) — webhook, Confluence,
  email (SMTP), Google Drive (as a Google Doc), and **Discord / Slack channel
  send-back** (post the summary into a channel, reusing the workspace's bot
  token).
- **Two-layer plugin model** — a tenant admin **enables** a plugin and configures
  its account credentials once (including a **Connect Google Drive** OAuth flow
  that captures the refresh token instead of pasting it); each workspace then
  picks only the non-secret target (channel / space / folder). A plugin a tenant
  has disabled is refused.
- **Gating** — destinations are gated by the pure delivery policy (enabled +
  configured / connected) before anything is sent.
- **Per-schedule + per-destination rolling control** — a schedule can pin delivery
  to a chosen subset of destinations (ADR-014), and a destination can opt into
  receiving the **in-progress** rolling digest on every run rather than only the
  finalized end-of-period digest (ADR-108).

## Knowledge base

- **Automatic knowledge units** — headlines, key points, and action items are
  extracted from every summary with their provenance.
- **Semantic search** — natural-language search across the workspace's units,
  each hit linking back to its source messages.
- **Wiki synthesis** — regenerate a single topic-organized `knowledge-base` page
  from the units on demand.
- **Rolling-ingest dedup (all 4 layers)** — content-addressed ids collapse exact
  repeats; a semantic near-duplicate gate stops paraphrases; delta-only ingest
  feeds each rolling period incrementally; and **provenance-merge** folds a
  re-stated fact's sources into the existing unit instead of dropping it.
- **AI wiki curator** — an advisory health report flagging duplicate clusters and
  stale units (read-only, so reversible; audit-logged).

## Multi-tenancy, identity & access

- **Tenants → workspaces** — organizations provision tenants and create workspaces
  under them; every stored row is workspace/tenant-scoped (no cross-tenant leak).
- **Host → tenant routing** — a subdomain or custom domain resolves to a tenant.
- **Roles (RBAC)** — Owner / Admin / Member / Guest, with a permission model
  (e.g. `ManageSettings` is Admin+). A platform-operator role exists for
  cross-tenant operations, assigned out-of-band.
- **Sessions** — JWT access tokens + revocable refresh tokens, rotation on
  refresh, logout.
- **Real OAuth login** — Google / Discord authorization-code + PKCE flows
  (feature-gated; needs provider keys).
- **Membership-derived grants** — a login's workspace access is filtered to the
  user's actual entitlement; you cannot mint a token for a workspace you're not in.
- **Members & invitations admin** — list/role/remove members; issue invites (raw
  token shown once) and revoke them.

## Cost controls

- **Per-request cost cap** and **per-tenant rolling-window budgets** (granted by an
  owner), with spend drawn down on each metered call.
- **Bring-your-own LLM** — a tenant can point summarization at its own
  OpenAI-compatible endpoint and/or store an encrypted API key (AES-256-GCM).
- **Spend analytics** — total / recent-window / per-model spend dashboard.

## Dashboard (web UI)

A left-nav SPA with: Create (unified summary wizard), Summaries
(search/filter/pin/archive/tag), Schedules, Knowledge (base + search + curator),
Spend, Jobs, WhatsApp (import + coverage), Discord & Slack sources, Delivery
destinations, Plugins (tenant admin), Members, Audit, and Settings. Live updates
stream over Server-Sent Events.

## Operations

- **Single binary** that also serves the dashboard SPA; SQLite persistence with a
  tracked migration ledger.
- **Observability** — a Prometheus `/metrics` endpoint (DB gauges + HTTP request
  counters) and one structured JSON access log per request carrying a correlation
  id.
- **Deployable** — a multi-stage Dockerfile, `docker-compose.yml`, and `fly.toml`;
  secrets provided at runtime, the database on a mounted volume.
- **Audit ledger** — security/admin events (member changes, invites, curation
  runs, denied grants) recorded and surfaced to admins.

## On the roadmap (not yet shipped)

- Discord/Slack **DM** send and an independent background poller (fetch on its own
  cadence, separate from summary schedules).
- Curator **auto-apply** (merge/prune with undo) and LLM topic re-organization.
- Per-perspective prompt presets; push templates per destination.
- End-to-end runs of OAuth login and the Confluence/Drive/SMTP sends against live
  services (the plumbing is in place; only real credentials are missing).
- Voice-note transcription.

See [Functional areas](functional-areas.md) for how these capabilities are
organized into subsystems, and the [coverage map](../coverage-map.md) for the
authoritative shipped-vs-planned matrix.
