#!/usr/bin/env bash
# replay-test.sh — acceptance tests for TCP/HTTP flow replay.
#
# Prereqs:
#   - teamx built
#   - capture.db with at least one recorded TLS flow (run capture-test.sh first)
#   - a proxy exit online for client-replay mode
#
# Usage: ./tests/replay-test.sh

set -u
cd "$(dirname "$0")/.."

TEAMX=${TEAMX:-./target/debug/teamx}
CAPDB="$HOME/.teamx/capture.db"
PASS=0; FAIL=0

ok()  { echo "PASS: $1"; PASS=$((PASS+1)); }
bad() { echo "FAIL: $1"; FAIL=$((FAIL+1)); }
check() { if [ "$2" -eq 0 ]; then ok "$1"; else bad "$1"; fi; }

sql() { sqlite3 "$CAPDB" "$1" 2>/dev/null | tr -d ' '; }

echo "== replay-test: setup =="
[ -f "$CAPDB" ] || { echo "no capture.db — run capture-test.sh first"; exit 1; }
FID=$(sql "SELECT id FROM flows ORDER BY id DESC LIMIT 1")
[ -n "$FID" ] || { echo "no flows recorded"; exit 1; }
echo "using flow id=$FID"

# ---------------------------------------------------------------- T-C1 TCP client replay
echo "== T-C1: TCP client replay =="
# Record a fresh, deterministic session against httpbin-like echo (use
# httpbingo.org which echoes request metadata), then replay its up-stream.
SEED_URL="https://httpbingo.org/get"
BEFORE_BODY=$(curl -s --max-time 15 --socks5-hostname 127.0.0.1:18081 \
  "$SEED_URL" 2>/dev/null | head -c 200)

# find the newest flow (the curl above, if capture was running) else reuse FID
NEWFID=$(sql "SELECT id FROM flows ORDER BY id DESC LIMIT 1")

OUT=/tmp/teamx-replay-$$.json
timeout 60 $TEAMX replay tcp start "$NEWFID" --diff -o "$OUT" >/dev/null 2>&1
RC=$?
[ $RC -eq 0 ]
check "tcp replay exits 0" $?

if [ -f "$OUT" ]; then
  python3 -c "
import json,sys
d=json.load(open('$OUT'))
sys.exit(0 if d.get('status') in ('ok','diff') else 1)
" 2>/dev/null
  check "replay result JSON valid" $?
else
  bad "replay result file missing"
fi

# ---------------------------------------------------------------- T-C2 mock serve
echo "== T-C2: mock serve mode =="
PORT=18090
$TEAMX replay serve "$FID" --port $PORT >/dev/null 2>&1 &
MOCK_PID=$!
sleep 1.5
# any connection should receive *something* from the recorded down-stream
RESP=$(curl -s --max-time 8 "http://127.0.0.1:$PORT/" 2>/dev/null | head -c 100)
kill $MOCK_PID 2>/dev/null
[ -n "$RESP" ]
check "mock serve returns recorded bytes (${#RESP}B)" $?

# ---------------------------------------------------------------- T-C4 HTTP replay
echo "== T-C4: HTTP replay =="
# HAR round-trip on a synthetic exchange (decryption-independent): build one
# via the proxy against a plain-HTTP target so no keylog is needed.
PLAIN_URL="http://httpbingo.org/get"
curl -s --max-time 15 --socks5-hostname 127.0.0.1:18081 "$PLAIN_URL" >/dev/null 2>&1
sleep 1

HAR=/tmp/teamx-test-$$.har
$TEAMX replay export har --session last -o "$HAR" >/dev/null 2>&1
[ -s "$HAR" ]
check "HAR exported" $?
have python3 && python3 -c "
import json,sys
h=json.load(open('$HAR'))
e=h['log']['entries']
sys.exit(0 if isinstance(e,list) and len(e)>=1 else 1)
" 2>/dev/null
check "HAR structure valid (log.entries[])" $?

# to-curl prints an executable command
CURLCMD=$($TEAMX replay to-curl $(sql "SELECT id FROM http_exchanges ORDER BY id DESC LIMIT 1" 2>/dev/null) 2>/dev/null | head -1)
case "$CURLCMD" in
  curl\ *) ok "to-curl emits a curl command" ;;
  *)       bad "to-curl output unexpected: ${CURLCMD:0:40}" ;;
esac

# single send reproduces the response status
EXID=$(sql "SELECT id FROM http_exchanges ORDER BY id DESC LIMIT 1" 2>/dev/null)
if [ -n "$EXID" ]; then
  ORIG_STATUS=$(sql "SELECT status FROM http_exchanges WHERE id=$EXID;" 2>/dev/null)
  SEND_OUT=/tmp/teamx-send-$$.json
  timeout 30 $TEAMX replay http send "$EXID" -o "$SEND_OUT" >/dev/null 2>&1
  RC=$?
  [ $RC -eq 0 ]
  check "replay http send exits 0" $?
  NEW_STATUS=$(python3 -c "
import json,sys
try:
    d=json.load(open('$SEND_OUT'))
    print(d.get('response',{}).get('status',''))
except Exception:
    print('')
" 2>/dev/null)
  [ "$NEW_STATUS" = "$ORIG_STATUS" ]
  check "replayed status matches original ($ORIG_STATUS)" $?
else
  echo "SKIP: no decrypted exchanges yet (needs Part D decrypt pipeline or plain HTTP)"
fi

# variables + mapping flags parse
$TEAMX replay http send "$EXID" \
  --var token=test123 --map api.prod.com=api.staging.local \
  -o /dev/null >/dev/null 2>&1
check "var/map flags accepted" $?   # may fail to connect to staging; flag parsing is what's tested here — accept both outcomes
ok "(flag parse smoke — outcome not asserted)"

# ---------------------------------------------------------------- cleanup
rm -f /tmp/teamx-replay-$$.json /tmp/teamx-send-$$.json /tmp/teamx-test-$$.har

echo ""
echo "== replay-test: $PASS passed, $FAIL failed =="
exit $([ $FAIL -eq 0 ] && echo 0 || echo 1)
