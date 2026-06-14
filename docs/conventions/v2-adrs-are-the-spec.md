# The v2 ADRs are the product spec

The legacy **summarybot-ng** (v2) architecture-decision records live in this repo
at [`docs/reference/v2-adr/`](../reference/v2-adr/README.md) — committed and
**first-class**. They are the authoritative description of *intended product
behavior*. This rewrite re-implements that product in Rust/WASM; it does not get
to silently redefine the product by omission.

## Why

A real incident: a user asked for weekly summaries of an imported WhatsApp chat.
The behavior was fully specified in v2 (ADR-088/089 unified create-summary wizard
with a **Retrospective / specific-past-dates** mode; ADR-101 rolling periods;
ADR-107 lookback defaults). But because the *Rust code* hadn't built the
retrospective flow, it was treated as "not built — let me guess what you want."
That's backwards: **absence of an implementation is a gap to close against the
spec, not a question to re-litigate with the user.** The spec already answered it.

## How to apply

- **Before building or scoping a feature**, check `docs/reference/v2-adr/` for the
  relevant ADR(s) and design against them. Don't infer intended behavior from the
  current Rust surface — the Rust may be incomplete; the ADR is the intent.
- **Don't ask the user to re-decide what an ADR already decided.** If an ADR
  specifies the flow, implement it (re-derived for Rust). Ask only about genuine
  *deltas* the rewrite introduces, or where ADRs conflict / are silent.
- **Record divergence explicitly.** Where the rewrite intentionally departs from a
  v2 ADR (scope cut, different mechanism), capture it in this repo's own ADRs
  (`docs/adr/`, ADR-119+) and the [coverage map](../coverage-map.md) — never as a
  silent drop. A spec'd-but-unbuilt capability is a `⛔`/`🔌` row, not "out of
  scope," unless an ADR here says it's dropped.
- **Reference, re-derive — don't copy code.** The ADRs (and the
  `/workspaces/summarybot-ng-reference` tree) are reference-only; the rewrite owns
  its own implementation, data model, and tests.

The synthesized starting point is [`docs/reference-brief.md`](../reference-brief.md)
(the reasoning/traps behind the committed requirements); the per-decision detail is
in `docs/reference/v2-adr/`.
