# ADR-120: Shared platform sources (many-to-many connections)

> Rewrite-era ADR. Numbering continues from the reference project's ADR set
> (which ended at ADR-118).

- **Status**: Accepted (2026-06-04)
- **Deciders**: Martin Cleaver
- **Related**: WSP-008, WSP-015, TEN-007, ADR-066 (platform-agnostic
  architecture), PRD §7.1 data model
- **Supersedes**: the global `UNIQUE (platform, platform_id)` constraint in the
  original PRD §7 `workspace_connections` schema.

## Context

A `WorkspaceConnection` maps an external platform source — a Discord guild, a
Slack team, a WhatsApp export — to a SummaryBot workspace (WSP-008). It answers
"when content arrives from this source, which workspace owns it?"

The original PRD §7 schema declared `UNIQUE (platform, platform_id)`, meaning a
source could belong to **exactly one** workspace. The Phase 1 repository
implemented that faithfully (and mis-cited it as WSP-010).

That constraint is wrong for a real, established use case from the prior
implementation: **the same Slack team is shared across multiple workspaces**,
and those workspaces can live in **different tenants** (e.g. several
Discord-based communities that share one common Slack). The exclusive binding
makes this impossible.

Note this is a *different* concern from **WSP-010**, which rejects double-binding
of a platform **user identity** (one person → one SummaryBot user). WSP-010 stays
strict; this ADR only relaxes **source connections**.

## Decision

A platform source may connect to **multiple workspaces, including across
tenants**.

1. **No global uniqueness.** Drop `UNIQUE (platform, platform_id)`. Keep only
   per-workspace hygiene: `UNIQUE (workspace_id, platform, platform_id)` (a
   workspace can't attach the same source twice).

2. **Resolution returns a set.** "Which workspace owns this source?" becomes
   "which workspace(s)?" — resolution returns zero or more workspaces.

3. **Full channel overlap.** No uniqueness at channel grain either. If two
   workspaces both subscribe to the same shared channel, **each summarizes it
   independently** (separate summaries, separate cost). Channel selection is a
   per-workspace scope concern (ADR-011, Phase 6), not a global constraint.

4. **Cross-tenant is intended.** A shared source feeding workspaces in different
   tenants is explicitly allowed. This is a **documented exception to tenant
   isolation (TEN-007)**: the same source content lands in each subscribing
   tenant by design. Tenant isolation still holds for everything else — reads of
   workspaces, summaries, etc. remain tenant-scoped.

## Consequences

- **Positive**: supports the shared-Slack reality; aligns with the
  platform-agnostic model (ADR-066) where sources and workspaces are decoupled.
- **Negative / trade-offs**:
  - Duplicate processing/LLM cost when multiple workspaces summarize the same
    channel — accepted; each workspace is an independent consumer.
  - Tenant isolation is no longer absolute for *ingested source content*. This
    must be surfaced to operators (a shared source is a deliberate, visible
    choice), and security review (Phase 9) must treat shared sources as a known,
    audited boundary rather than a leak.
- **Code impact (Phase 1 part 1)**: drop the global UNIQUE constraint; change
  `attach_connection` to reject only same-workspace duplicates (not
  cross-workspace binds); change connection resolution to return a list of
  workspaces. Tests updated accordingly.
