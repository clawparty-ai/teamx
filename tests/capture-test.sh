#!/usr/bin/env bash
# capture-test.sh — acceptance tests for the local traffic capture system.
#
# Prereqs:
#   - teamx built (target/debug or ~/.local/bin)
#   - a proxy exit online (default_exit configured in route table)
#   - root available for tun0 mode; socks5 mode runs unprivileged
#
# Usage:
#   ./tests/capture-test.sh              # socks5-mode tests only (no root needed)
#   CAPTURE_WITH_TUN0=1 ./tests/capture-test.sh   # + tun0 tap tests (root)
#
# Each check prints PASS/FAIL; exit code is non-zero if any failed.

set -u
cd "$(dirname "$0")/.."

TEAMX=${TEAMX:-./target/debug/teamx}
CAPDB="$HOME/.teamx/capture.db"
PASS=0; FAIL=0

ok()  { echo "PASS: $1"; PASS=$((PASS+1)); }
bad() { echo "FAIL: $1"; FAIL=$((FAIL+1)); }
check() { # desc, condition-result(0=pass)
  if [ "$2" -eq 0 ]; then ok "$1"; else bad "$1"; fi
}

have() { command -v "$1" >/dev/null 2>&1; }

# ---------------------------------------------------------------- helpers
sql() { # run sqlite3 against capture.db (best-effort)
  have sqlite3 || return 1
  sqlite3 "$CAPDB" "$1"
}

flow_count() { sql "SELECT COUNT(*) FROM flows;" 2>/dev/null | tr -d ' '; }
chunk_count() { sql "SELECT COUNT(*) FROM stream_chunks;" 2>/dev/null | tr -d ' '; }

wait_for() { # wait_for <cond-cmd> <timeout-sec>
  local t=0
  until eval "$2" >/dev/null 2>&1; do
    sleep 0.5; t=$((t+1))
    [ $t -gt $(( $3 * 2 )) ] && return 1
  done
  return 0
}

# ---------------------------------------------------------------- T-B0 setup
echo "== capture-test: setup =="
[ -x "$TEAMX" ] || TEAMX=$(command -v teamx)
echo "teamx binary: $TEAMX"

rm -f "$CAPDB"

# Start capture (socks5 source) + a SOCKS5 proxy on 18081
$TEAMX capture start --sources socks5 --out "$CAPDB" >/dev/null 2>&1
$TEAMX capture status 2>/dev/null | grep -q running
check "capture start (status=running)" $?

$TEAMX proxy start --port 18081 >/dev/null 2>&1 &
PROXY_PID=$!
sleep 1.5

# ---------------------------------------------------------------- T-B1 taps
echo "== T-B1: taps & data integrity =="
TARGET="https://www.google.com/generate_204"
BEFORE=$(curl -s -o /dev/null -w '%{size_download}' --max-time 15 \
  --socks5-hostname 127.0.0.1:18081 "$TARGET")
sleep 1

FC=$(flow_count); [ "${FC:-0}" -ge 1 ]
check "flows recorded (got ${FC:-0})" $?

CC=$(chunk_count); [ "${CC:-0}" -ge 2 ]
check "stream_chunks both directions (got ${CC:-0})" $?

# sni extracted from ClientHello
SNI=$(sql "SELECT sni FROM flows WHERE sni IS NOT NULL LIMIT 1;" 2>/dev/null)
[ "$SNI" = "www.google.com" ]
check "ClientHello SNI extracted ($SNI)" $?

# client_random is 32 bytes
CR_LEN=$(sql "SELECT length(client_random) FROM flows WHERE client_random IS NOT NULL LIMIT 1;" 2>/dev/null)
[ "${CR_LEN:-0}" = "32" ]
check "client_random is 32 bytes" $?

# byte accounting sanity (up+down >= payload seen by curl)
TOT=$(sql "SELECT bytes_up+bytes_down FROM flows ORDER BY id DESC LIMIT 1;" 2>/dev/null)
[ "${TOT:-0}" -ge "$BEFORE" ]
check "byte totals cover payload (${TOT:-0} >= ${BEFORE})" $?

