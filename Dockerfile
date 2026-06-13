# syntax=docker/dockerfile:1
#
# Multi-stage production image for SummaryBot: builds the dashboard SPA and the
# native API binary, then ships a slim runtime. The API serves the SPA itself
# (WEB_DIR) and talks to its providers over rustls (no system OpenSSL). The
# sandboxed wasm component is a build-time/CLI artifact and is NOT needed at
# runtime, so it is intentionally absent from the image.

#####################  Stage 1 — dashboard SPA  ######################
FROM node:20-bookworm-slim AS web
WORKDIR /web
# Install deps against the lockfile first for layer caching, then build.
COPY web/package.json web/package-lock.json* ./
RUN npm ci
COPY web/ ./
RUN npm run build            # → /web/dist (hashed assets + index.html)

#####################  Stage 2 — API binary  #########################
FROM rust:1.96-bookworm AS build
WORKDIR /src
# rusqlite is compiled with the `bundled` SQLite, which needs a C toolchain —
# present in the full bookworm image (not the -slim variant).
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
# Production feature set: real LLM (http-llm), OAuth login, and every delivery
# sink (webhook is http-llm; discord/slack channel send; email; confluence;
# google drive). All TLS is rustls.
RUN cargo build --release -p api \
      --features http-llm,oauth,discord,slack,email,confluence,gdrive

#####################  Stage 3 — slim runtime  #######################
FROM debian:bookworm-slim AS runtime
# ca-certificates: outbound TLS to LLM providers / Discord / Slack / SMTP.
# curl: the container HEALTHCHECK below.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*
# Non-root runtime user; /data holds the SQLite database (mount a volume there
# so it survives container restarts/redeploys).
RUN useradd --system --uid 10001 --create-home app \
 && mkdir -p /data && chown app:app /data
WORKDIR /app
COPY --from=build /src/target/release/api /usr/local/bin/summarybot-api
COPY --from=web   /web/dist               /app/web/dist
ENV API_BIND=0.0.0.0:8080 \
    WEB_DIR=/app/web/dist \
    DATABASE_URL=/data/summarybot.db \
    BASE_DOMAIN=summarybot.app
# SECRET_KEY (HS256 signing key) is REQUIRED and deliberately not baked in —
# supply it at runtime (`-e SECRET_KEY=$(openssl rand -hex 32)`). The server
# fails fast without it. LLM_CONFIG_KEY (64 hex) enables encrypted BYO-key /
# platform-credential storage; LLM_* / OPENROUTER_API_KEY select the backend.
USER app
VOLUME ["/data"]
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
  CMD curl -fsS http://127.0.0.1:8080/healthz || exit 1
CMD ["summarybot-api"]
