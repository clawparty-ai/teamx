#!/usr/bin/env bash
# teamx network-mode mTLS identity test (I1).
# Verifies the `teamx serve` mutual-TLS handshake and that the RPC layer derives
# the actor identity from the client certificate CN (`member:<id>:<role>`), not
# from any self-reported session in the request body.
#
# Requires: curl, openssl, python3. Skipped gracefully if curl/openssl absent.
set -euo pipefail

TEAMX="${TEAMX:-$(dirname "$0")/../target/debug/teamx}"
DB="$(mktemp /tmp/teamx-mtls-XXXXXX).db"
HOME_DIR="$(mktemp -d /tmp/teamx-mtls-home-XXXXXX)"
PORT="${TEAMX_TEST_PORT:-5787}"
export TEAMX_DB="$DB"
export TEAMX_HOME="$HOME_DIR"

pass() { printf '  ok: %s\n' "$*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

cleanup() {
  [ -n "${SERVE_PID:-}" ] && kill "$SERVE_PID" 2>/dev/null || true
  rm -f "$DB" "$DB-wal" "$DB-shm"
  rm -rf "$HOME_DIR"
}
trap cleanup EXIT

if ! command -v curl >/dev/null 2>&1 || ! command -v openssl >/dev/null 2>&1; then
  echo "skipped: curl/openssl not available"
  exit 0
fi

step() { printf '\n=== %s ===\n' "$*"; }

step "setup: create team + owner cert + member invitation"
"$TEAMX" init
"$TEAMX" team create "Mtls" --session s:owner --json >/dev/null
OWNER=$("$TEAMX" team status --session s:owner --json | python3 -c "import json,sys; print(json.load(sys.stdin)['teams'][0]['members'][0]['id'])")
OWNER_CERT="$(mktemp -d /tmp/teamx-mtls-owner-XXXXXX)"
"$TEAMX" cert issue "$OWNER" owner --out "$OWNER_CERT" --json >/dev/null
INV=$("$TEAMX" team invite "reviewer: code review" --session s:owner --json)
LETTER=$(echo "$INV" | python3 -c "import json,sys; print(json.load(sys.stdin)['letter'])")
ALICE_MID=$(echo "$INV" | python3 -c "import json,sys; print(json.load(sys.stdin)['member_id'])")

# extract alice's cert/key/ca from the letter
ALICE_DIR="$(mktemp -d /tmp/teamx-mtls-alice-XXXXXX)"
echo "$LETTER" | python3 -c "
import json,sys,base64
s=sys.stdin.read().strip()[len('teamx-inv:v1:'):]
d=json.loads(base64.b64decode(s))
c=d['certificates']
open('$ALICE_DIR/client.crt','w').write(c['client_cert'])
open('$ALICE_DIR/client.key','w').write(c['client_key'])
open('$ALICE_DIR/ca.crt','w').write(c['ca_cert'])
"

step "server certificate verifies against the CA (openssl)"
openssl verify -CAfile "$HOME_DIR/ca/ca.crt" "$HOME_DIR/ca/server.crt" >/dev/null 2>&1 \
  || fail "server cert should verify against the CA"
pass "server cert chain OK"

step "start serve (mTLS, forced)"
"$TEAMX" serve --addr 127.0.0.1 --port "$PORT" >/dev/null 2>&1 &
SERVE_PID=$!
# wait for the listener to come up
for _ in $(seq 1 30); do
  if curl -sS --max-time 2 --cacert "$HOME_DIR/ca/ca.crt" --cert "$OWNER_CERT/member.crt" --key "$OWNER_CERT/member.key" "https://127.0.0.1:$PORT/health" >/dev/null 2>&1; then
    break
  fi
  sleep 0.2
done
pass "serve is up"

step "no client cert → rejected"
if curl -sS --max-time 5 --cacert "$HOME_DIR/ca/ca.crt" "https://127.0.0.1:$PORT/health" >/dev/null 2>&1; then
  fail "request without a client cert should be rejected"
fi
pass "rejected without client cert"

step "owner cert → health + team.status (identity from cert, empty args)"
curl -sS --max-time 5 --cacert "$HOME_DIR/ca/ca.crt" --cert "$OWNER_CERT/member.crt" --key "$OWNER_CERT/member.key" \
  "https://127.0.0.1:$PORT/health" | grep -q '"ok":true' || fail "health should be ok with a valid cert"
STATUS=$(curl -sS --max-time 5 --cacert "$HOME_DIR/ca/ca.crt" --cert "$OWNER_CERT/member.crt" --key "$OWNER_CERT/member.key" \
  -H 'Content-Type: application/json' -d '{"method":"team.status","args":{}}' "https://127.0.0.1:$PORT/rpc")
echo "$STATUS" | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert d.get('ok') is True, d
assert d['data']['teams'][0]['team']['name']=='Mtls', d
" || fail "team.status should resolve the owner identity from the cert"
pass "owner identity resolved from certificate CN"

step "RPC team.import with the member cert creates the pending seat"
IMPORT=$(curl -sS --max-time 5 --cacert "$ALICE_DIR/ca.crt" --cert "$ALICE_DIR/client.crt" --key "$ALICE_DIR/client.key" \
  -H 'Content-Type: application/json' \
  -d "{\"method\":\"team.import\",\"args\":{\"letter\":\"$LETTER\",\"name\":\"Alice\"}}" \
  "https://127.0.0.1:$PORT/rpc")
echo "$IMPORT" | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert d.get('ok') is True, d
assert d['data']['member_id']=='$ALICE_MID', d
assert d['data']['status']=='pending', d
assert d['data']['role']=='reviewer', d
" || fail "team.import should create the member using the cert-derived id"
pass "imported member id matches the cert CN member id"

step "member cert cannot act as the owner (authorization stays role-scoped)"
# Alice is pending, so she cannot approve anyone yet — approving herself should fail.
if curl -sS --max-time 5 --cacert "$ALICE_DIR/ca.crt" --cert "$ALICE_DIR/client.crt" --key "$ALICE_DIR/client.key" \
  -H 'Content-Type: application/json' -d "{\"method\":\"team.approve\",\"args\":{\"member_id\":\"$ALICE_MID\"}}" \
  "https://127.0.0.1:$PORT/rpc" 2>/dev/null | grep -q '"ok":true'; then
  fail "pending member should not be able to approve"
fi
pass "pending member cannot approve (non-owner rejected)"

step "cross-team read is rejected (network mode authorization)"
# a second team owned by a different session; Alice (member of Mtls) must NOT be
# able to read it over RPC.
OTHER_ID=$("$TEAMX" team create "Other" --session s:owner2 --json | python3 -c "import json,sys; print(json.load(sys.stdin)['team']['id'])")
if curl -sS --max-time 5 --cacert "$ALICE_DIR/ca.crt" --cert "$ALICE_DIR/client.crt" --key "$ALICE_DIR/client.key" \
  -H 'Content-Type: application/json' -d "{\"method\":\"team.status\",\"args\":{\"team\":\"$OTHER_ID\"}}" \
  "https://127.0.0.1:$PORT/rpc" 2>/dev/null | grep -q '"ok":true'; then
  fail "member should not be able to read another team's status"
fi
if curl -sS --max-time 5 --cacert "$ALICE_DIR/ca.crt" --cert "$ALICE_DIR/client.crt" --key "$ALICE_DIR/client.key" \
  -H 'Content-Type: application/json' -d "{\"method\":\"events\",\"args\":{\"team\":\"$OTHER_ID\"}}" \
  "https://127.0.0.1:$PORT/rpc" 2>/dev/null | grep -q '"ok":true'; then
  fail "member should not be able to read another team's events"
fi
pass "cross-team read rejected (team.status / events)"

step "ALL mTLS TESTS PASS"
