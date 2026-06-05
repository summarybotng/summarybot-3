# ADR-121: WhatsApp collection process & coverage-gap awareness

> Rewrite-era ADR. Numbering continues from the reference project's ADR set
> (which ended at ADR-118; the rewrite has since added 119, 120).

- **Status**: Accepted (2026-06-05)
- **Deciders**: Martin Cleaver
- **Related**: WHA-001..020, DAT-003..006, WSP-010 (no silent identity merge),
  ADR-066 (platform-agnostic architecture), PRD §12.0 (host/WASM split),
  PRD §13.2 / ADR-053 (WhatsApp live fetch BLOCKED — push-only API)
- **Supersedes**: ADR-081 (WhatsApp Import Management) and ADR-112 (WhatsApp
  Coverage Gap Awareness) from the reference project. Their analysis is sound and
  reused; this ADR re-derives the model for the greenfield Rust/WASM build
  (`workspace_id`, not `guild_id`; proper auth, not a static ingest API key).

## Context

Discord and Slack expose live APIs we pull from. **WhatsApp does not** — its
Cloud API is push-only, so historical messages cannot be fetched on demand
(settled in PRD §13.2 / ADR-053; live fetch stays BLOCKED, and third-party
bridges like Baileys are rejected as TOS-violating). The **only** way history
enters the system is a human exporting a chat from their phone (`_chat.txt`,
usually zipped with media) and uploading it.

That single constraint reshapes ingestion. A WhatsApp source is:

- **human-initiated** — nothing arrives unless someone exports and uploads;
- **partial** — an export only covers the period the exporter was in the group
  and the messages still on their device;
- **overlapping** — different members upload overlapping date ranges;
- **snapshot-based** — no incremental sync, no message IDs, no server of record.

So the system's job is not "fetch" but **solicit, validate, merge, and make the
gaps legible** — and tell people exactly what is still worth contributing.

Two further realities from the reference implementation (ADR-081):

- **The contact-name problem.** The same person appears as "Rob Smith" in one
  export, "Robert" in another, "You" in their own, and "+1 555…" when not in the
  contact book. There is no central identity.
- **PII in exports.** Phone numbers appear verbatim and must never be stored or
  shown (WHA-006, Critical; anonymization per the reference ADR-028).

## Decision

### 1. Collection is a first-class, attributed, idempotent pipeline

Each upload becomes a tracked **Import** record (who uploaded, when, original
filename, `file_hash`, size, detected format, status). The pipeline:

1. **Upload + attribution** — bound to a workspace and the uploading user (from
   the Phase 1 identity/session layer — *not* a static `INGEST_API_KEY`).
2. **File-level dedup** — SHA-256 `file_hash`; an identical re-upload is detected
   and surfaced ("already imported on {date} by {user}").
3. **Format detection + parse** — iOS/Android, locale date formats, system
   messages, `<Media omitted>`, voice notes (WHA-003), forwards (WHA-004),
   replies (WHA-005). This is **bounded pure compute → runs in the WASM guest**:
   the host unzips and streams `_chat.txt`, the guest parses to
   `NormalizedMessage`. (§12.0; consistent with the Phase 2 adapter model.)
4. **Anonymize-on-ingest** — phone numbers are HMAC-hashed to stable pseudonyms
   **before** anything is stored; raw PII is never persisted. The HMAC key is a
   secret, so this step is **host-side** (uses the `Secret<T>` wrapper from
   Phase 1). Strengthens WHA-006 from "never expose" to "never store raw."
5. **Identity resolution** — see §3.
6. **Message-level dedup** — see §2.
7. **Coverage update** — see §4.

Imports are **soft-deletable**: the record is hidden but its message
fingerprints are retained so dedup stays correct and a re-upload doesn't
resurrect duplicates. A sanitized message view (pseudonyms only, never
phone/contact names) is available for verification.

### 2. Dedup by synthetic fingerprint — a documented exception to DAT-005

