#!/usr/bin/env bash
# Build + run the SummaryBot API for local dev, loading secrets from .env.local.
#
# Usage:  ./scripts/dev-api.sh            (foreground)
#         ./scripts/dev-api.sh &          (background)
#
# Reads .env.local (gitignored) for SECRET_KEY, LLM_CONFIG_KEY, LLM/OAuth secrets,
# etc. See .env.local for the full list. Anything unset falls back to a safe
# default / the demo backend.
set -euo pipefail
cd "$(dirname "$0")/.."

ENV_FILE="${ENV_FILE:-.env.local}"
if [[ -f "$ENV_FILE" ]]; then
  echo "==> loading $ENV_FILE"
  set -a
  # shellcheck disable=SC1090
  source "$ENV_FILE"
  set +a
else
  echo "==> no $ENV_FILE found — using demo defaults (set SECRET_KEY at minimum)"
fi

# Sensible local defaults if the env file didn't set them.
export API_BIND="${API_BIND:-127.0.0.1:8080}"
export DATABASE_URL="${DATABASE_URL:-/tmp/sbverify.db}"
export OAUTH_REDIRECT_BASE="${OAUTH_REDIRECT_BASE:-http://localhost:8080}"

# Feature set: real LLM + OAuth connect + all delivery sinks.
FEATURES="${FEATURES:-http-llm,oauth,confluence,gdrive,discord,slack,email}"

echo "==> building api (--features $FEATURES)"
cargo build -p api --features "$FEATURES"

echo "==> starting api on $API_BIND (db: $DATABASE_URL)"
exec ./target/debug/api
