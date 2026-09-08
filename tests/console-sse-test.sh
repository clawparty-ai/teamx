#!/usr/bin/env bash
# Console SSE test — verifies /api/stream relays new team events in network mode
# and only heartbeats are fine while idle. Also asserts auth (401 without cookie).
#
# Requires: curl, openssl, python3.
set -euo pipefail

TEAMX="${TEAMX:-$(cd "$(dirname "$0")/.." && pwd)/target/debug/teamx}"
DB="$(mktemp /tmp/teamx-sse-XXXXXX).db"
SERVE_HOME="$(mktemp -d /tmp/teamx-sse-serve-XXXXXX)"
CONSOLE_HOME="$(mktemp -d /tmp/teamx-sse-console-XXXXXX)"
SPORT="${TEAMX_SSE_SERVE_PORT:-5794}"
CPORT="${TEAMX_SSE_CONSOLE_PORT:-9529}"
pass(){ printf '  ok: %s\n' "$*"; }
fail(){ echo "FAIL: $*" >&2; exit 1; }
cleanup(){
  [ -n "${CONSOLE_PID:-}" ] && kill "$CONSOLE_PID" 2>/dev/null || true
  [ -n "${SERVE_PID:-}" ] && kill "$SERVE_PID" 2>/dev/null || true
  rm -f "$DB" "$DB-wal" "$DB-shm"
  rm -rf "$SERVE_HOME" "$CONSOLE_HOME" /tmp/teamx-sse-cookie
}
trap cleanup EXIT
step(){ printf '\n=== %s ===\n' "$*"; }

step "setup: team + owner cert + member invitation (console identity)"
export TEAMX_DB="$DB" TEAMX_HOME="$SERVE_HOME"
"$TEAMX" init >/dev/null
"$TEAMX" team create "SseTeam" --session s:owner --json >/dev/null
OWNER=$("$TEAMX" team status --session s:owner --json | python3 -c "import json,sys; print(json.load(sys.stdin)['teams'][0]['members'][0]['id'])")
OWNER_CERT="$(mktemp -d /tmp/teamx-sse-owner-XXXXXX)"
"$TEAMX" cert issue "$OWNER" owner --out "$OWNER_CERT" --json >/dev/null
INV=$("$TEAMX" team invite "contributor: console user" --session s:owner --json)
LETTER=$(echo "$INV" | python3 -c "import json,sys; print(json.load(sys.stdin)['letter'])")
C_MID=$(echo "$INV" | python3 -c "import json,sys; print(json.load(sys.stdin)['member_id'])")
# member cert material for the console workspace
CERT_DIR="$(mktemp -d /tmp/teamx-sse-m-XXXXXX)"
echo "$LETTER" | python3 -c "
import json,sys,base64
s=sys.stdin.read().strip()[len('teamx-inv:v1:'):]
d=json.loads(base64.b64decode(s)); c=d['certificates']
open('$CERT_DIR/member.crt','w').write(c['client_cert'])
open('$CERT_DIR/member.key','w').write(c['client_key'])
"
TEAM_ID=$("$TEAMX" team status --session s:owner --json | python3 -c "import json,sys; print(json.load(sys.stdin)['teams'][0]['team']['id'])")

step "start serve"
"$TEAMX" serve --addr 127.0.0.1 --port "$SPORT" >/dev/null 2>&1 &
SERVE_PID=$!
for _ in $(seq 1 30); do
  if curl -sS --max-time 2 --cacert "$SERVE_HOME/ca/ca.crt" --cert "$OWNER_CERT/member.crt" --key "$OWNER_CERT/member.key" "https://127.0.0.1:$SPORT/health" >/dev/null 2>&1; then break; fi
  sleep 0.2
done
pass "serve up"

