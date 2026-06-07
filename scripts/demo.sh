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

echo "==> create a summary (real Phase-3 pipeline, deterministic demo client)"
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

# ----------------------------------------------------------------------------
# Control plane (Phase 8 tenancy): provision a tenant, create a workspace,
# attach a source, invite + accept a member, resolve the tenant by host.
# ----------------------------------------------------------------------------
echo "==> provision tenant 'acme' (caller becomes Owner)"
curl -fsS -X POST "${BASE}/tenants" \
  -H "authorization: Bearer ${TOKEN}" -H 'content-type: application/json' \
  -d '{"id":"acme","name":"Acme Inc","subdomain":"acme"}' \
  | jq '{id, name, subdomain}'

echo "==> resolve tenant by host header (acme.summarybot.app)"
curl -fsS "${BASE}/tenant" -H 'host: acme.summarybot.app' | jq '{id, name, subdomain}'

echo "==> create a workspace under the tenant (WSP-009)"
curl -fsS -X POST "${BASE}/tenants/acme/workspaces" \
  -H "authorization: Bearer ${TOKEN}" -H 'content-type: application/json' \
  -d '{"id":"ws-eng","name":"Engineering"}' | jq '{id, tenant_id, name}'

echo "==> attach a Slack source to the workspace (WSP-008)"
curl -fsS -X POST "${BASE}/tenants/acme/workspaces/ws-eng/connections" \
  -H "authorization: Bearer ${TOKEN}" -H 'content-type: application/json' \
  -d '{"platform":"slack","platform_id":"T-DEMO"}' | jq '{platform, platform_id}'

echo "==> invite a member (raw token returned once)"
INVITE=$(curl -fsS -X POST "${BASE}/tenants/acme/invites" \
  -H "authorization: Bearer ${TOKEN}" -H 'content-type: application/json' \
  -d '{"email":"teammate@example.com","role":"member"}')
INVITE_TOKEN=$(echo "${INVITE}" | jq -r .token)
echo "    invite for $(echo "${INVITE}" | jq -r .email) as $(echo "${INVITE}" | jq -r .role)"

echo "==> a second user logs in and accepts the invite"
TOKEN2=$(curl -fsS -X POST "${BASE}/auth/login" \
  -H 'content-type: application/json' \
  -d '{"provider":"email","subject":"y","email":"teammate@example.com","workspaces":[]}' \
  | jq -r .access_token)
curl -fsS -X POST "${BASE}/invites/accept" \
  -H "authorization: Bearer ${TOKEN2}" -H 'content-type: application/json' \
  -d "{\"token\":\"${INVITE_TOKEN}\"}" | jq '{accepted, role}'

echo "==> list tenant members (owner + accepted member)"
curl -fsS "${BASE}/tenants/acme/members" \
  -H "authorization: Bearer ${TOKEN}" | jq 'map({user_id, role})'

echo "==> auth is enforced (no token → 401)"
echo "    status: $(curl -s -o /dev/null -w '%{http_code}' "${BASE}/workspaces/${WS}/summaries")"

echo "==> openapi"
curl -fsS "${BASE}/openapi.json" | jq '.info'

echo "==> done"
