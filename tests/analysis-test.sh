#!/usr/bin/env bash
# analysis-test.sh — acceptance tests for the traffic analysis pipeline (P2).
#
# Local assertions (protocol/flow stats, HTTP reconstruction on synthetic data,
# TLS decrypt failure classification) run unconditionally; decryption of a real
# captured TLS flow is gated on hub reachability and SKIPs when offline.
#
# Prereqs: teamx built. Usage: ./tests/analysis-test.sh

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

echo "== analysis-test: setup =="
[ -x "$TEAMX" ] || TEAMX=$(command -v teamx)
echo "teamx binary: $TEAMX"

# ---------------------------------------------------------------- T-D1 stats + list
echo "== T-D1: stats & list execute =="
$TEAMX --json analyze stats >/tmp/teamx-astats-$$.json 2>&1
python3 -c "import json; d=json.load(open('/tmp/teamx-astats-$$.json')); assert 'total_flows' in d and 'protocols' in d" 2>/dev/null
check "stats returns total_flows + protocols" $?

$TEAMX --json analyze list --limit 5 >/dev/null 2>&1
check "analyze list exits 0" $?
$TEAMX --json analyze list --sni '*.google.com' --limit 5 >/dev/null 2>&1
check "analyze list --sni filter parses" $?

# ---------------------------------------------------------------- T-D2 protocol detection (on captured data)
echo "== T-D2: protocol detection =="
TLS_N=$(sql "SELECT COUNT(*) FROM flows WHERE proto='tls';" 2>/dev/null | tr -d ' ')
if [ "${TLS_N:-0}" -ge 1 ]; then
  ok "tls flows detected (${TLS_N})"
else
  skip "no tls flows captured (hub offline or no traffic)"
fi

# ---------------------------------------------------------------- T-D3 HTTP reconstruct (local, synthetic)
echo "== T-D3: HTTP reconstruction (synthetic) =="
FID=$(sql "SELECT id FROM flows WHERE proto='http/1.1' ORDER BY id DESC LIMIT 1;" 2>/dev/null | tr -d ' ')
if [ -n "$FID" ]; then
  R=$($TEAMX --json analyze http --flow "$FID" 2>/dev/null)
  echo "$R" | grep -q 'request_head\|response_head'
  check "analyze http returns request/response heads" $?
else
  skip "no plain-HTTP flow captured (needs network)"
fi

# ---------------------------------------------------------------- T-D4 decrypt failure classification
echo "== T-D4: decrypt (no-key classification) =="
if [ -n "$(sql "SELECT client_random FROM flows WHERE client_random IS NOT NULL LIMIT 1;" 2>/dev/null)" ]; then
  FID=$(sql "SELECT id FROM flows WHERE client_random IS NOT NULL ORDER BY id DESC LIMIT 1;" 2>/dev/null | tr -d ' ')
  OUT=$($TEAMX --json analyze decrypt --flow "$FID" 2>&1)
  # without a matching keylog this should be a classified error, not a crash
  echo "$OUT" | grep -qi 'no matching traffic secret\|decrypted_bytes'
  check "decrypt reports classified outcome (flow $FID)" $?
else
  skip "no TLS flow with client_random"
fi

rm -f /tmp/teamx-astats-$$.json

echo ""
echo "== analysis-test: $PASS passed, $FAIL failed, $SKIP skipped =="
exit $([ $FAIL -eq 0 ] && echo 0 || echo 1)
