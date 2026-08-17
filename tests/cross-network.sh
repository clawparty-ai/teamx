#!/usr/bin/env bash
# teamx N4 — cross-network path verification (single-machine simulation).
#
# Binds `teamx serve` to 0.0.0.0 and connects over the machine's LAN IP (not
# loopback) so the server certificate SAN + CA trust chain are exercised the
# way a remote member would. Skips gracefully when no non-loopback IPv4 exists.
#
# Requires: curl, openssl, python3.
set -euo pipefail

TEAMX="${TEAMX:-$(dirname "$0")/../target/debug/teamx}"
DB="$(mktemp /tmp/teamx-net-XXXXXX).db"
HOME_DIR="$(mktemp -d /tmp/teamx-net-home-XXXXXX)"
PORT="${TEAMX_TEST_PORT:-5793}"
export TEAMX_DB="$DB"
export TEAMX_HOME="$HOME_DIR"

pass() { printf '  ok: %s\n' "$*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }
cleanup() {
  [ -n "${SERVE_PID:-}" ] && kill "$SERVE_PID" 2>/dev/null || true
  rm -f "$DB" "$DB-wal" "$DB-shm"
  rm -rf "$HOME_DIR" "${OWNER_CERT:-}" "${MEMBER_DIR:-}"
}
trap cleanup EXIT

step() { printf '\n=== %s ===\n' "$*"; }

# Detect a non-loopback IPv4 (macOS `ipconfig`, then Linux `hostname -I`).
LAN_IP=""
if command -v ipconfig >/dev/null 2>&1; then
  LAN_IP="$(ipconfig getifaddr en0 2>/dev/null || ipconfig getifaddr en1 2>/dev/null || true)"
fi
if [ -z "$LAN_IP" ] && command -v hostname >/dev/null 2>&1; then
  LAN_IP="$(hostname -I 2>/dev/null | awk '{print $1}' || true)"
fi
if [ -z "$LAN_IP" ] || [ "$LAN_IP" = "127.0.0.1" ]; then
  echo "skipped: no non-loopback IPv4 detected"
  exit 0
fi

if ! command -v curl >/dev/null 2>&1 || ! command -v openssl >/dev/null 2>&1; then
  echo "skipped: curl/openssl not available"
  exit 0
fi

step "setup: create team + owner cert + invite (LAN server URL)"
"$TEAMX" init
"$TEAMX" team create "Net" --session s:owner --json >/dev/null
OWNER=$("$TEAMX" team status --session s:owner --json | python3 -c "import json,sys; print(json.load(sys.stdin)['teams'][0]['members'][0]['id'])")
OWNER_CERT="$(mktemp -d /tmp/teamx-net-owner-XXXXXX)"
"$TEAMX" cert issue "$OWNER" owner --out "$OWNER_CERT" --json >/dev/null
INV=$("$TEAMX" team invite "reviewer: code review" --server-url "https://$LAN_IP:$PORT" --session s:owner --json)
LETTER=$(echo "$INV" | python3 -c "import json,sys; print(json.load(sys.stdin)['letter'])")
MEMBER_DIR="$(mktemp -d /tmp/teamx-net-member-XXXXXX)"
echo "$LETTER" | python3 -c "
import json,sys,base64
s=sys.stdin.read().strip()[len('teamx-inv:v1:'):]
d=json.loads(base64.b64decode(s))
c=d['certificates']
open('$MEMBER_DIR/client.crt','w').write(c['client_cert'])
open('$MEMBER_DIR/client.key','w').write(c['client_key'])
open('$MEMBER_DIR/ca.crt','w').write(c['ca_cert'])
"
pass "invite embeds https://$LAN_IP:$PORT"

step "start serve on 0.0.0.0 with LAN IP SAN"
"$TEAMX" serve --addr 0.0.0.0 --port "$PORT" --san "$LAN_IP" >/dev/null 2>&1 &
SERVE_PID=$!
for _ in $(seq 1 30); do
  if curl -sS --max-time 2 --cacert "$HOME_DIR/ca/ca.crt" --cert "$OWNER_CERT/member.crt" --key "$OWNER_CERT/member.key" "https://127.0.0.1:$PORT/health" >/dev/null 2>&1; then
    break
  fi
  sleep 0.2
done

step "server cert SAN includes the LAN IP"
python3 - <<EOF
import subprocess
out = subprocess.run(['openssl','x509','-in','$HOME_DIR/ca/server.crt','-noout','-text'], capture_output=True, text=True).stdout
assert '$LAN_IP' in out, f"server cert should carry LAN IP $LAN_IP as a SAN"
EOF
pass "server cert SAN has $LAN_IP"

step "connect over the LAN IP (not loopback) with mTLS"
STATUS=$(curl -sS --max-time 5 --cacert "$HOME_DIR/ca/ca.crt" --cert "$OWNER_CERT/member.crt" --key "$OWNER_CERT/member.key" \
  -H 'Content-Type: application/json' -d '{"method":"team.status","args":{}}' "https://$LAN_IP:$PORT/rpc")
echo "$STATUS" | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert d.get('ok') is True, d
assert d['data']['teams'][0]['team']['name']=='Net', d
" || fail "team.status over LAN IP failed"
pass "RPC over $LAN_IP resolves identity from cert"

step "member import over the LAN IP creates the pending seat"
IMPORT=$(curl -sS --max-time 5 --cacert "$MEMBER_DIR/ca.crt" --cert "$MEMBER_DIR/client.crt" --key "$MEMBER_DIR/client.key" \
  -H 'Content-Type: application/json' \
  -d "{\"method\":\"team.import\",\"args\":{\"letter\":\"$LETTER\",\"name\":\"Alice\"}}" \
  "https://$LAN_IP:$PORT/rpc")
echo "$IMPORT" | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert d.get('ok') is True, d
assert d['data']['status']=='pending', d
" || fail "team.import over LAN IP failed"
pass "member imported over $LAN_IP"

step "ALL CROSS-NETWORK (LAN) TESTS PASS"
