# ADR-131: Platform-operator per-tenant plugin disable

> Rewrite-era ADR. Numbering continues from the local ADR set (…, ADR-130).

- **Status**: Accepted (2026-06-14)
- **Deciders**: Martin Cleaver
- **Related**: ADR-119 (platform-operator role), ADR-126 (optional integration
  plugins; two-layer enablement), TEN-007 (tenant isolation), AUD-001 (audit)

## Context

ADR-126 gives each tenant a two-layer plugin model: a tenant admin **enables** a
delivery plugin and configures its account credentials, then workspaces add
destinations that pick only the target. At delivery time a plugin a tenant has
**not** touched is allowed (default-on); an explicit tenant row with
`enabled = false` is the hard gate.

ADR-126 already anticipated that "a **platform operator** (ADR-119) may disable a
plugin … overriding workspace enablement" — e.g. a compliance hold, an abusive
integration, or a deprecated sink — but ADR-119 deliberately scoped the operator
role to cross-tenant identity-claim approval only and said broader powers "would
need their own ADR." This is that ADR.

The requirement: **the platform operator can disable a specific plugin for a
specific tenant, and every plugin defaults to ON.** A tenant admin must not be
able to re-enable what the operator has disabled.

## Decision

1. **A distinct operator veto, not the tenant toggle.** Add an
   `operator_disabled` flag to `tenant_plugins`, separate from the tenant's
   `enabled`. The two are orthogonal (mirroring ADR-119's "orthogonal role"):
   - `operator_disabled = true` → the plugin is **off for that tenant**, full
     stop, regardless of the tenant's `enabled`. The tenant admin sees it as a
     read-only "disabled by operator" state and cannot override it.
   - `operator_disabled = false` (the default, and any tenant with no row) →
     existing ADR-126 behavior (tenant enablement / default-on).

   Delivery gating (`apply_tenant_layer`) checks the operator veto **before** the
   tenant `enabled` check, so the operator decision always wins.

2. **Default ON.** Absence is permission. A fresh tenant, or any plugin the
   operator has not explicitly disabled, is available (subject to the existing
   tenant layer). The operator acts only to *remove* a capability.

3. **Operator identity stays config-based (ADR-119).** Operators are the ids in
   `PLATFORM_OPERATOR_IDS` (comma-separated), loaded at startup — never granted
   in-app. The operator endpoints are gated on that membership, not on any
   workspace/tenant role, so cross-tenant authority never leaks into tenant roles.

4. **Operator endpoints, separate namespace.** Under `/operator/…`, authorized
   only for configured operators (else 404/403, indistinguishable from "not an
   operator"):
   - `GET  /operator/status` → `{ is_operator }` (lets the dashboard reveal the
     operator controls).
   - `GET  /operator/tenants/:tenant/plugins` → each plugin's `operator_disabled`
     for that tenant.
   - `PUT  /operator/tenants/:tenant/plugins/:kind` `{ disabled }` →
     read-modify-write the row, preserving the tenant's `enabled`/config/connected.
   The tenant-facing `GET /tenants/:tenant/plugins` also returns
   `operator_disabled` so a tenant admin sees (read-only) why a plugin is off.

5. **Audit.** Every operator disable/enable is audit-logged (AUD-001) under the
   operator's id with the tenant + kind, so the action is attributable.

## Consequences

- **Positive**: a clear compliance/kill switch that a tenant cannot circumvent;
  default-on keeps the common path frictionless; reuses the config-based operator
  identity (no new grant surface to secure); one extra boolean, no new table.
- **Negative / trade-offs**: operator changes require knowing the tenant id (no
  cross-tenant *listing* of tenants yet — out of scope, like ADR-119's stance);
  the operator UI is minimal (per-tenant, in the existing Plugins view) rather
  than a full operator console.
- **Backward-compatible**: the column defaults to 0; existing deployments with no
  `PLATFORM_OPERATOR_IDS` have no operators and behave exactly as before.
