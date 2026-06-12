# ADR-129: Knowledge dedup under rolling summaries & updates

> Rewrite-era ADR. Numbering continues from the reference project's ADR set
> (which ended at ADR-118).

- **Status**: Proposed (2026-06-12)
- **Deciders**: Martin Cleaver
- **Related**: ADR-127 (knowledge subsystem v1 — units + embeddings + search),
  ADR-101 (rolling-period summaries), ADR-118 (rolling dedup / SUM-010), ADR-087
  (weekly continuity / `human_correction` units); PRD §3.3, §8.1
- **Scope note**: "RVF" here = the **vector dataset** — today the SQLite
  `knowledge_units` store with brute-force cosine (ADR-127 v1); RuVector/HNSW is
  the deferred swap-in behind the same seam. This design is store-agnostic.

## Context

Knowledge units (ADR-127) are extracted from each produced summary, embedded, and
stored for semantic search + wiki synthesis. Today a unit's id is **positional** —
`ku_{summary_id}_{i}` — so every ingest writes fresh rows.

That is fine for one-shot summaries but **breaks under rolling-period summaries**
(ADR-101) and re-synthesis/updates, which re-state the same facts repeatedly:

- A weekly rolling summary **accumulates daily** (Append / Re-summarize / Hybrid).
  Each daily run re-summarizes overlapping content, so "Alice owns the Thursday
  migration" gets extracted again and again.
- Re-summarization **paraphrases** — "Migration scheduled Thursday, owned by
  Alice" — so even exact-string checks miss it.
- **Updates**: a re-run or edited summary should *replace* its old units, not pile
  new ones alongside them; **human markdown corrections** (ADR-087,
  `human_correction`, confidence 1.0) must supersede machine units, not duplicate.

Without dedup the vector dataset bloats with near-identical units, which (a) skews
search (one fact returned five times), (b) inflates wiki synthesis input, and (c)
wastes embedding cost. ADR-118 set the legacy policy (deterministic content IDs +
a 0.95 similarity gate); this ADR adapts it to the ADR-127 store.

## Decision

**Three layers of dedup, cheapest-first**, plus replace-set semantics for updates.

### Layer 1 — Deterministic, content-addressed unit IDs (exact dedup)

Replace the positional id with a content hash that is **independent of which
summary/run produced it**:

```
unit_id = "ku_" + sha256( workspace_id ":" source_key ":" unit_kind ":" normalize(text) )
```

- `source_key` = the channel/source the fact is about (so the same fact in two
  channels stays distinct; cross-channel merge is out of scope).
- `unit_kind` ∈ {headline, key_point, action_item}.
- `normalize(text)` = trim, collapse internal whitespace, NFC, casefold — a
  **stable** normalization (changing it invalidates every stored id; treat the key
  format as load-bearing, per ADR-118).

Store with `INSERT … ON CONFLICT(id) DO UPDATE SET last_seen = …, accumulation_count = accumulation_count + 1` (and refresh provenance — see Layer 4). Re-ingesting the *verbatim* fact from any later run collapses onto the same row. A DB `UNIQUE`/PK on the hash makes this enforceable, not advisory.

### Layer 2 — Semantic near-duplicate gate (fuzzy dedup)

Exact hashing misses paraphrase. Before inserting a *new* (non-hash-matched) unit,
embed it and query the most-similar existing unit for the same
`(workspace, source_key, unit_kind)`:

- Reuse the existing `rank_by_cosine` (ADR-127); today O(units-in-scope), a top-1
  query, trivial at v1 scale and an HNSW top-1 once RuVector lands.
- If `max_cosine ≥ NEAR_DUP_THRESHOLD` (start **0.93**; ADR-118 used 0.95 — tune on
  real data), treat as a duplicate: **don't insert a new vector**. Instead merge
  into the matched unit (Layer 4).
- Else insert as a new unit.

This is the inline CoherenceGate of ADR-118, expressed against our embeddings.

### Layer 3 — Guard before extraction (ingest only the delta)

The rolling state machine (`decide_rolling`, ADR-101) already tracks
`accumulated_through`. Knowledge ingest for a rolling schedule must extract units
**only from the messages newly accumulated** in `(accumulated_through, until]`,
never re-extract the whole rolling summary each run. This removes the bulk of
duplicates at the source; Layers 1–2 catch the residue (overlap windows, catch-up
re-fetch). On **Finalize**, no re-ingest happens — units were ingested
incrementally during accumulation.

### Layer 4 — Provenance merge + replace-set updates

- **Near-dup / exact-dup hit:** keep one unit; *append* the new run's
  `source_message_ids` + summary id to its provenance and bump `last_seen` /
  `accumulation_count`. Search ranking can then favor recently-corroborated units
  without growing the vector count.
- **Confidence wins ties:** a `human_correction` unit (confidence 1.0, ADR-087)
  supersedes a machine unit it matches — replace text/embedding, keep provenance.
- **Re-run / edited summary (replace-set):** units carry their originating
  `summary_id`. Re-ingesting a given `summary_id` first deletes that summary's
  prior units, then re-inserts through Layers 1–2. Combined with content-hash
  upsert across *different* summaries, this means: edits don't orphan stale units,
  and identical facts across summaries still collapse.

## Schema (additions to `knowledge_units`, ADR-127)

- `id` becomes the **content hash** (Layer 1) — PK already; drop the positional
  scheme. (Migration: this invalidates existing ids — acceptable at v1 since the
  store is demo data; a real migration would re-key by recomputing hashes.)
- add `unit_kind TEXT`, `source_key TEXT`, `confidence REAL DEFAULT 0.5`,
  `last_seen INTEGER`, `accumulation_count INTEGER DEFAULT 1`.
- provenance (`source_message_ids`) already exists; it becomes append-on-merge.

## Consequences

- **Idempotent ingest**: re-ingesting the same fact (verbatim *or* paraphrased) N
  times yields exactly one unit — the load-bearing invariant; unit-testable with
  the deterministic `DemoEmbedder` (no network).
- **Search quality**: one fact, one hit, ranked by recency-of-corroboration.
- **Cost**: one embedding per *candidate* (not per occurrence); the similarity
  gate is a top-1 cosine query.
- **Coupling**: ties knowledge ingest to the rolling state machine (Layer 3) — so
  this lands *with* the ADR-101 wiring, not before it. Until then, the v1 one-shot
  path keeps the positional scheme; Layer 1 is the first step and can ship
  independently (it helps even one-shot re-runs).
- **Threshold risk**: too high → dupes leak; too low → distinct facts merge. Make
  `NEAR_DUP_THRESHOLD` configurable and log merges so it's tunable on real data.
- **RuVector swap (ADR-127)**: store-agnostic — exact dedup is a DB constraint,
  fuzzy dedup is a top-1 vector query; both carry over, the latter just gets faster.

## Sequencing

1. **Layer 1** (content-hash ids + upsert) — independent, helps re-runs today.
2. **Layer 2** (similarity gate) — reuses existing embeddings/cosine.
3. **Layers 3–4** land **with the ADR-101 rolling wiring** (they need
   `accumulated_through` and the replace-set hook).
