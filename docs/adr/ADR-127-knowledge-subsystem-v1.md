# ADR-127: Knowledge subsystem v1 — semantic search, coherence, wiki

> Rewrite-era ADR. Numbering continues from the reference project's ADR set
> (which ended at ADR-118).

- **Status**: Proposed (2026-06-12)
- **Deciders**: Martin Cleaver
- **Related**: PRD §8 (KNO/WIK/COH/CUR), §12.8 Phase 7; brief open Q#7 (RuVector
  Rust API vs FFI) and Q#8 (lock the embedding model); ADR-004 (grounded
  citations), ADR-095 (map-reduce)

## Context

Phase 7 adds the compounding-knowledge layer the legacy product had: extract
knowledge units from summaries, embed them, search semantically, validate claims
against sources (coherence), and synthesize a wiki. Two brief open questions gate
it: which embedding model (Q#8 — must be locked, since a change invalidates all
stored vectors) and whether RuVector ships as a native crate or needs FFI (Q#7).

## Decision

A pragmatic, fully-testable v1 that delivers the HIGH-priority pieces (semantic
search, coherence gate, provenance) without a hard dependency on an unproven
external vector crate or in-container model weights:

1. **Embeddings via a local self-hosted model over HTTP (Q#8).** Reuse the
   OpenAI-compatible endpoint already used for the LLM (`/v1/embeddings`) — the
   operator's Mac-mini Ollama `nomic-embed-text` (768-dim). An `Embedder` trait
   abstracts it; a **deterministic demo embedder** (hashed n-grams → unit vector)
   keeps the default build + tests network-free. The model id + dimension are
   **pinned and stored with each unit**; a model change re-embeds, never mixes.
   (No candle/weights in the container — the PRD's "local self-hosted model" is
   satisfied by the local Ollama.)

2. **Vector store = SQLite + brute-force cosine, behind a trait (Q#7).** Knowledge
   units live in a `knowledge_units` table with the embedding as an f32 blob;
   semantic search is exact cosine top-k computed in Rust. This is the
   PRD-sanctioned "Rust-native fallback" (KNO-006) and is exact + dependency-free
   at our scale. A `VectorSearch` seam lets RuVector/HNSW drop in later when the
   crate is proven; FTS keyword search is an additive future index.

3. **Knowledge units carry provenance (KNO-001, COH-005).** Each produced summary
   yields units (its key points, action items, and a headline unit), each linked
   to the summary and to the source message ids from the summary's grounded
   citations — so every unit traces back to source segments.

4. **Coherence gate (COH-001, HIGH).** A pure grounding check validates a
   summary's claims against the substantial source messages (lexical overlap
   floor in v1; an LLM-judge is a later, stronger pass). It flags ungrounded
   claims rather than blocking — surfaced as a `coherence` score/flags on the
   summary — so a hallucinated claim is visible, not silently published.

5. **Wiki synthesis (WIK-001..003).** Summaries/units are grouped into emergent
   topic pages (v1: keyword/tag clustering over units), regenerable on demand.
   The AI Wiki Curator (CUR-*) is **deferred** to a later increment.

6. **Ingestion is best-effort and post-delivery.** Knowledge ingestion runs after
   a summary is stored/delivered; an ingestion failure (e.g. embedder down)
   records a status and never fails the summary. Tracked per summary (KNO-004).

## Consequences

**Positive**
- Ships the HIGH items (semantic search, coherence, provenance) end-to-end,
  fully unit-testable via the demo embedder; real embeddings via the Mac mini.
- No risky external crate or multi-hundred-MB weights; default build stays lean.
- The `Embedder` + `VectorSearch` seams make the model and index swappable
  (RuVector/HNSW, FTS5) without touching callers.

**Negative / costs**
- Brute-force cosine is O(n) per query — fine for thousands of units per
  workspace, not millions; HNSW is the eventual answer.
- Lexical coherence is weaker than an LLM-judge (a follow-up).
- Re-embedding on model change is a manual reindex (acceptable; model is pinned).

## Alternatives considered

- **RuVector native crate now.** The intended long-term store, but unproven/
  unavailable here; the PRD explicitly permits a Rust-native fallback. Deferred
  behind the `VectorSearch` seam.
- **In-process candle embeddings.** Pulls large deps + model weights into the
  container for little gain over the already-present local Ollama. Rejected for v1.

## Implementation phasing

1. **Semantic search foundation:** `KnowledgeUnit` + extraction + cosine/top-k
   (domain, pure); `knowledge_units` storage (repository); `Embedder` (demo +
   HTTP) + `KnowledgeService` ingest/search (host); search API + ingestion wired
   into the summary path.
2. **Coherence gate (COH-001):** pure grounding check + a `coherence` signal on
   stored summaries.
3. **Wiki synthesis (WIK-001):** topic pages from units, list/search/regenerate.
4. **Curator (CUR-*):** deferred.
