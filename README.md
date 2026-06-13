# SummaryBot

Turn the conversations a team already has — WhatsApp, Discord, Slack — into
trustworthy, searchable knowledge: grounded summaries, rolling digests, and an
emergent knowledge base, delivered where your team already works.

SummaryBot is a **multi-tenant, multi-platform** conversation-summarization
platform: a Rust/WASM core (pure `domain` policy → `repository` storage → `host`
orchestration → thin `api`) serving a React dashboard, with the summarize step
sandboxed as a `wasm32-wasip2` component.

## Documentation

Start with the product docs in **[`docs/product/`](docs/product/README.md)**:

| Doc | What it covers |
|-----|----------------|
| [Mission & Vision](docs/product/mission-and-vision.md) | Why SummaryBot exists and the principles that guide it |
| [Capabilities](docs/product/capabilities.md) | What it does today (shipped + tested) |
| [Functional areas](docs/product/functional-areas.md) | The subsystems and how they fit |
| [Roles & user stories](docs/product/roles-and-user-stories.md) | Who uses it and the jobs they get done |
| [User guide](docs/product/user-guide.md) | Step-by-step: run, connect, summarize, schedule, deliver, administer |

Deeper references:

- **[Coverage map](docs/coverage-map.md)** — the authoritative shipped-vs-planned matrix (kept current with each change).
- **[PRD](docs/PRD.md)** — full requirements; **[reference brief](docs/reference-brief.md)** — the reasoning/traps behind them.
- **[ADRs](docs/adr/)** — architecture decisions. **[Conventions](docs/conventions/)** — durable working agreements.

## Quickstart

**Run locally (development):**

```bash
# API (in-memory DB); add --features http-llm,oauth,discord,slack,email,confluence,gdrive for the full build
API_BIND=127.0.0.1:8080 SECRET_KEY=dev-secret DATABASE_URL=:memory: cargo run -p api

# Dashboard (separate terminal) — proxies the API
cd web && npm install && npm run dev
```

Open the URL Vite prints, enter any workspace name, and choose **Dev sign-in**.
Or run `scripts/demo.sh` for a no-secrets, in-memory end-to-end API walk.

**Run in production (Docker):**

```bash
SECRET_KEY=$(openssl rand -hex 32) \
LLM_CONFIG_KEY=$(openssl rand -hex 32) \
OPENROUTER_API_KEY=sk-or-...           \
docker compose up --build
```

`SECRET_KEY` is required (never baked into the image); the SQLite database
persists in a named volume. `fly.toml` is included for Fly.io. See the
[user guide](docs/product/user-guide.md#1-running-summarybot) for all env vars.

## Repository layout

```
crates/
  domain/          pure policy + types (no I/O), exhaustively unit-tested
  repository/      SQLite storage; every read/write workspace/tenant-scoped
  host/            orchestration + all I/O (LLM, fetchers, delivery, knowledge, scheduler)
  api/             thin axum HTTP layer (OpenAPI-shaped) + serves the dashboard
  wasm-summarize/  the summarize step, built for wasm32-wasip2
web/               React/TypeScript dashboard (Vite)
wit/               the WASM component interface
docs/              product docs, ADRs, PRD, coverage map, conventions
```

## Build & test

```bash
cargo build --workspace          # default (network-free) build
cargo test  --workspace          # unit + integration tests
cargo clippy --workspace --all-targets
cd web && npm run build          # typecheck + build the dashboard
```

Optional integrations are behind cargo features (`http-llm`, `oauth`, `discord`,
`slack`, `email`, `confluence`, `gdrive`) so the default build stays
network-free. The production image enables them all.

## License

No license file is currently present in the repository; contact the maintainers
for usage terms.
