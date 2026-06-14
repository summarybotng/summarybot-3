# User guide

A practical walkthrough of SummaryBot, from signing in to delivering digests and
administering a tenant. The dashboard is a left-nav web app served by the API; the
same actions are available over the REST API (`GET /openapi.json` lists every
route).

> **Orientation.** A **tenant** is your organization; it holds **workspaces**; a
> workspace draws from **sources** (a WhatsApp chat, a Discord/Slack server). You
> work inside a workspace; tenant-wide setup (members, plugins) lives under the
> tenant.

## 1. Running SummaryBot

### Locally (development)
- Start the API: `API_BIND=127.0.0.1:8080 SECRET_KEY=dev-secret DATABASE_URL=:memory: cargo run -p api`
  (add `--features http-llm,oauth,discord,slack,email,confluence,gdrive` to enable
  real LLM, OAuth, and the integration sinks).
- Start the dashboard: `cd web && npm run dev`, then open the printed URL
  (proxies the API, so you share an origin).
- Or run `scripts/demo.sh` for a no-secrets, in-memory end-to-end walk of the API.

### Production (Docker)
```bash
SECRET_KEY=$(openssl rand -hex 32) \
LLM_CONFIG_KEY=$(openssl rand -hex 32) \
OPENROUTER_API_KEY=sk-or-...           \
docker compose up --build
```
The SQLite database persists in a named volume; `SECRET_KEY` is required and never
baked into the image. `fly.toml` is included for Fly.io. See the repo's
`Dockerfile` / `docker-compose.yml` for all env vars.

## 2. Signing in

On the login screen, enter a **workspace name** and choose **Dev sign-in** (email
provider) — this is the development path. With the `oauth` feature and provider
keys configured, **Sign in with Google/Discord** runs the real authorization-code
flow.

Your session only grants workspaces you're **entitled** to (owned or via tenant
membership). A brand-new/unclaimed workspace name is open so first-run works.

## 3. The dashboard at a glance

Left nav, grouped:

- **(top)** Summaries · Schedules · Knowledge · Spend
- **Sources** WhatsApp · Discord · Slack
- **Admin** Delivery · Plugins · Members · Audit · Settings

Updates stream live (SSE), so new summaries appear without refreshing.

## 4. Importing a WhatsApp chat

1. In WhatsApp, open the chat → ⋮ / contact name → **Export chat** → *Without
   media*.
2. **WhatsApp** tab → choose the `.zip` (or `_chat.txt`), enter a **channel id**
   (e.g. `family-group`), confirm the **timezone** (auto-detected from your
   browser), and **Import**. Re-uploading is safe — duplicates are skipped, phone
   numbers are anonymized.
3. Below the import you'll see **Coverage & history gaps**: a covered-vs-gaps
   timeline, coverage %, and per-gap **Contributors**. For each fillable gap you
   can **Copy ask** (a ready-to-send message) or **Request export** (a tracked
   invitation that auto-resolves when a covering import arrives).
4. **Weekly digests of the history:** click **Summarize by week** on the chat's
   coverage card. It walks the imported history and produces one summary per week
   that has messages (empty weeks are skipped), each dated to the week it covers —
   they appear on the **Summaries** tab. (For an *ongoing* weekly digest of a chat
   you keep re-importing, set up a rolling weekly schedule on the Schedules tab
   scoped to the chat's channel id.)

## 5. Connecting Discord / Slack

1. **Discord** (or **Slack**) tab → paste the **bot token** (stored encrypted).
2. For Discord, **Load servers** and pick your server from the dropdown — no need
   to find the guild id (you can still type one as a fallback). Slack needs no
   server pick (its token is workspace-scoped).
3. **Browse channels** → **Load channels** lists the server's channels (Discord
   grouped by **category**); tick the ones you want — your selection fills the
   sync list. (You can still type channel ids manually instead.)
4. **Sync** pulls recent history for the selected channels (or all of them if none
   selected) into the workspace. The bot must be a member of the channels.
5. You can then **summarize** any synced channel directly from the tab.

## 6. Creating a summary

The quickest path is the **Create** tab — one wizard for every kind of summary:

