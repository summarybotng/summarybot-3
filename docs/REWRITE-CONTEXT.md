# Rewrite Context & Sources of Truth

> Durable notes for the SummaryBot-NG → Rust/WASM rewrite. Committed to git on purpose:
> the Codespace/devcontainer is disposable, so anything not in this repo can vanish.

## Target

Ground-up rewrite of SummaryBot-NG as **Rust + WASM**, "state of the art", explicitly
**including promoted future requirements**. Driven by `docs/PRD.md`.

## Sources of truth (priority order)

1. **`docs/PRD.md`** — the working spec. Authoritative for *what* to build.
2. **Old-project ADRs** — design *rationale* the PRD only references by number.
   Location: `/workspaces/summarybot-ng-reference/docs/adr/` (~115 files, ADR-001..118).
3. **`/workspaces/summarybot-ng-reference/docs/technical-debt.md`** — failure modes to design away from.
4. **Old implementation** — `/workspaces/summarybot-ng-reference/src/` — **reference only**,
   for proven domain logic & edge cases. Do NOT port structure (god-files: `summaries.py` 5,574 lines,
   `archive.py` 4,051, `wiki.py` 2,277). Re-derive for Rust.

## Reference repo

- Old project: https://github.com/summarybotng/summarybot-ng/
- Cloned to `/workspaces/summarybot-ng-reference` (NOT tracked by git — ephemeral).
- **Auto-restored** on devcontainer rebuild by `.devcontainer/postCreate.sh`.

## Scoping decisions made

- PRD + ADRs + technical-debt.md = sources of truth; old `src/` = reference-only.
- Promoted to committed requirements (from PRD §13): Platform-Agnostic (§2.4 WSP-*),
  Multi-Tenancy (§6.3 TEN-*), Coherence/Advanced RuVector (§8.3 COH-*), AI Wiki Curator (§8.4 CUR-*).
- `guild_id` → `workspace_id` rename is intended throughout (WSP-002).
- WhatsApp collection + coverage-gap awareness **promoted to committed v1 reqs**
  (PRD §2.3 WHA-009..019, ADR-121, superseding ref ADR-081/112). Driven by the
  push-only/no-API reality: ingestion is upload-only, so the system must solicit,
  dedup (synthetic fingerprint — exception to DAT-005), anonymize-on-ingest, and
  surface coverage gaps with scoped import invitations. v1 consent posture is
  anonymize-only (no attestation gate); stricter consent deferred to Phase 9.

## Known gaps / cautions

- The repo has 115 ADR files spanning ADR-001..118; three numbers were never written:
  **ADR-113, ADR-115, ADR-116** (no file, no cross-references, absent from git history —
  skipped numbers, not deletions). They sit in the Confluence/RuVector tail of the log,
  so any content was likely reserved-then-abandoned there. **None of the three are
  referenced by `docs/PRD.md`** — every ADR the PRD cites has a backing document.
- Old repo has a `docs/PRD-rewrite.md` — likely the source our `docs/PRD.md` derived from; worth diffing.

## Synthesis brief

See `docs/reference-brief.md` — consolidated "what the PRD doesn't tell you" output from the
ADR/tech-debt research pass.
