# ADR-126: Optional integration plugins (delivery sinks)

> Rewrite-era ADR. Numbering continues from the reference project's ADR set
> (which ended at ADR-118).

- **Status**: Proposed (2026-06-08)
- **Deciders**: Martin Cleaver
- **Related**: ADR-125 (pluggable & BYO LLM — the feature-gated + encrypted-config
  precedent), PRD §4 (delivery), DEL-001..006 / DEN-* (destination gating),
  ADR-119 (platform-operator role), ADR-099 (Confluence, legacy)

## Context

We want **Google Drive** and **Confluence** integrations in v3, shipped as
**optional plugins** rather than hard dependencies. Both are, for v3, **delivery
sinks** — they *publish* a produced summary outward (Confluence pages, Drive
documents). Neither is an ingestion source in this scope.

The delivery layer already has the right *shape* for this — a runtime-registered
`Deliverer` trait and `DeliveryService.with_deliverers(&[Box<dyn Deliverer>])`
(see `host/src/delivery.rs`, the webhook deliverer DSH-010). But three things
make it not-yet-pluggable:

1. **Closed kind enum.** `domain::DestinationKind` is a fixed set
   (`Dashboard | PlatformChannel | PlatformDm | Email | Webhook`). A plugin
   cannot introduce a new sink kind without editing core, and `resolve_delivery`
   gates on that fixed variant list.
2. **Single-field config.** A destination's config is one encrypted string
   (`workspace_destinations.address_enc`). Confluence needs base-URL + space-key
   + API token; Drive needs a folder id + an OAuth token. One opaque string
   doesn't model multi-field config.
3. **No plugin descriptor/registry.** Nothing lets the dashboard or API discover
   "which integrations exist, what config they require, are they
   enabled/configured for this workspace."

Without resolving these we'd bolt each integration on with a new enum variant +
a bespoke config table + a hardcoded deliverer — the opposite of "plugin," and
all rework later.

## Decision

A small, capability-scoped delivery-plugin seam, built natively and gated by
cargo features. Specifically:

1. **Capability model.** A plugin declares capabilities; v3 ships the **Sink**
   capability only (publish a summary). The trait/descriptor names capabilities
   generally so a future **Source** capability can be added without redesign,
   but Source is explicitly out of scope here.

2. **Mechanism: cargo feature + per-workspace enable.** Each integration is a
   native module behind its own feature (`confluence`, `gdrive`), excluded from
   the default build exactly like `http-llm`. When compiled in, it registers a
   descriptor + a `Deliverer`; a workspace then *enables and configures* it at
   runtime. The default build stays network-free and dependency-light.

3. **Open the destination kind.** Replace the closed `DestinationKind` with a
   kind that carries the built-ins **plus** an open `Plugin(PluginId)` arm (or a
   validated string kind). `resolve_delivery` gates on **capability + enabled +
   configured** flags, not a hard-coded variant list, so adding a sink touches
   no core policy.

4. **Generic encrypted config.** Generalize per-destination storage from a
   single `address_enc` to an **encrypted JSON config blob** keyed by
   `(workspace_id, destination_id)`, reusing the ADR-125 AES-GCM secretbox. The
   plaintext is a small JSON object per plugin schema; the repository still never
   sees plaintext. (The webhook deliverer migrates to a one-field `{ "url": … }`
   blob — no behavior change.)

5. **Plugin descriptor.** `{ id, display_name, capabilities, config_schema }`
   where `config_schema` lists fields with name + `secret: bool`. The API uses it
   to validate submitted config; the dashboard uses it to render the form,
   masking secret fields and showing only a non-secret hint (as webhook does).

6. **Enablement & gating.** Compiled-in plugins are enabled+configured per
   **workspace**, gated through `DeliveryCapabilities`. A **platform operator**
   (ADR-119) may disable a plugin platform-wide (e.g. compliance), overriding
   workspace enablement.

7. **Credentials.** Confluence-sink uses a stored **API token** (no OAuth) →
   buildable immediately. Drive-sink needs a **Google OAuth token** → it depends
   on the (separate, not-yet-built) real-OAuth credential-exchange seam, so
   **sequence Confluence first, Drive after OAuth lands**. Both tokens live in
   the encrypted config blob.

8. **Native, not WASM.** Plugins are native feature-gated modules. WASM
   components (via the existing Wasmtime host) are rejected for now: OAuth +
   outbound HTTP across the WASI boundary is impractical and buys no isolation we
   need here. Revisit if untrusted third-party plugins ever become a goal.

## Consequences

**Positive**
- Minimal core churn: open one enum, widen one column, add a descriptor — the
  `Deliverer` trait, `DeliveryService`, gating policy, and dashboard
  add/test/remove flow are otherwise reused as-is.
- Consistent with ADR-125 (feature-gated, encrypted config, per-tenant/workspace).
- Default build stays lean and offline; integrations are opt-in at compile time.
- New sinks (e.g. Notion, S3) later cost a descriptor + a `Deliverer` impl, no
  core edits.

**Negative / costs**
- Opening `DestinationKind` ripples through `domain/delivery.rs`,
  `host/delivery.rs`, `api/destinations.rs`, and the web Delivery view (kind is
  currently a fixed string in a few places).
- The config migration (string → JSON blob) touches `workspace_destinations` and
  the webhook deliverer (kept behavior-compatible).
- Drive is **blocked on real OAuth**; only Confluence is immediately buildable.

## Alternatives considered

- **Extend the closed enum per integration** (add `Confluence`, `GoogleDrive`
  variants). Simplest, but every integration edits core domain + policy + UI —
  not pluggable; rejected.
- **Bespoke config table per integration.** Matches today's `tenant_llm_config`
  / `workspace_destinations` style, but doesn't scale and duplicates
  encryption/CRUD; rejected in favor of one generic blob store.
- **Runtime-loaded / WASM plugins.** Maximum flexibility/isolation, but large
  machinery (registry, ABI/versioning) or impractical I/O; deferred.

## Implementation phasing

1. **Seam refactor (no new integration yet):** open `DestinationKind` to plugin
   kinds; migrate destination config to an encrypted JSON blob; add the plugin
   descriptor + registry; generalize gating + the API/UI to be schema-driven.
   Webhook moves onto the new shape as the reference plugin.
2. **Confluence sink** behind `--features confluence` (API token; publish/update
   a page per summary).
3. **Real OAuth credential seam** (shared with the dev-login replacement).
4. **Google Drive sink** behind `--features gdrive`, consuming the OAuth seam.