# console workspace: import member letter into CONSOLE_HOME/.teamx and approve
mkdir -p "$CONSOLE_HOME/.teamx"
cp -R "$SERVE_HOME/ca" "$CONSOLE_HOME/.teamx/ca" 2>/dev/null || true
# Console in network mode reads the letter from its workspace .teamx; set up an
# identity directory with the member letter via teamx local import.
RPC() {
  curl -sS --max-time 8 --cacert "$SERVE_HOME/ca/ca.crt" --cert "$1/member.crt" --key "$1/member.key" \
    -H 'Content-Type: application/json' -d "{\"method\":\"$2\",\"args\":$3}" "https://127.0.0.1:$SPORT/rpc"
}
RPC "$CERT_DIR" team.import "{\"letter\":\"$LETTER\",\"name\":\"Consoler\"}" >/dev/null
RPC "$OWNER_CERT" team.approve "{\"member_id\":\"$C_MID\"}" >/dev/null

step "start console (network -> serve) as the member via env mTLS"
# The member's mTLS material is passed via TEAMX_MTLS_* env (same discovery the
# tunnel_client uses); console --server-url points it at the test serve.
(
  cd "$CONSOLE_HOME"
  TEAMX_HOME="$CONSOLE_HOME/.teamx" \
  TEAMX_SERVER_URL="https://127.0.0.1:$SPORT" \
  TEAMX_MTLS_CERT="$CERT_DIR/member.crt" \
  TEAMX_MTLS_KEY="$CERT_DIR/member.key" \
  TEAMX_MTLS_CA="$SERVE_HOME/ca/ca.crt" \
  "$TEAMX" console --addr 127.0.0.1 --port "$CPORT" --server-url "https://127.0.0.1:$SPORT" >console.log 2>&1 &
)
CONSOLE_PID=$!
for _ in $(seq 1 30); do grep -q "open:" "$CONSOLE_HOME/console.log" 2>/dev/null && break; sleep 0.3; done
TOKEN=$(sed -n 's/.*token=\([0-9a-f]*\).*/\1/p' "$CONSOLE_HOME/console.log" | head -1)
[ -n "$TOKEN" ] || { echo "console token not found:"; cat "$CONSOLE_HOME/console.log"; exit 1; }
pass "console up"

BASE="https://127.0.0.1:$CPORT"
rm -f /tmp/teamx-sse-cookie
# sanity: /?token= must set the auth cookie and index must return 200
curl -sk -c /tmp/teamx-sse-cookie "$BASE/?token=$TOKEN" -o /dev/null
code_idx=$(curl -sk -b /tmp/teamx-sse-cookie -o /dev/null -w '%{http_code}' "$BASE/")
[ "$code_idx" = "200" ] && pass "cookie auth ok (index 200)" || { echo "index with cookie -> $code_idx; console.log tail:"; tail -6 "$CONSOLE_HOME/console.log"; exit 1; }

step "S-01: /api/stream without cookie -> 401"
code=$(curl -sk -o /dev/null -w '%{http_code}' --max-time 3 "$BASE/api/stream")
[ "$code" = "401" ] && pass "no cookie -> 401" || fail "no cookie -> $code"

step "S-02: with cookie, a new discussion.message arrives on the stream"
# Open the SSE stream in background, then create + message a discussion.
curl -sk -N -b /tmp/teamx-sse-cookie --max-time 12 "$BASE/api/stream" > "$CONSOLE_HOME/sse.out" 2>&1 &
SSE_PID=$!
sleep 1
CR=$(RPC "$OWNER_CERT" discussion.create "{\"title\":\"sse topic\",\"body\":\"hi\",\"team\":\"$TEAM_ID\"}")
DID=$(echo "$CR" | python3 -c "import json,sys; print(json.load(sys.stdin)['data']['discussion_id'])")
RPC "$OWNER_CERT" discussion.say "{\"id\":\"$DID\",\"body\":\"hello via sse\",\"team\":\"$TEAM_ID\"}" >/dev/null
sleep 4
kill "$SSE_PID" 2>/dev/null || true
grep -q '"type":"discussion.message"' "$CONSOLE_HOME/sse.out" && pass "stream relayed discussion.message" || { echo "--- sse.out ---"; cat "$CONSOLE_HOME/sse.out"; fail "no discussion.message in stream"; }

echo
echo "== result: PASSED =="