1. **What** — pick a platform (WhatsApp / Discord / Slack), then a chat/channel
   (or **All channels in this workspace**). Discord/Slack let you pick a server
   first; for Discord you can also scope to **all channels in a category** (the
   category's channels are resolved fresh each run).
2. **When** — choose **Now** (a recent-window summary, with 4h/24h/7d/30d presets),
   **Recurring** (create a schedule, optionally a rolling digest), or **Past** (a
   by-week retrospective across a WhatsApp chat's imported history).

The older entry points still work too: **Summarize now** on the WhatsApp/Source
tab, or the **New summary** paste box on the **Summaries** tab.

Every summary is **structured** (key points, action items, technical terms,
participants) with **citations**, and is saved to the dashboard automatically. Use
the Summaries tab to **search/filter, pin, archive, and tag**.

## 7. Scheduling digests

1. **Schedules** tab → create a schedule: pick **daily / weekly / monthly / hourly
   / custom**, the time + timezone, and (optionally) bind a **source channel** so
   fresh messages are fetched before each run.
2. For a **rolling** digest, choose a rolling period (weekly/biweekly/monthly) and
   a merge **strategy**: *Append* (dated sections) or *Hybrid* (a synthesized,
   coherent end-of-period digest).
3. **Deliver to** — by default a schedule delivers to *all* of the workspace's
   enabled destinations. To pin it to a subset, click the destination chips in the
   New-schedule form; the list shows "→ N destinations" (or "→ all destinations").
4. **Pause/Resume**, **Run now**, and review **run history** from the same tab.

## 8. Delivering summaries elsewhere

Delivery is **two-layer**: a tenant admin sets up credentials once on **Plugins**;
each workspace picks the target on **Delivery**.

**As a tenant admin (Plugins tab):**
1. Enter your **tenant id**. The **Settings** tab shows a **"Your tenants"** picker
   listing the tenants you belong to (with your role) so you can pick one instead of
   typing its id; type an id there to join or create a new tenant (you become Owner).
2. For each plugin (Confluence, Email, Google Drive, Discord, Slack, Webhook):
   **Enable** it and fill its **account credentials** (Confluence base/creds, SMTP
   creds…). For **Google Drive**, click **Connect Google Drive** to authorize via
   OAuth — the refresh token is captured for you (no pasting).

**As a workspace user (Delivery tab):**
3. Pick a **Type** (only the per-workspace **target** fields appear — channel id,
   Confluence space key, Drive folder, recipient, webhook URL) and **Add**.
4. **Test** sends a sample so you can confirm it works; **Remove** deletes it.

Every summary still always lands on the dashboard regardless of destinations.
Note: Discord/Slack send-back reuses the bot token you set on the source tab.

## 9. Using the knowledge base

**Knowledge** tab:
- **Knowledge base** — **Generate / Regenerate** the topic-organized wiki page from
  everything summarized so far.
- **Curator** — **Curate** produces an advisory health report: duplicate clusters
  and stale units. It's read-only (nothing changes), and the run is audit-logged.
- **Knowledge search** — natural-language search across all units; each hit shows
  its kind and links back to source messages.

## 10. Cost & provider settings

**Settings** tab (tenant admin): point summarization at your **own
OpenAI-compatible endpoint** and/or store an encrypted **API key** (BYO LLM), and
pick the model. An **Owner** can grant a **budget** over a rolling window — metered
calls draw it down and are refused when exhausted. **Spend** tab shows total /
recent-window / per-model cost.

## 11. Administering a tenant

- **Settings** → **provision a tenant** (you become its Owner).
- **Members** → invite people (the raw invite token shows once — copy it), set
  roles (Owner/Admin/Member/Guest), or remove members.
- **Audit** → review security/admin events for your tenant (newest first; Admin+).
- Host-based routing: a subdomain or custom domain can resolve to your tenant.

## 12. Operating the deployment (operators)

- **Health:** `GET /healthz`. **Metrics:** `GET /metrics` (Prometheus — DB gauges +
  HTTP request counts/latency). **Logs:** one structured JSON line per request with
  a correlation id (also returned as the `x-correlation-id` response header).
- **Data:** SQLite at `DATABASE_URL` (a `/data` volume in the container); schema
  migrations apply automatically on startup via a tracked ledger.
- **Secrets:** `SECRET_KEY` (required, token signing), `LLM_CONFIG_KEY` (enables
  encrypted BYO-key / credential storage), provider keys (`OPENROUTER_API_KEY` or
  `LLM_BASE_URL`, `GOOGLE_CLIENT_ID/SECRET`, …).

## Troubleshooting

- **"server has no encryption key configured"** — set `LLM_CONFIG_KEY` (64 hex
  chars) to store delivery/tenant credentials.
- **A delivery test fails with "no bot token"** — set the Discord/Slack bot token
  on its source tab first; channel send-back reuses it.
- **"the '<kind>' plugin is disabled for your tenant"** — enable it on the Plugins
  tab.
- **Google Drive "no GOOGLE_CLIENT_ID configured"** — the server needs the
  operator's Google OAuth app credentials for the Connect flow.
- **A workspace shows no data after login** — confirm your membership; tokens only
  grant entitled workspaces.

---

See [Capabilities](capabilities.md) for the full feature list and
[Roles & user stories](roles-and-user-stories.md) for who can do what.
