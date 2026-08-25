#!/usr/bin/env bash
# analysis-test.sh — acceptance tests for the traffic analysis pipeline
# (protocol detection, TLS decrypt, HTTP parsing, FTS search, stats).
#
# Prereqs:
#   - capture.db with recorded flows (run capture-test.sh first)
#   - SSLKEYLOGFILE captured during recording for decrypt tests (or the
#     import-keylog fixture used by capture-test.sh)
#
# Usage: ./tests/analysis-test.sh

set -u
cd "$(dirname "$0")/.."

TEAMX=${TEAMX:-./target/debug/teamx}
CAPDB="$HOME/.teamx/capture.db"
PASS=0; FAIL=0

ok()  { echo "PASS: $1"; PASS=$((PASS+1)); }
bad() { echo "FAIL: $1"; FAIL=$((FAIL+1)); }
check() { if [ "$2" -eq 0 ]; then ok "$1"; else bad "$1"; fi; }

sql() { sqlite3 "$CAPDB" "$1" 2>/dev/null | tr -d ' '; }

echo "== analysis-test: setup =="
[ -f "$CAPDB" ] || { echo "no capture.db — run capture-test.sh first"; exit 1; }

# ---------------------------------------------------------------- T-D1 protocol detection
echo "== T-D1: protocol detection =="
TLS_N=$(sql "SELECT COUNT(*) FROM flows WHERE protocol='tls';" 2>/dev/null)
[ "${TLS_N:-0}" -ge 1 ]
check "tls flows detected (${TLS_N:-0})" $?

ANY_N=$(sql "SELECT COUNT(*) FROM flows WHERE protocol IS NOT NULL AND protocol != '';" 2>/dev/null)
ALL_N=$(sql "SELECT COUNT(*) FROM flows;" 2>/dev/null)
[ "${ANY_N:-0}" -eq "${ALL_N:-1}" ]
check "every flow has a protocol label ($ANY_N/$ALL_N)" $?

# ---------------------------------------------------------------- T-D2 decrypt pipeline
echo "== T-D2: TLS decrypt pipeline =="
$TEAMX analyze decrypt --all >/dev/null 2>&1
DEC_OK=$(sql "SELECT COUNT(*) FROM http_exchanges;" 2>/dev/null)
DEC_ERR=$(sql "SELECT COUNT(DISTINCT decrypt_error) FROM flows WHERE decrypt_error IS NOT NULL AND decrypt_error != '';" 2>/dev/null)

echo "  exchanges=$DEC_OK, distinct error classes=$DEC_ERR"
if [ "${DEC_OK:-0}" -ge 1 ]; then
  ok "decrypt produced http_exchanges ($DEC_OK)"
else
  # Without a real keylog this is expected — assert failures are *classified*
  ERR=$(sql "SELECT decrypt_error FROM flows WHERE decrypt_error IS NOT NULL LIMIT 1;" 2>/dev/null)
  [ -n "$ERR" ]
  check "no keys available but failure classified ($ERR)" $?
fi

# failure classification vocabulary
if [ "${DEC_ERR:-0}" -ge 1 ]; then
  BAD=$(sql "SELECT COUNT(*) FROM flows WHERE decrypt_error NOT IN
    ('no-keys','bad-record','unsupported-cipher','handshake-failed');" 2>/dev/null)
  [ "${BAD:-0}" -eq 0 ]
  check "decrypt errors use known classes" $?
fi

# ---------------------------------------------------------------- T-D3 HTTP parsing
echo "== T-D3: HTTP parsing correctness =="
EXN=$(sql "SELECT COUNT(*) FROM http_exchanges;" 2>/dev/null)
if [ "${EXN:-0}" -ge 1 ]; then
  # method/url/status sanity
  BADROWS=$(sql "SELECT COUNT(*) FROM http_exchanges
                 WHERE method IS NULL OR url IS NULL OR status IS NULL
                    OR url NOT LIKE 'http%';" 2>/dev/null)
  [ "${BADROWS:-1}" = "0" ]
  check "exchanges have method/url/status ($EXN rows)" $?

  # timing sane
  BADT=$(sql "SELECT COUNT(*) FROM http_exchanges WHERE res_end < req_start;" 2>/dev/null)
  [ "${BADT:-1}" = "0" ]
  check "timing monotonic (res_end >= req_start)" $?

  # headers are JSON arrays
  HJ=$(sql "SELECT COUNT(*) FROM http_exchanges WHERE json_valid(req_headers);" 2>/dev/null)
  [ "${HJ:-0}" -eq "$EXN" ]
  check "req_headers stored as JSON" $?
else
  echo "SKIP: no exchanges (needs real keylog); parser covered by unit tests"
fi

# ---------------------------------------------------------------- T-D4 FTS search
echo "== T-D4: full-text search =="
FTS=$(sql "SELECT COUNT(*) FROM sqlite_master WHERE name='plaintext_fts';" 2>/dev/null)
if [ "${FTS:-0}" -ge 1 ]; then
  HIT=$(sql "SELECT COUNT(*) FROM plaintext_fts WHERE plaintext_fts MATCH 'http' LIMIT 5;" 2>/dev/null)
  [ "${HIT:-0}" -ge 1 ]
  check "FTS matches common term (hits=$HIT)" $?
else
  echo "SKIP: plaintext_fts not built yet"
fi

# ---------------------------------------------------------------- T-D5 filter syntax
echo "== T-D5: filter syntax =="
R=$($TEAMX analyze list --filter 'sni:*.google.com' --json 2>/dev/null | head -c 200)
case "$R" in
  *\{*) ok "sni glob filter returns JSON results" ;;
  *)    bad "sni filter unexpected output: ${R:0:60}" ;;
esac
$TEAMX analyze list --filter 'bytes:>10KB and method:POST' >/dev/null 2>&1
check "compound filter parses (bytes+method)" $?
$TEAMX analyze list --filter 'bogus!!syntax' >/dev/null 2>&1
[ $? -ne 0 ]
check "invalid filter rejected with error" $?

# ---------------------------------------------------------------- T-D6 stats
echo "== T-D6: statistics engine =="
$TEAMX analyze stats --by host --json >/dev/null 2>&1
check "stats by host executes" $?
HOSTROW=$(sql "SELECT COUNT(*) FROM stats_by_host;" 2>/dev/null)
[ "${HOSTROW:-0}" -ge 1 ]
check "stats_by_host populated ($HOSTROW hosts)" $?
# p95 between p50 and max sanity on one row
SANE=$(sql "SELECT COUNT(*) FROM stats_by_host
            WHERE p50_ms <= p95_ms;" 2>/dev/null)
TOT=$(sql "SELECT COUNT(*) FROM stats_by_host;" 2>/dev/null)
[ "${SANE:-0}" = "${TOT:-1}" ]
check "percentiles monotonic (p50<=p95)" $?
TL=$(sql "SELECT COUNT(*) FROM stats_timeline;" 2>/dev/null)
[ "${TL:-0}" -ge 1 ]
check "timeline buckets exist ($TL)" $?

echo ""
echo "== analysis-test: $PASS passed, $FAIL failed =="
exit $([ $FAIL -eq 0 ] && echo 0 || echo 1)