# ---------------------------------------------------------------- T-B2 pcap export
echo "== T-B2: pcap export & ecosystem compat =="
FID=$(sql "SELECT id FROM flows ORDER BY id DESC LIMIT 1;" 2>/dev/null)
PCAP=/tmp/teamx-test-$$.pcap
KEYLOG=/tmp/teamx-test-$$.keylog
if have tshark; then
  $TEAMX capture export "$FID" --format pcap --with-keylog \
    -o "$PCAP" --keylog-out "$KEYLOG" >/dev/null 2>&1
  [ -s "$PCAP" ]
  check "pcap file generated" $?
  tshark -r "$PCAP" 2>/dev/null | grep -qi "tls\|tcp"
  check "tshark parses the pcap" $?
  # SNI visible in the ClientHello
  tshark -r "$PCAP" -Y "tls.handshake.extensions_server_name" -T fields \
    -e tls.handshake.extensions_server_name 2>/dev/null | grep -q google
  check "SNI visible in exported pcap" $?
else
  echo "SKIP: tshark not installed (pcap parse checks)"
fi

# ---------------------------------------------------------------- T-B3 keylog
echo "== T-B3: keylog association =="
# Simulate an app exporting keys: append a line matching our captured flow's
# client_random with a dummy secret — decrypt will fail on bad secret, but the
# 🔑 association logic must mark the flow as having keys.
if [ -n "${FID:-}" ] && [ -n "$(sql "SELECT client_random FROM flows WHERE id=$FID AND client_random IS NOT NULL;" 2>/dev/null)" ]; then
  CR=$(sql "SELECT lower(hex(client_random)) FROM flows WHERE id=$FID;")
  KL=/tmp/teamx-test-$$.sslkeys
  printf 'CLIENT_HANDSHAKE_TRAFFIC_SECRET %s 0000000000000000000000000000000000000000000000000000000000000000\n' "$CR" > "$KL"
  $TEAMX analyze import-keylog "$KL" >/dev/null 2>&1
  MATCH=$(sql "SELECT COUNT(*) FROM flows f JOIN tls_keys k ON f.client_random=k.client_random WHERE f.id=$FID;" 2>/dev/null)
  [ "${MATCH:-0}" -eq 1 ]
  check "client_random joins tls_keys (🔑 marker works)" $?
  rm -f "$KL"
else
  echo "SKIP: no client_random captured (no TLS flow)"
fi

# ---------------------------------------------------------------- T-B4 capacity
echo "== T-B4: capacity governance =="
MAXB=$(sql "SELECT COALESCE(MAX(seq),0) FROM stream_chunks;" 2>/dev/null)
# hammer: many small requests to grow the db, then verify prune keeps size sane
for i in 1 2 3 4 5 6; do
  curl -s -o /dev/null --max-time 10 --socks5-hostname 127.0.0.1:18081 "$TARGET" &
done; wait
sleep 1
$TEAMX capture prune --max-size 1MB >/dev/null 2>&1
DBSZ=$(stat -f%z "$CAPDB" 2>/dev/null || stat -c%s "$CAPDB" 2>/dev/null)
[ "${DBSZ:-999999999}" -le $(( 1024*1024*2 )) ]
check "prune keeps db near max-size (db=${DBSZ:-?}B)" $?

# dropped counter bounded under parallel load
DROP=$(sql "SELECT value FROM meta WHERE key='dropped';" 2>/dev/null || echo 0)
echo "  (dropped=$DROP — informational)"

# ---------------------------------------------------------------- T-B5 perf isolation
echo "== T-B5: perf isolation (capture on vs off) =="
measure() { # url -> total time via curl
  curl -s -o /dev/null -w '%{time_total}' --max-time 20 "$1"
}
OFF_T=$(curl -s -o /dev/null -w '%{time_total}' --max-time 20 "$TARGET")
ON_T=$(curl -s -o /dev/null -w '%{time_total}' --max-time 20 \
  --socks5-hostname 127.0.0.1:18081 "$TARGET")
echo "  direct=${OFF_T}s via-proxy(capture-on)=${ON_T}s"
# proxy path adds a full exit round-trip; just require it completes < 10s
python3 -c "import sys; sys.exit(0 if float('${ON_T:-99}') < 10 else 1)"
check "proxied request completes <10s with capture on (${ON_T}s)" $?

# ---------------------------------------------------------------- cleanup
kill $PROXY_PID 2>/dev/null
$TEAMX capture stop >/dev/null 2>&1
rm -f "$PCAP" "$KEYLOG"

echo ""
echo "== capture-test: $PASS passed, $FAIL failed =="
exit $([ $FAIL -eq 0 ] && echo 0 || echo 1)
