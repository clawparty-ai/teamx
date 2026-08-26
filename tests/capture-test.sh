#!/usr/bin/env bash
# capture-test.sh — acceptance tests for the local traffic capture system (P1).
#
# Prereqs: teamx built; a proxy exit online (for the network taps).
# Network assertions are gated on hub reachability and SKIP when offline;
# local CLI/DB assertions always run.
#
# Usage:
#   ./tests/capture-test.sh                          # socks5-mode tests
#   CAPTURE_WITH_TUN0=1 ./tests/capture-test.sh      # + tun0 tap tests (root)

set -u
cd "$(dirname "$0")/.."

TEAMX=${TEAMX:-./target/debug/teamx}
export TEAMX_CAPTURE_DB="${TEAMX_CAPTURE_DB:-/tmp/teamx-capture-test.db}"
CAPDB="$TEAMX_CAPTURE_DB"
PASS=0; FAIL=0; SKIP=0

ok()  { echo "PASS: $1"; PASS=$((PASS+1)); }
bad() { echo "FAIL: $1"; FAIL=$((FAIL+1)); }
skip(){ echo "SKIP: $1"; SKIP=$((SKIP+1)); }
check() { [ "$2" -eq 0 ] && ok "$1" || bad "$1"; }

have() { command -v "$1" >/dev/null 2>&1; }

sql() { have sqlite3 && sqlite3 "$CAPDB" "$1" 2>/dev/null; }

# hub/network reachability gate
export TEAMX_SERVER_URL="${TEAMX_SERVER_URL:-https://81.70.41.108:8888}"
hub_online() {
  local host port
  host=$(echo "$TEAMX_SERVER_URL" | sed -E 's#https?://([^:/]+)(:([0-9]+))?.*#\1#')
  port=$(echo "$TEAMX_SERVER_URL" | sed -E 's#https?://[^:/]+:?([0-9]+)?.*#\1#')
  port="${port:-443}"
  nc -z -G 6 "$host" "$port" 2>/dev/null
  return $?
}

# ---------------------------------------------------------------- setup
echo "== capture-test: setup =="
[ -x "$TEAMX" ] || TEAMX=$(command -v teamx)
echo "teamx binary: $TEAMX"
rm -f "$CAPDB"

# ---------------------------------------------------------------- T-B0 local CLI
echo "== T-B0: local capture CLI =="
$TEAMX --json capture status >/dev/null 2>&1
check "capture status exits 0" $?
$TEAMX --json capture list --limit 5 >/dev/null 2>&1
check "capture list exits 0 (empty db)" $?
$TEAMX --json capture prune --max-size 512MB >/dev/null 2>&1
check "capture prune exits 0" $?
$TEAMX --json analyze stats >/dev/null 2>&1
check "analyze stats exits 0 (empty db)" $?
$TEAMX --json replay http export-har -o /tmp/teamx-empty-$$.har >/dev/null 2>&1
check "replay export-har exits 0 (empty db)" $?

# ---------------------------------------------------------------- network taps
if hub_online; then
  echo "== T-B1: proxy capture taps (network) =="
  PROXY_PORT=18081
  if [ "${CAPTURE_WITH_TUN0:-0}" = "1" ]; then
    $TEAMX tun0 start --capture >/dev/null 2>&1 &
    PROXY_PID=$!
    SOCKS_FLAG=""
  else
    $TEAMX proxy start --port "$PROXY_PORT" --capture >/dev/null 2>&1 &
    PROXY_PID=$!
    SOCKS_FLAG="--socks5-hostname 127.0.0.1:$PROXY_PORT"
  fi
  sleep 2

  TARGET="https://www.google.com/generate_204"
  curl -s -o /dev/null --max-time 15 $SOCKS_FLAG "$TARGET"
  sleep 1

  FC=$(sql "SELECT COUNT(*) FROM flows;" 2>/dev/null | tr -d ' ')
  [ "${FC:-0}" -ge 1 ]
  check "flows recorded (got ${FC:-0})" $?

  SNI=$(sql "SELECT sni FROM flows WHERE sni IS NOT NULL LIMIT 1;" 2>/dev/null)
  [ "$SNI" = "www.google.com" ]
  check "ClientHello SNI extracted ($SNI)" $?

  CC=$(sql "SELECT COUNT(*) FROM stream_chunks;" 2>/dev/null | tr -d ' ')
  [ "${CC:-0}" -ge 2 ]
  check "stream_chunks both directions (got ${CC:-0})" $?

  FID=$(sql "SELECT id FROM flows ORDER BY id DESC LIMIT 1;" 2>/dev/null)
  if [ -n "$FID" ]; then
    PCAP=/tmp/teamx-test-$$.pcap
    $TEAMX --json capture export --flow "$FID" --pcap "$PCAP" >/dev/null 2>&1
    head -c4 "$PCAP" 2>/dev/null | xxd -p 2>/dev/null | grep -qiE 'd4c3b2a1|a1b2c3d4'
    check "pcap magic present (flow $FID)" $?
    rm -f "$PCAP"
  else
    skip "no flow captured (proxy/hub issue)"
  fi

  kill $PROXY_PID 2>/dev/null
else
  skip "hub offline — proxy capture taps (T-B1) not run"
fi

echo ""
echo "== capture-test: $PASS passed, $FAIL failed, $SKIP skipped =="
exit $([ $FAIL -eq 0 ] && echo 0 || echo 1)
