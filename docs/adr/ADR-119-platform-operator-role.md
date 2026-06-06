# ADR-119: Platform-operator role

> Rewrite-era ADR. Numbering continues from the reference project's ADR set
> (which ended at ADR-118). Resolves brief open question #12.

- **Status**: Accepted (2026-06-04)
- **Deciders**: Martin Cleaver
- **Related**: WSP-011..014 (identity claim/transfer), PRM-001/PRM-008 (§6.2),
  TEN-007 (§6.3)

## Context

The identity claim/transfer workflow (WSP-011..014) lets a user claim a platform
account that is currently bound to someone else. Self-service auto-approves when
the claimant can complete the platform's own OAuth (WSP-012). Disputes that
can't be self-verified escalate to a human approver (WSP-013):

- **Same-tenant** collisions have an obvious authority — the **tenant admin**.
- **Cross-tenant** collisions do not: a tenant admin has no authority over a
  different tenant, so neither side can fairly adjudicate.

The per-workspace permission levels (PRM-001: `NONE < SUMMARIZE < SCHEDULE <
ADMIN`) are all scoped *within* a workspace/tenant. None can act across tenant
boundaries. We need a role that sits above all tenants for this narrow purpose.

## Decision

Introduce a **platform-operator** role.

1. **System-level and orthogonal.** It is *not* a fifth value in the
   per-workspace permission enum. Workspace `ADMIN` remains the ceiling within a
   tenant; platform-operator is a separate, deployment-wide capability. This
   keeps cross-tenant authority from ever leaking into ordinary workspace roles.

2. **Assigned out-of-band only.** Operators are configured at the deployment
   level (e.g. a `PLATFORM_OPERATOR_IDS` list in config / bootstrap), never
   granted through the in-app UI. Self-host: the instance operator. Hosted: the
   vendor's ops. No one can grant themselves the role from inside the app, which
   removes the "secure first operator" bootstrap problem.

3. **Narrowly scoped (for now).** The role's only power today is approving
   **cross-tenant** identity claims (the cross-tenant path of WSP-011..014).
   Broader superadmin capabilities (impersonation, tenant management) are
   explicitly out of scope and would need their own ADR.

## Consequences

- **Positive**: cross-tenant disputes have a clear, fair adjudicator; the role
  cannot be escalated into from a workspace role; no in-app grant surface to
  secure; one definition serves both self-host and hosted.
- **Negative / trade-offs**: changing who is an operator requires a config
  change + redeploy/restart (acceptable — this is a rare, high-trust role).
- **Audit**: every operator action on a claim is audit-logged (WSP-014).
- **Implementation note**: no code yet — the claim/transfer workflow
  (WSP-011..014) is unbuilt. The role's first code touchpoint is the auth layer
  (Phase 1 part 3): load operator ids from config and gate the cross-tenant
  claim-approval path on membership. Until then this is spec-only.
