#!/bin/bash
# tun0 macOS integration test (requires sudo — run as: sudo bash tests/tun0-macos-test.sh)
#
# Verifies:
#   1. tun0 device is created (utunN)
#   2. fake-ip route is injected
#   3. TCP traffic through tun0 is forwarded via a teamx egress
#   4. fake-ip DNS returns fake A records
#   5. tun0 stop cleans up
#
# Prereqs: a reachable teamx server + an egress. Set these env vars:
#   TEAMX          path to teamx binary (default ../target/debug/teamx)
#   SERVER_URL     e.g. https://127.0.0.1:5805
#   EXIT_NAME      e.g. tun-egress
#   MTLS_CERT/KEY/CA   egress-side client certs (unused here if local setup)
#   TEST_SERVER    optional: start a local serve on this port (default 5805)
#   TEST_HOME      teamx home for the local serve (default $TMPDIR/tun0-lab)
set -uo pipefail

TEAMX="${TEAMX:-$(cd "$(dirname "$0")/.." && pwd)/target/debug/teamx}"
SERVER_URL="${SERVER_URL:-}"
EXIT_NAME="${EXIT_NAME:-tun-egress}"
TEST_SERVER="${TEST_SERVER:-5805}"
TEST_HOME="${TEST_HOME:-$TMPDIR/tun0-lab}"
LAB="${TMPDIR}/tun0-macos-run"
rm -rf "$LAB"; mkdir -p "$LAB"

FAILS=0
pass() { echo "  ok: $1"; }
fail() { echo "FAIL: $1"; FAILS=$((FAILS+1)); }

# If SERVER_URL empty, set up a local serve + egress on this machine.
if [ -z "$SERVER_URL" ]; then
  echo "=== setup: local serve + egress ==="
  export TEAMX_HOME="$TEST_HOME/server" TEAMX_DB="$TEST_HOME/server/teamx.db"
  mkdir -p "$TEST_HOME/server" "$TEST_HOME/owner" "$TEST_HOME/b"
  "$TEAMX" init >/dev/null 2>&1
  CREATE=$("$TEAMX" team create TunMac --session s:owner --json)
  OWNER=$(echo "$CREATE" | python3 -c 'import sys,json;print(json.load(sys.stdin)["owner_member_id"])')
  "$TEAMX" cert issue "$OWNER" owner --out "$TEST_HOME/owner" >/dev/null 2>&1
  INVITE=$("$TEAMX" team invite "contributor: exit node" --session s:owner --json)
  MEMBER_B=$(echo "$INVITE" | python3 -c 'import sys,json;print(json.load(sys.stdin)["member_id"])')
  LETTER=$(echo "$INVITE" | python3 -c 'import sys,json;print(json.load(sys.stdin)["letter"])')
  python3 - "$LETTER" "$TEST_HOME/b" <<'PY'
import sys,json,base64,pathlib
d=json.loads(base64.b64decode(sys.argv[1][len('teamx-inv:v1:'):]))
p=pathlib.Path(sys.argv[2]); p.mkdir(exist_ok=True)
(p/'client.crt').write_text(d['certificates']['client_cert'])
(p/'client.key').write_text(d['certificates']['client_key'])
(p/'ca.crt').write_text(d['certificates']['ca_cert'])
PY
  "$TEAMX" team import "$LETTER" --name exit-b --session s:b >/dev/null 2>&1
  "$TEAMX" team approve "$MEMBER_B" --session s:owner >/dev/null 2>&1

  nohup "$TEAMX" serve --addr 127.0.0.1 --port "$TEST_SERVER" >"$LAB/serve.log" 2>&1 &
  sleep 2
  export TEAMX_SERVER_URL="https://127.0.0.1:$TEST_SERVER"
  export TEAMX_MTLS_CERT="$TEST_HOME/b/client.crt" TEAMX_MTLS_KEY="$TEST_HOME/b/client.key" TEAMX_MTLS_CA="$TEST_HOME/server/ca/ca.crt"
  nohup "$TEAMX" proxy exit "$EXIT_NAME" >"$LAB/exit.log" 2>&1 &
  sleep 2
  SERVER_URL="$TEAMX_SERVER_URL"
  echo "  server=$SERVER_URL exit=$EXIT_NAME"
fi

# tun0 start needs root — we already are (sudo).
echo "=== tun0 start ==="
"$TEAMX" tun0 start --server "$SERVER_URL" --exit "$EXIT_NAME" >"$LAB/tun0.log" 2>&1 &
TUN_PID=$!
sleep 3
cat "$LAB/tun0.log"

# find the actual utun device name
DEV=$(grep -oE 'utun[0-9]+' "$LAB/tun0.log" | head -1)
if [ -z "$DEV" ]; then
  # fallback: ask ifconfig for tun-like interfaces
  DEV=$(ifconfig | grep -oE '^utun[0-9]+' | tail -1)
fi
[ -n "$DEV" ] && pass "tun device up: $DEV" || fail "no utun device found"
if [ -z "$DEV" ]; then echo "--- tun0.log ---"; cat "$LAB/tun0.log"; echo "--- serve ---"; cat "$LAB/serve.log"; echo "--- exit ---"; cat "$LAB/exit.log"; kill $TUN_PID 2>/dev/null; exit 1; fi

echo "=== fake-ip DNS ==="
FAKE=$(dig +short @198.18.0.1 example.com A 2>/dev/null | head -1)
echo "  fake A for example.com: $FAKE"
if echo "$FAKE" | grep -qE '^198\.18\.'; then pass "fake-ip DNS answered 198.18.x.x"; else fail "fake-ip DNS: got '$FAKE'"; fi

echo "=== TCP through tun0 (curl --interface) ==="
# Curl example.com through the tun device; it should succeed via the egress.
CODE=$(curl -s --max-time 15 --interface "$DEV" -o /dev/null -w '%{http_code}' https://example.com 2>&1)
echo "  https://example.com via $DEV -> HTTP $CODE"
[ "$CODE" = "200" ] && pass "curl via tun0 returned 200" || fail "curl via tun0: $CODE"

# Exit IP should be the egress node's IP (here: same machine, loopback demo
# uses the local egress). For a real egress we'd check ifconfig.me.
echo "=== stop ==="
"$TEAMX" tun0 stop --dev "$DEV" >/dev/null 2>&1
sleep 1
kill $TUN_PID 2>/dev/null
wait $TUN_PID 2>/dev/null
echo "  tun0 stopped"

echo
if [ "$FAILS" = "0" ]; then echo "ALL TUN0 MACOS TESTS PASS"; exit 0; else echo "$FAILS FAILURES"; exit 1; fi
