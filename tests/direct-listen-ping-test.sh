#!/usr/bin/env bash
# Direct-connect P1 E2E: provider `direct listen` + consumer `direct ping`.
#
# Verifies the full member-to-member path on loopback:
#   advertise (LAN IP + listener port) → probe (issues one-time pairing
#   ticket) → mTLS connect (member certs, team CA) → ticket validated
#   server-side (direct.ticket.check) → ping/pong RTTs.
# Also verifies the probe rate limit: the (N+1)th probe inside the window is
# refused and audited as direct.rejected.
#
# Requires: curl, python3. Skipped gracefully if absent.
set -euo pipefail

TEAMX="${TEAMX:-$(dirname "$0")/../target/debug/teamx}"
DB="$(mktemp /tmp/teamx-direct-p1-XXXXXX).db"
HOME_DIR="$(mktemp -d /tmp/teamx-direct-p1-home-XXXXXX)"
PORT="${TEAMX_TEST_PORT:-5793}"
export TEAMX_DB="$DB"
export TEAMX_HOME="$HOME_DIR"

pass() { printf '  ok: %s\n' "$*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

PROV_PID=""
SERVE_PID=""
PROV_LOG="$(mktemp /tmp/teamx-direct-p1-provlog-XXXXXX)"
cleanup() {
  [ -n "${PROV_PID:-}" ] && kill "$PROV_PID" 2>/dev/null || true
  [ -n "${SERVE_PID:-}" ] && kill "$SERVE_PID" 2>/dev/null || true
  rm -f "$DB" "$DB-wal" "$DB-shm" "$PROV_LOG"
  rm -rf "$HOME_DIR"
}
trap cleanup EXIT

if ! command -v curl >/dev/null 2>&1 || ! command -v python3 >/dev/null 2>&1; then
  echo "skipped: curl/python3 not available"
  exit 0
fi

step() { printf '\n=== %s ===\n' "$*"; }

# Extract a letter's certificates into a directory (client.crt/key + ca.crt).
extract_letter() { # letter_json member_id outdir
  echo "$1" | python3 -c "
import json,sys,base64
letter=json.load(sys.stdin)['letter']
s=letter.strip()[len('teamx-inv:v1:'):]
d=json.loads(base64.b64decode(s))
c=d['certificates']
open('$3/client.crt','w').write(c['client_cert'])
open('$3/client.key','w').write(c['client_key'])
open('$3/ca.crt','w').write(c['ca_cert'])
"
}

step "setup: team + owner cert + two member invitations"
"$TEAMX" init >/dev/null
"$TEAMX" team create "Direct" --session s:owner --json >/dev/null
OWNER=$("$TEAMX" team status --session s:owner --json | python3 -c "import json,sys; print(json.load(sys.stdin)['teams'][0]['members'][0]['id'])")
OWNER_CERT="$(mktemp -d /tmp/teamx-direct-p1-owner-XXXXXX)"
"$TEAMX" cert issue "$OWNER" owner --out "$OWNER_CERT" --json >/dev/null

INV_P=$("$TEAMX" team invite "provider member" --session s:owner --json)
PROV_MID=$(echo "$INV_P" | python3 -c "import json,sys; print(json.load(sys.stdin)['member_id'])")
PROV_DIR="$(mktemp -d /tmp/teamx-direct-p1-prov-XXXXXX)"
extract_letter "$INV_P" "$PROV_MID" "$PROV_DIR"

INV_C=$("$TEAMX" team invite "consumer member" --session s:owner --json)
CONS_MID=$(echo "$INV_C" | python3 -c "import json,sys; print(json.load(sys.stdin)['member_id'])")
CONS_DIR="$(mktemp -d /tmp/teamx-direct-p1-cons-XXXXXX)"
extract_letter "$INV_C" "$CONS_MID" "$CONS_DIR"

# Import both letters (creates the member rows, pending) then approve (active).
# Session keys must be unique per member (UNIQUE(team_id, session_key)).
"$TEAMX" team import "$(echo "$INV_P" | python3 -c "import json,sys; print(json.load(sys.stdin)['letter'])")" --session s:prov-import --json >/dev/null
"$TEAMX" team import "$(echo "$INV_C" | python3 -c "import json,sys; print(json.load(sys.stdin)['letter'])")" --session s:cons-import --json >/dev/null
"$TEAMX" team approve "$PROV_MID" --session s:owner --json >/dev/null
"$TEAMX" team approve "$CONS_MID" --session s:owner --json >/dev/null
pass "team ready: owner=$OWNER provider=$PROV_MID consumer=$CONS_MID"

step "start serve (mTLS)"
"$TEAMX" serve --addr 127.0.0.1 --port "$PORT" >/dev/null 2>&1 &
SERVE_PID=$!
for _ in $(seq 1 30); do
  if curl -sS --max-time 2 --cacert "$HOME_DIR/ca/ca.crt" --cert "$OWNER_CERT/member.crt" --key "$OWNER_CERT/member.key" "https://127.0.0.1:$PORT/health" >/dev/null 2>&1; then
    break
  fi
  sleep 0.2
done
pass "serve is up on https://127.0.0.1:$PORT"

step "provider: direct listen (random port, loopback advertised)"
TEAMX_MTLS_CERT="$PROV_DIR/client.crt" \
TEAMX_MTLS_KEY="$PROV_DIR/client.key" \
TEAMX_MTLS_CA="$PROV_DIR/ca.crt" \
  "$TEAMX" direct listen --session s:prov --lan-ip 127.0.0.1 --server "https://127.0.0.1:$PORT" \
  >"$PROV_LOG" 2>&1 &
PROV_PID=$!
for _ in $(seq 1 30); do
  grep -q "direct listener on tcp://" "$PROV_LOG" 2>/dev/null && break
  sleep 0.2
done
grep -q "direct listener on tcp://" "$PROV_LOG" || { cat "$PROV_LOG"; fail "provider listener did not come up"; }
pass "provider listener up and advertised"

step "consumer: direct ping (probe → ticket → mTLS → RTT ×2)"
PING_OUT=$(TEAMX_MTLS_CERT="$CONS_DIR/client.crt" \
TEAMX_MTLS_KEY="$CONS_DIR/client.key" \
TEAMX_MTLS_CA="$CONS_DIR/ca.crt" \
  "$TEAMX" direct ping "$PROV_MID" --session s:cons --count 2 --timeout-ms 1500 \
  --server "https://127.0.0.1:$PORT" 2>&1) || { echo "$PING_OUT"; fail "direct ping failed"; }
echo "$PING_OUT"
echo "$PING_OUT" | grep -q "direct ping $PROV_MID via 127.0.0.1:" || fail "ping output missing header"
echo "$PING_OUT" | grep -q "rtt=" || fail "ping output missing RTTs"
pass "direct ping end-to-end OK"

step "provider saw the established session"
grep -q "direct session established with $CONS_MID" "$PROV_LOG" \
  || { cat "$PROV_LOG"; fail "provider did not log an established session"; }
pass "provider logged session with $CONS_MID"

step "rate limit: probe #11 inside the window is refused"
LAST=""
for _ in $(seq 1 11); do
  LAST=$(TEAMX_MTLS_CERT="$CONS_DIR/client.crt" \
  TEAMX_MTLS_KEY="$CONS_DIR/client.key" \
  TEAMX_MTLS_CA="$CONS_DIR/ca.crt" \
    "$TEAMX" direct ping "$PROV_MID" --session s:cons --count 1 --timeout-ms 1500 \
    --server "https://127.0.0.1:$PORT" 2>&1 || true)
done
echo "$LAST" | grep -q "rate limited" || { echo "$LAST"; fail "probe #11 should be rate limited"; }
pass "probe rate limited after 10 in window"

step "direct.rejected audited in the ledger"
python3 - "$DB" <<'PY'
import sqlite3, sys
conn = sqlite3.connect(sys.argv[1])
n = conn.execute("SELECT COUNT(*) FROM events WHERE type='direct.rejected'").fetchone()[0]
assert n >= 1, f"expected direct.rejected events, got {n}"
print(f"  ok: ledger has {n} direct.rejected event(s)")
PY
pass "direct.rejected present"

echo
echo "ALL DIRECT-P1 TESTS PASSED"
