# ADR-130: Wiring rolling-period summaries end-to-end

> Rewrite-era ADR. Numbering continues from the reference project's ADR set.

- **Status**: Accepted (2026-06-12)
- **Deciders**: Martin Cleaver
- **Related**: ADR-101 (rolling-period policy — the pure state machine), ADR-129
  (rolling-ingest dedup, planned), ADR-108 (per-destination rolling delivery),
  ADR-128 (live source fetch on scheduled runs); PRD §3.3 (ROL-001..006)

## Context

`domain/rolling.rs` shipped the pure policy (`RollingPeriod::window`,
`decide_rolling` → StartNew / Accumulate / Finalize) but nothing drove it: the
scheduler produced an independent summary each run. This ADR records the
decisions made wiring it into storage, the runner, the API, and the UI.

## Decisions

1. **Two side tables, not new `Schedule` fields.** Rolling config
   (`rolling_schedules`: period/strategy/end_day) and active state
   (`rolling_summaries`) live in their own tables keyed by `schedule_id`, so the
   `Schedule` domain struct and its 12-arg `build()` were left untouched (same
   approach as `schedule_sources`, ADR-128).

2. **The active-state PK *is* the one-active-per-schedule invariant.**
   `rolling_summaries.schedule_id` is the primary key — at most one in-flight
   accumulator per schedule. **Finalize deletes the row** (so the next run sees
   `None` → StartNew) and emits a normal `summary_records` row, so finalized
   history lives with every other summary (dashboard, search, spend) rather than
   in a parallel store.

3. **Accumulate the delta, fold the tail on finalize.** Each run summarizes only
   `(accumulated_through, now]` (Brief length) and appends it; this is also where
   the live-source fetch (ADR-128) runs, so weeklies pull fresh messages. On
   Finalize the runner first folds `(accumulated_through, period_end]` (the days
   since the last run) before publishing — no tail is lost.

4. **v1 merge is Append; the finalized digest is a text record.** `append` writes
   dated `## YYYY-MM-DD` sections (text + key points + action items) into a running
   markdown document. The finalized `SummaryRecord` carries that document in
   `summary.text` with a `rolling-<period>` tag. `resummarize`/`hybrid` are
   accepted in config but **alias Append for now** — a structured re-synthesis
   (proper merged key_points) and dedup (ADR-129) are the refinement.

5. **Empty windows contribute nothing.** A delta with no substantial messages adds
   no section; a period that accumulated nothing finalizes without publishing (no
   empty digests). Best-effort live sync failures never fail the run.

## Consequences

- Weekly/biweekly/monthly digests now work end-to-end: configure period+strategy
  on the schedule; each tick accumulates; the digest publishes when the period
  ends and delivers through the normal fan-out (dashboard + destinations).
- Unit-tested deterministically (StartNew → Accumulate → Finalize → exactly one
  digest) with the fake LLM + a fixed clock; no network.
- Refinements tracked: Hybrid merge (currently Append), rolling-ingest dedup
  (ADR-129), per-destination rolling delivery control (ADR-108), and a markdown
  renderer for the digest in the Summaries view (today it shows as text).
- The manual trigger takes an optional `?as_of=<unix_secs>` (backfill/testing) so
  a rolling period can be driven past its end to finalize on demand — used to
  demonstrate the full StartNew→Accumulate→Finalize cycle live without waiting for
  a real period boundary. (Minor: the trigger's `produced` flag still keys off
  `sum_{id}_{now}`, so it reads `false` on a rolling finalize even though the
  digest — id `sum_{id}_{period_end}` — is stored, delivered, and visible.)
