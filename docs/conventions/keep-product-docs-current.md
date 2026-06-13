# Keep the product docs current

`docs/product/` is the comprehensive, product-level documentation —
[mission & vision](../product/mission-and-vision.md),
[capabilities](../product/capabilities.md),
[functional areas](../product/functional-areas.md),
[roles & user stories](../product/roles-and-user-stories.md), and the
[user guide](../product/user-guide.md), tied together by the
[index](../product/README.md). Like the [coverage map](keep-coverage-map-current.md),
it only stays useful if it's updated as the product changes.

## Why

These docs are how a reader understands *what SummaryBot is and how to use it*
without reading the code. Stale product docs are worse than none: they describe a
product that no longer exists, mis-state what a role can do, or walk a user
through a flow that has changed — eroding trust in the docs and the product. The
user has asked that they be kept current as a standing agreement.

## How to apply

When a change lands that affects the product surface, update the relevant doc(s)
**in the same commit/PR** as the change — never as a deferred follow-up:

- **A capability ships, changes, or is dropped** → update
  [capabilities.md](../product/capabilities.md) (present-tense only for what's
  actually built end-to-end; move anything not-yet-shipped to the explicit roadmap
  section). This usually accompanies a [coverage-map](keep-coverage-map-current.md)
  row change — do both.
- **A subsystem, boundary, or the request flow changes** → update
  [functional-areas.md](../product/functional-areas.md).
- **A role, permission, or access rule changes**, or a new user-facing job appears
  → update [roles-and-user-stories.md](../product/roles-and-user-stories.md).
- **A user-facing flow, screen, setting, env var, or run/deploy step changes** →
  update [user-guide.md](../product/user-guide.md) (and verify the steps against
  the running UI per [always-browser-check-ux](always-browser-check-ux.md) when
  it's a `web/` flow).
- **The product's purpose or direction shifts** (rare) → update
  [mission-and-vision.md](../product/mission-and-vision.md).
- Keep the [index](../product/README.md) and the [top-level README](../../README.md)
  in sync if the doc set or quickstart changes.

## Accuracy bar

Same honesty standard as the coverage map: describe a feature in the present tense
only when it **works end-to-end** (not "compiles" or "config exists"); call out
roadmap/planned items explicitly so the shipped-vs-planned line stays clear. Use
the product name **SummaryBot**. When unsure whether something is shipped, check
`docs/coverage-map.md` — it's the authoritative matrix.
