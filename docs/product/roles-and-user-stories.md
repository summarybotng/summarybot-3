# Roles & user stories

## Roles

SummaryBot's access model is **tenant membership + role**. A user is a member of
a tenant with exactly one role; the role grants permissions, and every data access
is additionally scoped to the workspace (so a role never reaches another tenant's
data). Authentication proves *who you are*; authorization is *your role on this
tenant*.

| Role | Intended for | Can do |
|------|--------------|--------|
| **Owner** | The person/org that provisioned the tenant | Everything an Admin can, plus tenant-level grants: set/clear the LLM budget, transfer/own the tenant. Created automatically for whoever provisions the tenant. |
| **Admin** | Operators who run the workspace day-to-day | Manage settings, members, and invites; enable/configure delivery **Plugins** and connect credentials; manage schedules, sources, and destinations; view the audit log. (`ManageSettings` permission.) |
| **Member** | Regular contributors | Use the workspaces they belong to: create/search summaries, import WhatsApp, run schedules, search the knowledge base, view delivery results. |
| **Guest** | Limited/external collaborators | Scoped read/participation as granted; cannot administer the tenant. |
| **Platform Operator** | The platform's own operators (cross-tenant) | Cross-tenant operations that sit *above* any single tenant. **Assigned out-of-band only — never through the UI** (PRM-008, ADR-119). |

Notes:
- **Membership-derived access.** A session token only grants the workspaces the
  user is actually entitled to (owns or is a member of the tenant for). Requesting
  a workspace you're not in is dropped and audit-logged. Unclaimed/dev workspaces
  stay open for first-run.
- **Isolation is structural.** Tenant isolation is enforced in the storage layer,
  not just checked in handlers (TEN-007).
- **Auditable.** Member/role/invite changes, denied grants, and curation runs are
  written to the audit ledger.

## Personas

- **Community organizer (Riya)** — runs a WhatsApp group; wants a record of
  decisions and a way to fill history gaps nobody captured.
- **Team lead (Marcus)** — runs a Slack/Discord team; wants a daily/weekly digest
  delivered back to a channel and an evergreen knowledge base.
- **Workspace admin (Dana)** — sets up integrations, manages members, controls
  spend, configures where summaries go.
- **Org owner (Sam)** — provisions the tenant, sets budgets, brings the org's own
  LLM provider.
- **Platform operator (Alex)** — runs the SummaryBot deployment for many tenants.

---

## User stories

Grouped by job-to-be-done. Each is a capability that exists today unless marked
*(roadmap)*.

### Getting started & access

- As a new user, I can **sign in** and land in a workspace, so I can start without
  setup. *(dev sign-in today; Google/Discord OAuth when configured)*
- As an org owner, I can **provision a tenant** and automatically become its
  Owner, so I can administer it.
- As an admin, I can **invite people** by email with a role, and the raw invite
  token is shown to me exactly once; I can **revoke** an invite or **change/remove**
  a member's role.
- As a security-conscious owner, I'm assured a member's token **only grants the
  workspaces they belong to**, so a stolen or crafted request can't reach others'
  data.

### Capturing conversation

- As a community organizer, I can **import a WhatsApp export** (zip or text) and
  have phone numbers anonymized automatically, so private data isn't stored raw.
- As a contributor, I can re-upload an export safely — **duplicates are skipped**.
- As an organizer, I can see a **coverage timeline** for each chat with the gaps
  classified (before we joined / between imports / since the last export), so I
  know exactly what history is missing.
- As an organizer, I can **request a specific date range** from members and watch
  the ask **auto-resolve** (credited to whoever filled it) when a covering import
  arrives.
- As a team lead, I can **connect a Discord or Slack bot token** and pull recent
  channel history into the workspace.

### Summarizing

- As a member, I can **summarize a conversation** and get structured output — key
  points, action items, participants — with **citations** linking each claim to
  real messages.
- As a member, I trust the summary because a **coherence score** flags
  weakly-grounded output and the system never silently fabricates.
- As an admin, I can set **per-workspace instructions** (e.g. "emphasize
  decisions") that shape every summary.
- As a member summarizing a huge backlog, the system **map-reduces** it within the
  model's limits rather than truncating.

### Scheduling & digests

- As a team lead, I can create a **daily or weekly schedule** that summarizes a
  channel automatically, and **fetches fresh messages** before each run.
- As a team lead, I get a **rolling weekly digest** that accumulates daily and is
  folded into one coherent summary at week's end (Hybrid merge).
- As an admin, I can **trigger a schedule on demand** and review its **run
  history**.

### Delivery

- As a team lead, I want the digest **posted back into our Slack/Discord channel**,
  not just sitting in a dashboard.
- As an admin, I can also deliver to a **webhook, Confluence, email, or Google
  Drive**.
- As a tenant admin, I **enable a plugin once for the tenant and connect its
  credentials** (including a one-click **Connect Google Drive**), then each
  workspace just picks the **target** (channel / space / folder); a plugin I've
  disabled is refused.
- As any user, I'm assured every summary is **always saved to the dashboard**
  regardless of other destinations.

### Knowledge

- As a member, I can **semantically search** everything we've summarized and jump
  from a hit back to its **source messages**.
- As an admin, I can **(re)generate the knowledge-base wiki page** organized by
  topic.
- As an admin, I can run the **curator** to get an advisory report of duplicate
  clusters and stale units — read-only, so nothing changes without me.
- As the system, re-stated facts **strengthen** an existing knowledge unit's
  provenance instead of piling up duplicates.

### Cost & configuration

- As an owner, I can **bring our own LLM** (an OpenAI-compatible endpoint and/or an
  encrypted API key) so summaries run on our provider.
- As an owner, I can **grant a budget** over a rolling window; metered calls draw
  it down and are refused when it's exhausted.
- As an admin, I can see **spend analytics** — total, recent window, and per-model.

### Operations

- As a platform operator, I can **deploy the single binary** via Docker/compose/Fly,
  with the database on a persistent volume and secrets supplied at runtime.
- As an operator, I can scrape **`/metrics`** (request counts + DB gauges) and read
  **structured access logs** with correlation ids to trace a request.
- As an admin, I can review the **audit log** of security and admin events for my
  tenant.

---

For the click-by-click version of these stories, see the
[User guide](user-guide.md).
