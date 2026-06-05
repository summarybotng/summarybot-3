# ADR-122: Platform-agnostic command & interaction surface

> Rewrite-era ADR. Numbering continues from the reference project's ADR set
> (which ended at ADR-118; the rewrite has added 119–121).

- **Status**: Accepted (2026-06-05)
- **Deciders**: Martin Cleaver
- **Related**: DIS-002 (slash-command registration), SCP-001..006 / ADR-011
  (scope types), SCH-\*/SCM-\* (scheduling), ADR-013 (job tracking), PRM-005
  (command-level checks), AUD-001 (command logging), WSP-006 (no platform
  hardcoded in core), §4 Delivery (the platform-agnostic *output* counterpart)
- **New requirements**: PRD §2.5 ODS-001..005, CMD-001..006

## Context

The reference project exposed a rich **in-chat** surface (`src/discord_bot/
commands.py`): a `/summarize` command (channel | category, time range, mode,
perspective, length) and a `/schedule` command group (create / list / pause /
resume / delete / status). This is the most direct way an end user interacts
with the product — they type a command *in the channel they're reading* and get
a summary back.

The rewrite's PRD documented the **building blocks** — slash-command
registration (DIS-002), scope types (SCP-\*), delivery formats (DIS-006..009) —
and a full **web dashboard** (§5), but never documented the in-chat surface
itself: which commands exist, what on-demand summarization is, or the
interaction UX (ephemeral responses, deferred long-running replies, scope
pickers). It also fell outside the phase roadmap. The pivot to "platform-
agnostic + dashboard-first" quietly dropped a core user-facing feature.

Two things need deciding: (1) **how** to model the command surface without
re-hardcoding Discord into the core, and (2) **whether on-demand** ("summarize
now") is a first-class requirement separate from scheduling.

## Decision

### 1. The command surface is platform-agnostic, rendered per-platform

Commands are defined **once, abstractly, in core** (the capability: "summarize
this scope over this range", "manage these schedules"). Each platform adapter
**renders** them natively — Discord as application slash commands, Slack as
slash commands / shortcuts — exactly as §4 delivery is defined agnostically and
rendered per-platform. No platform-specific command logic leaks into core
(WSP-006). This keeps the surface uniform and lets a new platform add a command
renderer without touching summarization logic.

### 2. On-demand summarization is first-class — but secondary in priority

"Summarize now" is a distinct capability from scheduling (§3 is the *automated*
sibling; this is the *manual* one) and is promoted to its own requirements
(ODS-\*). It shares **one service path** with the dashboard and with scheduled
runs — request → summarization pipeline (Phase 3) → delivery (Phase 4) — so it is
not a parallel implementation, just a different trigger.

Priority is **Medium**: dashboard-first remains the product priority, so the
in-chat on-demand surface is valued but not Critical. The one exception is
**CMD-001** (agnostic rendering), which is **High** — it is an architectural
constraint, not a feature, and getting it wrong is expensive to undo.

### 3. Long-running requests are async + tracked

A summary can take seconds to minutes. On-demand requests acknowledge
immediately (platform "defer"/typing affordance) and deliver when ready, tracked
as a job (ADR-013) so the same lifecycle, cost cap and error handling apply as
for scheduled runs.

### 4. Graceful degradation where there is no command channel

Platforms with no live command channel — WhatsApp is push-only (§13.2 /
ADR-053) — simply expose **no** commands. The agnostic model treats "has a
command surface" as a per-adapter capability; absence degrades gracefully rather
than erroring. (WhatsApp users act through the dashboard and the upload/coverage
flow of ADR-121 instead.)

### 5. Responses private-by-default; checked and logged

Where the platform supports it (e.g. Discord ephemeral), command responses are
private by default with an explicit opt-in to post publicly — summaries can
contain sensitive discussion. Every invocation is permission-checked (PRM-005)
and audit-logged (AUD-001).

## Consequences

- **Positive**: the core user-facing interaction is now specified; uniform
  surface across platforms; on-demand reuses the scheduled pipeline (no
  duplication); clean fit with the agnostic delivery model and the optional-
  connection model (a workspace may have no bot, WSP-007).
- **Negative / trade-offs**:
  - An agnostic command abstraction is more up-front design than wiring Discord
    slash commands directly; justified by WSP-006.
  - Per-platform feature parity is imperfect (autocomplete/pickers exist on some
    platforms, not others) — handled as graceful fallback (CMD-004), not blocked.
- **Roadmap**: on-demand summarization + command rendering land in **Phase 4**
  (first point where the pipeline + delivery exist); schedule-management commands
  ride **Phase 6** alongside the scheduler. Command rendering depends on the
  Phase 2 platform adapters.
