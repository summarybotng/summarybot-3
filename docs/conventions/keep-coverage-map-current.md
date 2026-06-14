# Keep the coverage map current

`docs/coverage-map.md` is a living at-a-glance comparison of the legacy
summarybot-ng feature set vs. the rewrite. It only stays useful if it's updated
as features land.

## Why

The user relies on it to gauge "how much more work to expect" without re-reading
the codebase. A stale map is worse than none — it misleads the estimate.

## How to apply

When a change lands that moves a feature's status (a seam becomes a real
implementation, a new capability ships, something is dropped from scope):

1. Update the relevant row(s) in `docs/coverage-map.md` (status emoji + notes).
2. Re-check the **Scorecard** and **Remaining work** sections — adjust the
   headline if a whole area changed.
3. Update the **UX reachability** matrix: a feature isn't done when the backend
   works — it's done when a user can reach it in the dashboard. When you ship UI
   for a capability, upgrade its reachability row (🔌/🟡 → ✅); when a capability
   lands **backend-first**, add/keep a 🔌 row so the gap stays visible until the UI
   catches up; if a flow still needs raw ids / server config / a feature flag,
   mark it 🟡, not ✅. (This matrix exists because a feature row read ✅ while the
   UX was 🟡/🔌 — don't let that recur.)
4. Bump the **As of** line to the new date (and commit short-hash if handy).
5. Commit the map update **in the same commit/PR** as the feature, so the map
   never lags the code.

Legend: ✅ done & tested · 🟡 partial · 🔩 seam only · ⛔ not started · ➖ dropped.
Be honest about 🔩 vs ✅ — "compiles" or "config exists" is not ✅; ✅ means the
feature actually works end-to-end (and, per [always-browser-check-ux], was seen
working when it has a UI). The same honesty applies to UX reachability: ✅ there
means a normal user can complete the job in the dashboard without pasting ids,
hitting the API, or rebuilding.
