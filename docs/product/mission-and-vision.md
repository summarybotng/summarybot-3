# Mission & Vision

## Mission

**Turn the conversations a team already has into durable, trustworthy knowledge —
without anyone having to take notes.**

Teams make decisions, surface action items, and accumulate hard-won context in
chat: WhatsApp groups, Discord servers, Slack channels. That knowledge is
real but ephemeral — it scrolls away, lives in one person's memory, and is
invisible to anyone who wasn't there. SummaryBot captures it, distills it into
structured summaries that **cite their sources**, and keeps a searchable,
self-organizing knowledge base so the team can answer "what did we decide?" and
"what's still open?" long after the messages have scrolled past.

## Vision

SummaryBot is becoming the **connective memory layer** for a multi-platform,
multi-tenant organization:

- **Platform-agnostic by design.** Conversation is conversation, wherever it
  happens. SummaryBot normalizes WhatsApp, Discord, and Slack into one model and
  treats new platforms as adapters, not rewrites. (This is the central pivot from
  the original product, which was a Discord-only bot — see the
  [reference brief](../reference-brief.md).)
- **Trustworthy, not just plausible.** An AI summary that invents facts is worse
  than no summary. Every claim is grounded in citations to real messages, and a
  coherence gate guards against hallucination. Trust is a hard requirement, not a
  nice-to-have.
- **Knowledge that organizes itself.** Facts extracted from summaries cluster into
  an emergent, regenerable knowledge base — searchable semantically, synthesized
  into a wiki, and curated for staleness and duplication over time.
- **Tenant-native and self-serve.** Organizations run independently and securely
  side by side, administer their own members and integrations, bring their own
  LLM and credentials, and stay within budgets they control.
- **Deliver where people already are.** Summaries flow back to the channel, the
  wiki, the inbox, or the drive — the team shouldn't have to come to a dashboard
  to get value (though the dashboard is always there).

## Principles

These principles are visible in the architecture and the way the product is built:

1. **Grounded over glib.** Citations, a coherence gate, and "best-effort partial,
   flagged degraded" beat confident fabrication. Never silently drop or invent.
2. **Tenant- and workspace-first.** Identity, data, and access are scoped to a
   workspace under a tenant from the ground up; cross-tenant leakage is
   structurally impossible, not merely discouraged.
3. **Least privilege, derived not asserted.** Access follows membership — a
   session is granted only the workspaces a user is actually entitled to.
4. **Persistent, restart-safe state.** "Done" means it survives a restart with
   correct semantics. No critical state lives only in memory.
5. **Bounded cost.** A hard per-request cost cap degrades gracefully rather than
   overspending; per-tenant budgets make platform-key spend measurable and
   capped.
6. **Reversible and auditable.** Destructive or curatorial actions are reversible
   and recorded in an audit ledger.
7. **Privacy by construction.** Phone numbers and other identifiers are
   anonymized at ingest; secrets are encrypted at rest and never echoed back.

## Who it's for

- **Communities & groups** (WhatsApp/Discord) that want a running record of what
  happened and what was decided, and a way to fill in history nobody captured.
- **Teams** (Slack/Discord) that want daily/weekly digests delivered back to a
  channel and an evergreen knowledge base instead of scrollback archaeology.
- **Organizations** that need multiple workspaces administered under one tenant,
  with their own LLM provider, budgets, roles, and delivery integrations.

See [Capabilities](capabilities.md) for what that looks like concretely today.
