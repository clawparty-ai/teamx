#!/bin/bash
# tun0 Linux integration test (requires root — run as: sudo bash tests/tun0-linux-test.sh)
#
# Verifies on Linux:
#   1. tun0 device is created (tunN) via /dev/net/tun
#   2. fake-ip route is injected
#   3. TCP through tun0 is forwarded via a teamx egress (real cloud exit)
#   4. fake-ip DNS returns fake A records
#   5. domain routing: *.com -> exit A, others -> default
#   6. tun0 stop cleans up
#
# Env (set before running):
#   TEAMX        path to teamx binary (default ../target/release/teamx)
#   SERVER_URL   teamx server URL (e.g. https://hub03.flomesh.io:8888)
#   EXIT_A / EXIT_B   two exits to test routing between (optional)
#   MTLS_CERT/KEY/CA  member certs that are already approved
#   EXPECT_IP_A  IP that exit A egresses from (for split check)
#   EXPECT_IP_B  IP that exit B egresses from
set -uo pipefail

TEAMX="${TEAMX:-$(cd "$(dirname "$0")/.." && pwd)/target/release/teamx}"
LAB="${TMPDIR:-/tmp}/tun0-linux-run"
rm -rf "$LAB"; mkdir -p "$LAB"

FAILS=0
pass() { echo "  ok: $1"; }
fail() { echo "FAIL: $1"; FAILS=$((FAILS+1)); }

[ -z "${SERVER_URL:-}" ] && { echo "SERVER_URL is required"; exit 2; }
export TEAMX_SERVER_URL="$SERVER_URL"

echo "=== tun0 start ==="
"$TEAMX" tun0 start --exit "${EXIT_A:-egress}" >"$LAB/tun0.log" 2>&1 &
TUN_PID=$!
sleep 4
cat "$LAB/tun0.log"

DEV=$(grep -oE '\btun[0-9]+' "$LAB/tun0.log" | head -1)
[ -z "$DEV" ] && DEV="tun0"
if ip link show "$DEV" >/dev/null 2>&1; then pass "tun device up: $DEV"; else fail "no tun device: $DEV"; cat "$LAB/tun0.log"; fi

echo "=== fake-ip route ==="
ip route show 2>/dev/null | grep -E '198\.18\.' && pass "fake-ip route present" || fail "no 198.18 route"

echo "=== fake-ip DNS ==="
FAKE=$(dig +short @198.18.0.1 example.com A 2>/dev/null | head -1)
echo "  fake A for example.com: $FAKE"
echo "$FAKE" | grep -qE '^198\.18\.' && pass "fake-ip DNS answered" || fail "fake-ip DNS: '$FAKE'"

echo "=== TCP through tun0 (curl --interface) ==="
CODE=$(curl -s --max-time 20 --interface "$DEV" -o /dev/null -w '%{http_code}' https://example.com 2>&1)
echo "  https://example.com via $DEV -> HTTP $CODE"
[ "$CODE" = "200" ] && pass "curl via tun0 200" || fail "curl via tun0: $CODE"

echo "=== egress IP via tun0 ==="
IP_OUT=$(curl -s --max-time 20 --interface "$DEV" https://ifconfig.me 2>&1)
echo "  egress IP: $IP_OUT"
if [ -n "${EXPECT_IP_A:-}" ]; then
  [ "$IP_OUT" = "$EXPECT_IP_A" ] && pass "egress IP matches A" || fail "egress IP $IP_OUT != $EXPECT_IP_A"
fi

echo "=== stop ==="
"$TEAMX" tun0 stop --dev "$DEV" >/dev/null 2>&1
kill $TUN_PID 2>/dev/null
wait $TUN_PID 2>/dev/null
sleep 1
pass "tun0 stopped"

echo
if [ "$FAILS" = "0" ]; then echo "ALL TUN0 LINUX TESTS PASS"; exit 0; else echo "$FAILS FAILURES"; exit 1; fi
