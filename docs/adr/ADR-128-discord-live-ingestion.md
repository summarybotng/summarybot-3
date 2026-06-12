# ADR-128: Discord live ingestion — fetch into the message store

> Rewrite-era ADR. Numbering continues from the reference project's ADR set
> (which ended at ADR-118).

- **Status**: Accepted (2026-06-12)
- **Deciders**: Martin Cleaver
- **Related**: ADR-051 (`PlatformFetcher` seam), ADR-121 (WhatsApp collection —
  the parallel ingest path), ADR-120 (shared platform sources), ADR-125
  (per-tenant LLM + encrypted BYO secrets), ADR-126 (sink plugins); PRD §12.3
  (WSP-006), §13.2 (live vs. push platforms)

## Context

The `PlatformFetcher` trait (ADR-051) defined the shape of a live platform source
but shipped only a test fake — no real adapter, and no production caller. Discord
is the first live source to wire up. Two questions: **how does fetched data reach
the summarizer**, and **where does the bot token live**.

The summarization spine already reads messages from the **message store**
(`WhatsAppRepository::list_messages(ws, channel, [start,end])`) — that is what the
scheduler and "summarize channel now" both consume. WhatsApp ingest (ADR-121)
*populates* that store; it is not a `PlatformFetcher`. So a live source has a
choice: fetch-then-summarize inline (a new bespoke path), or fetch-then-persist
(reuse the existing path).

## Decision

**Discord fetch persists into the message store, exactly parallel to WhatsApp
ingest.** A live sync writes normalized messages via `save_message` (idempotent on
the native message id — no dedup churn on re-sync), and everything downstream
(scheduled digests, on-demand channel summaries, knowledge ingestion) works
unchanged. We add *ingestion*, not a second summarization path.

1. **Transport: blocking `ureq` against Discord REST v10, behind a `discord`
   Cargo feature.** Mirrors `HttpLlmClient`/the sink plugins (ADR-125/126): the
   default build stays network-free; the host stays synchronous (the async move
   is still deferred, ADR-051). No `serenity`/`twilight` SDK — the REST surface we
   need (list guild channels, paginate channel messages, fetch guild/channel
   names) is small and a heavy gateway client would pull in an async runtime.

2. **Bot token stored encrypted per `(workspace, platform)`** in a new
   `platform_credentials` table, AES-256-GCM under the operator master key —
   identical handling to the LLM BYO key (ADR-125 2b) and delivery configs
   (ADR-126). The token is a workspace-scoped secret, never returned by the API.
   The `workspace_connections` row (ADR-120) still records *which* guild is
   attached; the credential table holds the *secret* to reach it.

3. **Pure normalization, unit-tested offline.** Snowflake→unix-seconds, the
   Discord message JSON → `NormalizedMessage` mapping, and the text-channel filter
   are plain functions compiled in every build and tested without a network. Only
   the `DiscordFetcher` (the `ureq` calls) is feature-gated. This keeps the
   risky-to-verify network layer thin and the parsing logic provable.

4. **Sync is a workspace-scoped action** (`POST …/connections/discord/sync` with a
   guild id, optional channel filter, and lookback window), not a background
   poller. v1 is pull-on-demand; a polling scheduler is a later refinement once
   the async runtime lands.

## Consequences

- Re-syncing a window is safe and cheap (idempotent by message id); overlapping
  windows converge instead of duplicating.
- Live verification needs a real bot token + guild; the pure layer and the
  token-storage/route wiring are fully verifiable offline. CI stays green without
  Discord credentials.
- Attachments map to `AttachmentKind` by Discord content-type; rich embeds are
  flattened to their text/description for now (a refinement if needed).
- Rate limits: v1 respects Discord's per-route 429 `retry-after` with a bounded
  wait; aggressive backfill of very large channels is out of scope for v1.

## Update — Slack (same session)

The design generalized cleanly to **Slack** as the second live source. A
`SlackFetcher` (Slack Web API: `conversations.list` / `conversations.history`,
bot token `xoxb-…`) mirrors `DiscordFetcher`; the pure layer (Slack `ts` →
unix-seconds, message-JSON → `NormalizedMessage`, system-subtype filter) is
unit-tested offline. The connection API is now **platform-generic**
(`/workspaces/:ws/connections/:platform[/token|/sync]`), dispatching to a
`make_platform_fetcher(platform, token, scope_id)` factory whose arms are
feature-gated (`discord`, `slack`); `status` reports `supported` so the UI knows
whether a build can fetch. The web side is one parameterized `Source` component
behind Discord and Slack tabs. Differences captured: Slack tokens are
workspace-scoped (no guild/`scope_id`), history needs the bot to be a channel
member (`not_in_channel` surfaced per-channel), and Slack reports API errors in
the JSON body (`ok:false`) rather than the HTTP status. Verified the same way —
a bogus token reaches Slack and returns `invalid_auth`.

## Update — scheduled live sync (same session)

Manual sync was the first step; a schedule can now **pull fresh messages before
it summarizes**, so live sources update without a manual click. The fetch+persist
loop is extracted into `host::sync_into_store(fetcher, repo, ws, scope, …)` —
unit-tested with a fake fetcher and reused by both the on-demand connection sync
and the scheduler. A schedule's optional source lives in a separate
`schedule_sources` table (`schedule_id → platform, source_id`), so the `Schedule`
domain struct and its build/storage path were untouched. The schedule runner, if
a source is bound, decrypts the platform token, builds the fetcher via the
factory, and syncs the scheduled channel's window before reading messages —
**best-effort**: a missing token, an uncompiled platform feature, or a network
failure logs and falls back to whatever is already stored, so a scheduled summary
never fails on sync. Exposed through the schedule API (`platform`/`source_id`)
and a "fetch from Discord/Slack" control on the Schedules form. A background
*poller* (sync on a cadence independent of summary schedules) remains future
work; this ties live refresh to the existing schedule tick.
