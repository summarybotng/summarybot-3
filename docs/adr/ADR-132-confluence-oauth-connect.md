# ADR-132: Confluence delivery via Atlassian OAuth (Connect wizard)

> Rewrite-era ADR. Numbering continues from the local ADR set (…, ADR-131).

- **Status**: Accepted (2026-06-14) — supersedes the API-token choice for
  Confluence in ADR-126/ADR-099.
- **Deciders**: Martin Cleaver
- **Related**: ADR-126 (optional integration plugins; two-layer model), ADR-131
  (operator plugin veto), the Google Drive OAuth connect flow (`api/src/oauth.rs`)

## Context

ADR-126/ADR-099 deliberately gave Confluence a **pasted API token** (email +
long-lived Atlassian API token, HTTP Basic) "buildable immediately", deferring
OAuth. In practice that's poor UX: the admin must leave the app, create a token
at id.atlassian.com, copy base URL + email + token, and paste secrets. Google
Drive already ships a one-click **Connect** OAuth wizard; Confluence should match.

Atlassian supports OAuth 2.0 3LO (`auth.atlassian.com`), with `offline_access`
for a refresh token and an `accessible-resources` lookup that yields the site's
**cloudId** (Confluence Cloud REST calls go to
`https://api.atlassian.com/ex/confluence/{cloudId}/…`).

## Decision

Add a **Connect Confluence** OAuth flow mirroring Google Drive's, and make the
Confluence deliverer prefer OAuth while keeping the API-token path working.

1. **Connect flow (reuse the existing seam).** `supports_connect("confluence")`
   becomes true. The existing `POST /tenants/:tenant/plugins/:kind/connect` +
   `GET /oauth/connect/callback` already carry `(tenant, kind)` in signed state;
   generalize them to pick the **Atlassian** provider for `confluence`
   (`auth.atlassian.com/authorize`, token `auth.atlassian.com/oauth/token`,
   scopes `write:confluence-content read:confluence-space.summary offline_access`,
   `audience=api.atlassian.com`, `prompt=consent`). Server app creds from
   `ATLASSIAN_CLIENT_ID` / `ATLASSIAN_CLIENT_SECRET`; one fixed redirect URI.
2. **Capture cloudId at callback.** After the code exchange, call
   `GET https://api.atlassian.com/oauth/token/accessible-resources` with the
   access token, take the first site, and store
   `{ refresh_token, cloud_id, site_url }` (encrypted) in the tenant plugin
   config; set `connected = true`, preserving any operator veto (ADR-131).
3. **Deliverer prefers OAuth, falls back to Basic.** At send time the Confluence
   deliverer: if config has `refresh_token` + `cloud_id`, refresh to an access
   token (`crate::oauth::refresh`, as Google Drive does) and POST the page to
   `…/ex/confluence/{cloud_id}/wiki/rest/api/content` with Bearer auth; else use
   the legacy `base_url` + `email` + `api_token` Basic path. Existing API-token
   destinations keep delivering unchanged.
4. **Descriptor.** `space_key` stays the workspace target. The typed tenant
   fields (`base_url`/`email`/`api_token`) remain for the legacy/manual path but
   the dashboard, seeing `supports_connect`, shows the **Connect** button instead.

## Consequences

- **Positive**: one-click Confluence setup matching Google Drive; no pasted
  secrets; tokens are scoped + revocable; the deliverer's OAuth/Basic fork keeps
  every existing destination working.
- **Negative / trade-offs**: needs an Atlassian OAuth app + server client
  id/secret (like Google); a per-send token refresh adds one HTTP round-trip;
  `accessible-resources` assumes the first site (multi-site selection is a later
  refinement). Live 3LO is verifiable only with real Atlassian credentials — the
  provider config + deliverer path-selection are unit-tested.
- **Migration**: none. The column/storage is the existing `config_enc`; legacy
  rows lack `refresh_token` and take the Basic path.
