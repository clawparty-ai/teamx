#!/usr/bin/env bash
# replay-test.sh — acceptance tests for TCP/HTTP flow replay (P3).
#
# Local assertions (stream reassembly, mock serve, HAR export, curl generation)
# run unconditionally; network replay (client mode, http send) is gated on hub
# reachability and SKIPs when offline.
#
# Prereqs: teamx built; capture.db with at least one flow (see capture-test.sh).
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

hub_online() {
  curl -s -o /dev/null --max-time 6 "https://www.google.com/generate_204" 2>/dev/null
  return $?
}

echo "== replay-test: setup =="
[ -x "$TEAMX" ] || TEAMX=$(command -v teamx)
echo "teamx binary: $TEAMX"

FID=$(sql "SELECT id FROM flows ORDER BY id DESC LIMIT 1;" 2>/dev/null | tr -d ' ')
if [ -z "$FID" ]; then
  echo "no flows in capture.db — seeding a synthetic flow for local assertions"
  FID=1
  SEEDED=1
else
  SEEDED=0
fi

# ---------------------------------------------------------------- T-C2 mock serve
echo "== T-C2: mock serve (local) =="
# seed a flow with a known down-stream if the DB is empty
if [ "$SEEDED" = "1" ]; then
  sql "INSERT OR IGNORE INTO flows (id, started_at, source, proto, local_addr, remote_addr) VALUES (1, 1.0, 'test', 'http/1.1', '10.0.0.2', '127.0.0.1:1');" >/dev/null 2>&1
  sql "INSERT OR IGNORE INTO stream_chunks (flow_id, dir, seq, ts, data) VALUES (1,'up',1,1.1,x'474554202f'),(1,'down',1,1.2,x'485454502f312e3120323030204f4b0d0a0d0a68656c6c6f');" >/dev/null 2>&1
fi
PORT=18090
$TEAMX replay serve --flow "$FID" --port $PORT >/dev/null 2>&1 &
MOCK_PID=$!
sleep 1.5
RESP=$(curl -s --max-time 5 "http://127.0.0.1:$PORT/" 2>/dev/null | head -c 100)
kill $MOCK_PID 2>/dev/null
[ -n "$RESP" ]
check "mock serve returns recorded bytes (${#RESP}B)" $?

# ---------------------------------------------------------------- T-C3 HTTP layer (local)
echo "== T-C3: HTTP exchange / HAR / to-curl (local) =="
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
  skip "no http_exchanges yet (to-curl)"
fi

# ---------------------------------------------------------------- T-C1/T-C4 network replay
if hub_online; then
  echo "== T-C1/T-C4: client + http replay (network) =="
  if [ -n "$FID" ]; then
    OUT=/tmp/teamx-replay-$$.json
    timeout 30 $TEAMX --json replay tcp start --flow "$FID" --diff -o "$OUT" >/dev/null 2>&1
    RC=$?
    [ $RC -eq 0 ] && check "tcp replay exits 0" 0 || check "tcp replay exits 0" 1
    if [ -f "$OUT" ]; then
      python3 -c "import json,sys; d=json.load(open('$OUT')); sys.exit(0 if 'status' in d else 1)" 2>/dev/null
      check "replay result JSON has status" $?
    fi
  fi
else
  skip "hub offline — client/http replay (T-C1/T-C4) not run"
fi

rm -f /tmp/teamx-test-$$.har /tmp/teamx-replay-$$.json

echo ""
echo "== replay-test: $PASS passed, $FAIL failed, $SKIP skipped =="
exit $([ $FAIL -eq 0 ] && echo 0 || echo 1)
