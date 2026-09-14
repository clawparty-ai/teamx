#!/usr/bin/env bash
# IM read-only query (P1) — RPC acceptance over mTLS (network-mode teams).
#
# Boots a real `teamx serve`, binds an IM identity, then drives `im.query`
# exactly as the gateway would:
#   unbound identity rejected → status (goal + self counts) → tasks (self only)
#   → questions (only targeting the caller) → discussions (team-shared)
#   → unknown kind rejected → ledger carries im.query with source=im:feishu
#
# Requires: curl, python3.
set -euo pipefail

TEAMX="${TEAMX:-${TEAMX_BIN:-$(dirname "$0")/../target/debug/teamx}}"
DB="$(mktemp /tmp/teamx-imquery-XXXXXX).db"
HOME_DIR="$(mktemp -d /tmp/teamx-imquery-home-XXXXXX)"
PORT="${TEAMX_IM_QUERY_TEST_PORT:-5795}"
export TEAMX_DB="$DB"
export TEAMX_HOME="$HOME_DIR"

pass() { printf '  ok: %s\n' "$*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }
step() { printf '\n=== %s ===\n' "$*"; }

cleanup() {
  [ -n "${SERVE_PID:-}" ] && kill "$SERVE_PID" 2>/dev/null || true
  rm -f "$DB" "$DB-wal" "$DB-shm"
  rm -rf "$HOME_DIR"
}
trap cleanup EXIT

if ! command -v curl >/dev/null 2>&1 || ! command -v python3 >/dev/null 2>&1; then
  echo "skipped: curl/python3 not available"; exit 0
fi

# Owner-side RPC (mTLS cert). args come from stdin as JSON.
rpc_owner() { # method args_json
  curl -sS --max-time 10 --cacert "$HOME_DIR/ca/ca.crt" \
    --cert "$OWNER_DIR/member.crt" --key "$OWNER_DIR/member.key" \
    -X POST "https://127.0.0.1:$PORT/rpc" \
    -H 'Content-Type: application/json' \
    -d "{\"method\":\"$1\",\"args\":$2}"
}
# Member-side RPC (mTLS cert) — how the in-process gateway calls im.* in P1.
rpc_member() { # method args_json
  curl -sS --max-time 10 --cacert "$HOME_DIR/ca/ca.crt" \
    --cert "$MEMBER_DIR/member.crt" --key "$MEMBER_DIR/member.key" \
    -X POST "https://127.0.0.1:$PORT/rpc" \
    -H 'Content-Type: application/json' \
    -d "{\"method\":\"$1\",\"args\":$2}"
}

step "setup: team + owner cert + member letter"
"$TEAMX" init >/dev/null
"$TEAMX" team create "IMQueryTeam" --session s:owner --json >/dev/null
"$TEAMX" goal set "IM readonly over RPC" --session s:owner >/dev/null
OWNER=$("$TEAMX" team status --session s:owner --json | python3 -c "import json,sys; print(json.load(sys.stdin)['teams'][0]['members'][0]['id'])")
OWNER_DIR="$(mktemp -d /tmp/teamx-imquery-owner-XXXXXX)"
"$TEAMX" cert issue "$OWNER" owner --out "$OWNER_DIR" --json >/dev/null
INV=$("$TEAMX" team invite "contributor: queries over IM" --session s:owner --json)
LETTER=$(echo "$INV" | python3 -c "import json,sys; print(json.load(sys.stdin)['letter'])")
MEMBER_MID=$(echo "$INV" | python3 -c "import json,sys; print(json.load(sys.stdin)['member_id'])")
MEMBER_DIR="$(mktemp -d /tmp/teamx-imquery-member-XXXXXX)"
echo "$LETTER" | python3 -c "
import json,sys,base64
s=sys.stdin.read().strip()[len('teamx-inv:v1:'):]
d=json.loads(base64.b64decode(s))
c=d['certificates']
open('$MEMBER_DIR/member.crt','w').write(c['client_cert'])
open('$MEMBER_DIR/member.key','w').write(c['client_key'])
"
# Import the letter (creates the member row, pending) then approve (active —
# the `ask` fixture requires an active target).
"$TEAMX" team import "$LETTER" --session s:imp --json >/dev/null
"$TEAMX" team approve "$MEMBER_MID" --session s:owner --json >/dev/null
pass "owner=$OWNER member=$MEMBER_MID"

step "start serve (mTLS)"
"$TEAMX" serve --addr 127.0.0.1 --port "$PORT" >/dev/null 2>&1 &
SERVE_PID=$!
for _ in $(seq 1 30); do
  curl -sS --max-time 2 --cacert "$HOME_DIR/ca/ca.crt" --cert "$OWNER_DIR/member.crt" \
    --key "$OWNER_DIR/member.key" "https://127.0.0.1:$PORT/health" >/dev/null 2>&1 && break
  sleep 0.2
done
pass "serve is up on https://127.0.0.1:$PORT"

step "fixture: a task assigned to the member + an open question (task.* RPC writes the server taskx table)"
rpc_owner "task.create" "{\"title\":\"member task\",\"assignee\":\"$MEMBER_MID\"}" >/dev/null
rpc_owner "ask" "{\"member_id\":\"$MEMBER_MID\",\"question\":\"status please?\"}" >/dev/null
pass "fixtures in place"

step "bind the IM identity (member side: create code + redeem)"
CODE=$(rpc_member "im.bind_code.create" "{}" | python3 -c "import json,sys; print(json.load(sys.stdin)['data']['code'])")
rpc_member "im.bind" "{\"platform\":\"feishu\",\"im_user_id\":\"u-query\",\"code\":\"$CODE\"}" \
  | python3 -c "import json,sys; d=json.load(sys.stdin); assert d['ok'] and d['data']['ok'], d; print('  ok: bound role', d['data']['member']['role'])"

step "unbound identity rejected"
OUT=$(rpc_member "im.query" "{\"platform\":\"feishu\",\"im_user_id\":\"u-nobody\",\"kind\":\"status\"}")
echo "$OUT" | python3 -c "import json,sys; d=json.load(sys.stdin); assert not d.get('ok', False), d" \
  || fail "unbound identity should be rejected"
pass "im.query without binding is refused"

step "kind=status (goal + self counts)"
OUT=$(rpc_member "im.query" "{\"platform\":\"feishu\",\"im_user_id\":\"u-query\",\"kind\":\"status\"}")
echo "$OUT" | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert d['ok'] and d['data']['ok'], d
data=d['data']['data']
assert data['goal']['state'], data
assert data['my_open_questions'] == 1, data
print('  ok: status goal=%s my_open_tasks=%s' % (data['goal']['state'], data['my_open_tasks']))
" || fail "status query failed"

step "kind=tasks (only the caller's assignments)"
OUT=$(rpc_member "im.query" "{\"platform\":\"feishu\",\"im_user_id\":\"u-query\",\"kind\":\"tasks\"}")
echo "$OUT" | python3 -c "
import json,sys
d=json.load(sys.stdin)
tasks=d['data']['data']['tasks']
assert [t['title'] for t in tasks] == ['member task'], tasks
print('  ok: tasks scoped to caller:', [t['id'] for t in tasks])
" || fail "tasks query failed"

step "kind=questions (only those targeting the caller)"
OUT=$(rpc_member "im.query" "{\"platform\":\"feishu\",\"im_user_id\":\"u-query\",\"kind\":\"questions\"}")
echo "$OUT" | python3 -c "
import json,sys
d=json.load(sys.stdin)
qs=d['data']['data']['questions']
assert len(qs) == 1 and qs[0]['asked_by'], qs
print('  ok: questions scoped, asked_by=%s' % qs[0]['asked_by'])
" || fail "questions query failed"

step "kind=discussions (team-shared, empty is fine)"
OUT=$(rpc_member "im.query" "{\"platform\":\"feishu\",\"im_user_id\":\"u-query\",\"kind\":\"discussions\"}")
echo "$OUT" | python3 -c "import json,sys; d=json.load(sys.stdin); assert d['ok'] and d['data']['ok'], d" \
  || fail "discussions query failed"
pass "discussions query OK"

step "unknown kind rejected"
OUT=$(rpc_member "im.query" "{\"platform\":\"feishu\",\"im_user_id\":\"u-query\",\"kind\":\"secrets\"}")
echo "$OUT" | python3 -c "import json,sys; d=json.load(sys.stdin); assert not d.get('ok', False), d" \
  || fail "unknown kind should be rejected"
pass "unknown kind refused"

step "ledger carries im.query with source=im:feishu"
python3 - "$DB" <<'PY'
import sqlite3, sys
conn = sqlite3.connect(sys.argv[1])
rows = conn.execute("SELECT payload_json FROM events WHERE type='im.query'").fetchall()
assert rows, "expected im.query audit events"
sources = [__import__('json').loads(r[0])['source'] for r in rows]
assert all(s == 'im:feishu' for s in sources), sources
print(f"  ok: {len(rows)} im.query audit events, all source=im:feishu")
PY
pass "audit trail verified"

echo
echo "ALL IM-QUERY TESTS PASSED"
