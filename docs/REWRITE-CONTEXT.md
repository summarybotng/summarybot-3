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

## Known gaps / cautions

- The PRD references ADR numbers up to 118; the repo has ~115 ADR files but some referenced
  ADRs may have no document (verify before assuming a spec exists).
- Old repo has a `docs/PRD-rewrite.md` — likely the source our `docs/PRD.md` derived from; worth diffing.

## Synthesis brief

See `docs/reference-brief.md` — consolidated "what the PRD doesn't tell you" output from the
ADR/tech-debt research pass.
