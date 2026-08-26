#!/usr/bin/env bash
# replay-test.sh — acceptance tests for TCP/HTTP flow replay (P3).
#
# Local assertions (mock serve, HAR export, curl generation) run
# unconditionally; network client replay is gated on hub reachability and
# SKIPs when offline.
#
# Prereqs: teamx built; capture-test.sh run first (for a TLS flow).
# Usage: ./tests/replay-test.sh

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

sql() { command -v sqlite3 >/dev/null 2>&1 && sqlite3 "$CAPDB" "$1" 2>/dev/null; }

export TEAMX_SERVER_URL="${TEAMX_SERVER_URL:-https://81.70.41.108:8888}"
hub_online() {
  local host port
  host=$(echo "$TEAMX_SERVER_URL" | sed -E 's#https?://([^:/]+)(:([0-9]+))?.*#\1#')
  port=$(echo "$TEAMX_SERVER_URL" | sed -E 's#https?://[^:/]+:?([0-9]+)?.*#\1#')
  port="${port:-443}"
  nc -z -G 6 "$host" "$port" 2>/dev/null
  return $?
}

echo "== replay-test: setup =="
[ -x "$TEAMX" ] || TEAMX=$(command -v teamx)
echo "teamx binary: $TEAMX"

# ---------------------------------------------------------------- seed a plain-HTTP flow
# Always seed a deterministic plain-HTTP flow for the local mock/HAR checks,
# independent of whatever capture-test.sh recorded.
UP_HEX=474554202f68656c6c6f20485454502f312e310d0a486f73743a206578616d706c652e636f6d0d0a0d0a
DOWN_HEX=485454502f312e3120323030204f4b0d0a436f6e74656e742d4c656e6774683a20350d0a0d0a68656c6c6f
if command -v sqlite3 >/dev/null 2>&1; then
  SEED_FID=$(sqlite3 "$CAPDB" "SELECT COALESCE(MAX(id),0)+1 FROM flows;" 2>/dev/null)
  sqlite3 "$CAPDB" "INSERT OR REPLACE INTO flows (id, started_at, source, proto, local_addr, remote_addr, bytes_up, bytes_down) VALUES ($SEED_FID, 1.0, 'test', 'http/1.1', '10.0.0.2', 'example.com:80', 39, 48);" 2>/dev/null
  sqlite3 "$CAPDB" "INSERT OR REPLACE INTO stream_chunks (flow_id, dir, seq, ts, data) VALUES ($SEED_FID,'up',1,1.1,x'$UP_HEX'),($SEED_FID,'down',1,1.2,x'$DOWN_HEX');" 2>/dev/null
else
  SEED_FID=""
fi

# ---------------------------------------------------------------- T-C2 mock serve
echo "== T-C2: mock serve (local) =="
PORT=18090
$TEAMX replay serve --flow "${SEED_FID:-1}" --port $PORT >/dev/null 2>&1 &
MOCK_PID=$!
sleep 1.5
RESP=$(curl -s --max-time 5 "http://127.0.0.1:$PORT/" 2>/dev/null | head -c 100)
kill $MOCK_PID 2>/dev/null
[ -n "$RESP" ]
check "mock serve returns recorded bytes (${#RESP}B)" $?

# ---------------------------------------------------------------- T-C3 HTTP layer (local)
echo "== T-C3: HTTP exchange / HAR / to-curl (local) =="
if [ -n "$SEED_FID" ]; then
  $TEAMX --json replay http extract --flow "$SEED_FID" >/dev/null 2>&1
fi
$TEAMX --json replay http export-har -o /tmp/teamx-test-$$.har >/dev/null 2>&1
[ -s /tmp/teamx-test-$$.har ]
check "HAR file generated" $?
python3 -c "
import json
h=json.load(open('/tmp/teamx-test-$$.har'))
e=h['log']['entries']
assert isinstance(e, list)
" 2>/dev/null
check "HAR structure valid (log.entries[])" $?

EXID=$(sql "SELECT id FROM http_exchanges ORDER BY id DESC LIMIT 1;" 2>/dev/null | tr -d ' ')
if [ -n "$EXID" ]; then
  CURLCMD=$($TEAMX replay http to-curl --exchange "$EXID" 2>/dev/null | head -1)
  case "$CURLCMD" in
    curl\ *) ok "to-curl emits a curl command" ;;
    *)       bad "to-curl output unexpected: ${CURLCMD:0:40}" ;;
  esac
else
  skip "no http_exchanges (to-curl)"
fi

# ---------------------------------------------------------------- T-C1 network client replay
if hub_online; then
  echo "== T-C1: client replay via proxy exit (network) =="
  TLS_FID=$(sql "SELECT id FROM flows WHERE proto='tls' ORDER BY id DESC LIMIT 1;" 2>/dev/null | tr -d ' ')
  if [ -n "$TLS_FID" ]; then
    OUT=/tmp/teamx-replay-$$.json
    $TEAMX --json replay tcp start --flow "$TLS_FID" --exit egress2 --diff -o "$OUT" >/dev/null 2>&1
    RC=$?
    [ $RC -eq 0 ] && check "tcp replay exits 0" 0 || check "tcp replay exits 0" 1
    if [ -f "$OUT" ]; then
      python3 -c "import json,sys; d=json.load(open('$OUT')); sys.exit(0 if 'status' in d else 1)" 2>/dev/null
      check "replay result JSON has status" $?
    fi
  else
    skip "no TLS flow recorded (run capture-test.sh first)"
  fi
else
  skip "hub offline — client replay (T-C1) not run"
fi

rm -f /tmp/teamx-test-$$.har /tmp/teamx-replay-$$.json

echo ""
echo "== replay-test: $PASS passed, $FAIL failed, $SKIP skipped =="
exit $([ $FAIL -eq 0 ] && echo 0 || echo 1)
