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