DAT-005 mandates "dedup on platform message ID, never timestamps." **WhatsApp
exports carry no message IDs**, so that rule cannot apply. This is a deliberate,
documented exception (in the same spirit as ADR-120's exception to TEN-007).

The WhatsApp dedup key is a **synthetic fingerprint**:

```
fingerprint = hash(workspace_id, chat_id, timestamp, resolved_identity_id, content_hash)
```

- Keyed on the **resolved identity** (§3), not the raw sender name, so the same
  message from two exports (where the sender is "Rob"/"Robert") collapses to one.
- `content_hash` over the message body guards against same-second collisions.
- Re-running an import produces no duplicates (idempotent, satisfying the
  *intent* of DAT-005 by a different mechanism). The trade-off — two genuinely
  distinct messages with identical (time, author, content) merge — is accepted
  as vanishingly rare and harmless for summarization.

### 3. Identity resolution is confidence-gated and reversible

Cross-export identity matching (phone-hash exact match, then fuzzy alias match
for contact names, plus "You" → uploader resolution) follows the **same
philosophy as WSP-010: no silent merge**. Automatic merges happen only above a
confidence threshold; lower-confidence matches are *suggested*, not applied.
Every merge is **reversible** and **audit-logged** (reusing the Phase 1 audit
ledger), mirroring the reversibility we require of the AI Wiki Curator.

### 4. Coverage-gap awareness is in scope for v1 — including invitations

This is the "show people what we want them to contribute" half, and v1 ships the
**full** path (ADR-112), not just visualization:

- **Join/event detection.** Parse system messages ("created group", "X joined",
  "You were added") to learn when the chat and each member began — so we know the
  chat is older than any export.
- **Gap classification.** Gaps are typed `before_join` / `between_imports` /
  `after_last`, and we distinguish **"no messages existed"** from **"nobody has
  imported this period yet"** (`can_fill`). This is the high-severity confusion
  the reference system had: an empty period read as "no data" when it was really
  "not imported."
- **Coverage timeline.** Per chat, a `█ covered / ░ fillable gap` timeline with a
  coverage percentage.
- **Contributor tracking.** Which member contributed which date range.
- **Scoped import invitations.** Request specific members export and import a
  **needed date range**, with instructions — the active "here's what's still
  missing, please contribute it" loop.

### 5. Auth: workspace-scoped, not a static ingest key

The reference design used an `INGEST_API_KEY` (WHA-008) and `guild_id`. The
greenfield build drops both: ingestion is authenticated through the Phase 1
identity/session layer and scoped to a `workspace_id`. `INGEST_API_KEY` is
removed from the configuration surface. WHA-008 is **superseded** by this.

## Consequences

- **Positive**: a coherent, attributed, idempotent collection model; PII never
  stored raw; users can *see* what's missing and act on it; clean fit with the
  host/WASM split and the Phase 1 identity/audit/secret primitives.
- **Negative / trade-offs**:
  - Identity resolution is inherently fuzzy and adds real complexity; some merges
    need human review.
  - Synthetic-fingerprint dedup can (rarely) merge two truly-identical-looking
    messages — accepted.
  - Coverage/contributor tracking reveals *that* a gap is fillable; we
    deliberately do **not** expose *who* holds the data beyond suggestion hints.

## Recorded open items

- **Third-party consent.** A WhatsApp zip contains other people's messages; the
  uploader is sharing third-party PII. For v1 the decision is **anonymize-on-
  ingest only** (no explicit uploader attestation gate) — accepted as the v1
  posture. A stricter consent ledger (per-participant opt-out / erasure) is a
  candidate for the Phase 9 hardening pass, not a v1 blocker. Flagged here so the
  residual exposure is explicit rather than silent.
- **Media handling.** Whether/where media files in the zip are retained vs
  discarded after voice-note transcription (WHA-003) — to be settled in Phase 2
  detailed design.
- **Upload channels.** Direct upload is v1; Google-Drive-sourced uploads
  (reference ADR-082) are deferred.
