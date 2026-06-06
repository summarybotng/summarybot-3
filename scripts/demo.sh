#!/usr/bin/env bash
# End-to-end demo of the SummaryBot API — no secrets, no LLM, no network.
# Boots the server on an in-memory DB and walks the auth + summary flow.
#
# Usage:  ./scripts/demo.sh            (needs: cargo, curl, jq)
set -euo pipefail

PORT="${PORT:-8787}"
BASE="http://127.0.0.1:${PORT}"
WS="ws-demo"

echo "==> building api"
cargo build -p api --quiet

echo "==> starting server on ${BASE} (in-memory db)"
API_BIND="127.0.0.1:${PORT}" SECRET_KEY="demo-secret-please-change" DATABASE_URL=":memory:" \
  cargo run -q -p api &
SERVER_PID=$!
trap 'kill "${SERVER_PID}" 2>/dev/null || true' EXIT

# Wait for liveness.
for _ in $(seq 1 50); do
  if curl -fsS "${BASE}/healthz" >/dev/null 2>&1; then break; fi
  sleep 0.2
done
echo "    healthz: $(curl -fsS "${BASE}/healthz")"

echo "==> login (email provider, granting ${WS})"
TOKEN=$(curl -fsS -X POST "${BASE}/auth/login" \
  -H 'content-type: application/json' \
  -d "{\"provider\":\"email\",\"subject\":\"x\",\"email\":\"demo@example.com\",\"workspaces\":[\"${WS}\"]}" \
  | jq -r .access_token)
echo "    got access token: ${TOKEN:0:24}…"

echo "==> create a summary (demo extractive, no LLM)"
SUMMARY=$(curl -fsS -X POST "${BASE}/workspaces/${WS}/summaries" \
  -H "authorization: Bearer ${TOKEN}" -H 'content-type: application/json' \
  -d '{"messages":["morning all","lets ship the release on friday","ill write the changelog","sounds good"]}')
ID=$(echo "${SUMMARY}" | jq -r .id)
echo "    created ${ID}: $(echo "${SUMMARY}" | jq -r .text)"

echo "==> pin it"
curl -fsS -X POST "${BASE}/workspaces/${WS}/summaries/${ID}/pin" \
  -H "authorization: Bearer ${TOKEN}" | jq '{id, pinned}'

echo "==> tag it"
curl -fsS -X PUT "${BASE}/workspaces/${WS}/summaries/${ID}/tags" \
  -H "authorization: Bearer ${TOKEN}" -H 'content-type: application/json' \
  -d '{"tags":["release","demo"]}' | jq '{id, tags}'

echo "==> list summaries"
curl -fsS "${BASE}/workspaces/${WS}/summaries" \
  -H "authorization: Bearer ${TOKEN}" | jq 'map({id, pinned, tags, text})'

echo "==> create a daily 09:00 UTC schedule"
SCHED=$(curl -fsS -X POST "${BASE}/workspaces/${WS}/schedules" \
  -H "authorization: Bearer ${TOKEN}" -H 'content-type: application/json' \
  -d '{"schedule_type":"daily","hour":9,"minute":0,"timezone":"UTC"}')
SCHED_ID=$(echo "${SCHED}" | jq -r .id)
echo "    ${SCHED_ID}: $(echo "${SCHED}" | jq -c '{schedule_type, hour, timezone, next_run, enabled}')"

echo "==> pause the schedule"
curl -fsS -X POST "${BASE}/workspaces/${WS}/schedules/${SCHED_ID}/pause" \
  -H "authorization: Bearer ${TOKEN}" | jq '{id, enabled}'

echo "==> list schedules"
curl -fsS "${BASE}/workspaces/${WS}/schedules" \
  -H "authorization: Bearer ${TOKEN}" | jq 'map({id, schedule_type, enabled})'

echo "==> delete the schedule (expect 204)"
echo "    status: $(curl -s -o /dev/null -w '%{http_code}' -X DELETE \
  "${BASE}/workspaces/${WS}/schedules/${SCHED_ID}" -H "authorization: Bearer ${TOKEN}")"

echo "==> auth is enforced (no token → 401)"
echo "    status: $(curl -s -o /dev/null -w '%{http_code}' "${BASE}/workspaces/${WS}/summaries")"

echo "==> openapi"
curl -fsS "${BASE}/openapi.json" | jq '.info'

echo "==> done"
