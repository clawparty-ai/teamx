#!/usr/bin/env bash
# Member workspace reporting test (network mode).
#
# Boots a real `teamx serve`, owner + one member (alice) via mTLS, then checks
# that a member's workspace directory is captured and surfaced in
# `team status` (the same JSON the plugin tool and the console read):
#   - any RPC carrying `workspace` records it on the member
#   - `presence.push` (plugin path) records it too
#   - the workspace is persisted (survives a fresh status read, offline-safe)
#   - a member that never reported a workspace stays null (no phantom paths)
#   - the CLI human output prints `[workspace: ...]`
#
# Requires: curl, openssl, python3.
set -euo pipefail

TEAMX="${TEAMX:-$(dirname "$0")/../target/debug/teamx}"
DB="$(mktemp /tmp/teamx-ws-XXXXXX).db"
HOME_DIR="$(mktemp -d /tmp/teamx-ws-home-XXXXXX)"
PORT="${TEAMX_WS_TEST_PORT:-5794}"
export TEAMX_DB="$DB"
export TEAMX_HOME="$HOME_DIR"

pass() { printf '  ok: %s\n' "$*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

cleanup() {
  [ -n "${SERVE_PID:-}" ] && kill "$SERVE_PID" 2>/dev/null || true
  rm -f "$DB" "$DB-wal" "$DB-shm"
  rm -rf "$HOME_DIR"
}
trap cleanup EXIT

if ! command -v curl >/dev/null 2>&1 || ! command -v openssl >/dev/null 2>&1; then
  echo "skipped: curl/openssl not available"; exit 0
fi
step() { printf '\n=== %s ===\n' "$*"; }

step "setup: team + owner cert + member invitation"
"$TEAMX" init >/dev/null
"$TEAMX" team create "WsTeam" --session s:owner --json >/dev/null
OWNER=$("$TEAMX" team status --session s:owner --json | python3 -c "import json,sys; print(json.load(sys.stdin)['teams'][0]['members'][0]['id'])")
OWNER_CERT="$(mktemp -d /tmp/teamx-ws-owner-XXXXXX)"
"$TEAMX" cert issue "$OWNER" owner --out "$OWNER_CERT" --json >/dev/null
INV=$("$TEAMX" team invite "contributor: does work" --session s:owner --json)
LETTER=$(echo "$INV" | python3 -c "import json,sys; print(json.load(sys.stdin)['letter'])")
ALICE_MID=$(echo "$INV" | python3 -c "import json,sys; print(json.load(sys.stdin)['member_id'])")
ALICE_DIR="$(mktemp -d /tmp/teamx-ws-alice-XXXXXX)"
echo "$LETTER" | python3 -c "
import json,sys,base64
s=sys.stdin.read().strip()[len('teamx-inv:v1:'):]
d=json.loads(base64.b64decode(s))
c=d['certificates']
open('$ALICE_DIR/member.crt','w').write(c['client_cert'])
open('$ALICE_DIR/member.key','w').write(c['client_key'])
"

step "start serve"
"$TEAMX" serve --addr 127.0.0.1 --port "$PORT" >/dev/null 2>&1 &
SERVE_PID=$!
for _ in $(seq 1 30); do
  if curl -sS --max-time 2 --cacert "$HOME_DIR/ca/ca.crt" --cert "$OWNER_CERT/member.crt" --key "$OWNER_CERT/member.key" "https://127.0.0.1:$PORT/health" >/dev/null 2>&1; then break; fi
  sleep 0.2
done
pass "serve is up"

CA="$HOME_DIR/ca/ca.crt"
TEAM_ID=$("$TEAMX" team status --session s:owner --json | python3 -c "import json,sys; print(json.load(sys.stdin)['teams'][0]['team']['id'])")

RPC() { # cert_dir method args
  curl -sS --max-time 10 --cacert "$CA" --cert "$1/member.crt" --key "$1/member.key" \
    -H 'Content-Type: application/json' -d "{\"method\":\"$2\",\"args\":$3}" \
    "https://127.0.0.1:$PORT/rpc"
}
ws_of() { # cert_dir member_id -> workspace (or null)
  RPC "$1" team.status "{\"team\":\"$TEAM_ID\"}" | python3 -c "
import json,sys
d=json.load(sys.stdin)
ms=d['data']['teams'][0]['members']
m=[x for x in ms if x['id']=='$2']
print(m[0]['workspace'] if m and m[0].get('workspace') is not None else 'null')
"
}

step "alice imports the letter, owner approves (member active)"
IMPORT=$(RPC "$ALICE_DIR" team.import "{\"letter\":\"$LETTER\",\"name\":\"Alice\"}")
echo "$IMPORT" | python3 -c "import json,sys; d=json.load(sys.stdin); assert d.get('ok') is True, d" || fail "import failed"
APPROVE=$(RPC "$OWNER_CERT" team.approve "{\"member_id\":\"$ALICE_MID\"}")
echo "$APPROVE" | python3 -c "import json,sys; d=json.load(sys.stdin); assert d.get('ok') is True, d" || fail "approve failed"
pass "imported + approved"

step "T-W1: a member that never reported a workspace is null"
GOT=$(ws_of "$OWNER_CERT" "$ALICE_MID")
[ "$GOT" = "null" ] || fail "expected null workspace, got $GOT"
pass "unreported member -> null"

step "T-W2: any RPC carrying workspace records it on the member"
RPC "$ALICE_DIR" member.set_state "{\"state\":\"active\",\"workspace\":\"/home/alice/work/proj\"}" >/dev/null
GOT=$(ws_of "$OWNER_CERT" "$ALICE_MID")
[ "$GOT" = "/home/alice/work/proj" ] || fail "expected /home/alice/work/proj, got $GOT"
pass "workspace recorded from a plain RPC"

step "T-W3: presence.push (plugin path) records it too"
RPC "$ALICE_DIR" presence.push "{\"data\":\"{\\\"active_sessions\\\":1,\\\"running_tools\\\":0,\\\"open_terminals\\\":0,\\\"workspace\\\":\\\"/home/alice/work/proj2\\\"}\"}" >/dev/null
GOT=$(ws_of "$OWNER_CERT" "$ALICE_MID")
[ "$GOT" = "/home/alice/work/proj2" ] || fail "expected /home/alice/work/proj2, got $GOT"
pass "workspace recorded from presence.push"

step "T-W4: an empty workspace never overwrites the recorded one"
RPC "$ALICE_DIR" member.set_state "{\"state\":\"active\",\"workspace\":\"   \"}" >/dev/null
GOT=$(ws_of "$OWNER_CERT" "$ALICE_MID")
[ "$GOT" = "/home/alice/work/proj2" ] || fail "blank workspace clobbered the value: $GOT"
pass "blank value ignored"

step "T-W5: CLI human output prints [workspace: ...]"
OUT=$("$TEAMX" team status --session s:owner)
echo "$OUT" | grep -q "\[workspace: /home/alice/work/proj2\]" \
  || fail "human output missing workspace marker:\n$OUT"
pass "human output shows the workspace"

step "T-W6: JSON output exposes the workspace field"
JSON=$("$TEAMX" team status --session s:owner --json)
echo "$JSON" | python3 -c "
import json,sys
d=json.load(sys.stdin)
ms=d['teams'][0]['members']
m=[x for x in ms if x['id']=='$ALICE_MID'][0]
assert m['workspace']=='/home/alice/work/proj2', m
" || fail "JSON status missing workspace"
pass "JSON output exposes workspace"

printf '\nALL PASSED\n'
