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
3. Bump the **As of** line to the new commit short-hash (and date).
4. Commit the map update **in the same commit/PR** as the feature, so the map
   never lags the code.

Legend: ✅ done & tested · 🟡 partial · 🔩 seam only · ⛔ not started · ➖ dropped.
Be honest about 🔩 vs ✅ — "compiles" or "config exists" is not ✅; ✅ means the
feature actually works end-to-end (and, per [always-browser-check-ux], was seen
working when it has a UI).
