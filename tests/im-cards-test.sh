#!/usr/bin/env bash
# IM P2 interactive cards (im-p2-feishu-interactive-cards) — E2E.
#
# Real `teamx serve` + mTLS, member path (cert identity):
#   owner requests a confirm token for im.verify on a done task
#   → token issued + im.card.requested audited
#   → im.card.execute with the token flips the task to verified
#   → token consumed (replay refused)
#   → non-lead request refused; unknown action refused
#   → ledger: im.card.requested + im.verify all with source + im_user
#
# Requires: curl, python3.
set -euo pipefail

TEAMX="${TEAMX:-${TEAMX_BIN:-$(dirname "$0")/../target/debug/teamx}}"
DB="$(mktemp /tmp/teamx-imcards-XXXXXX).db"
HOME_DIR="$(mktemp -d /tmp/teamx-imcards-home-XXXXXX)"
PORT="${TEAMX_IMCARDS_PORT:-5799}"
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

rpc() { # cert_dir method args_json
  curl -sS --max-time 10 --cacert "$HOME_DIR/ca/ca.crt" --cert "$1/member.crt" \
    --key "$1/member.key" -X POST "https://127.0.0.1:$PORT/rpc" \
    -H 'Content-Type: application/json' -d "{\"method\":\"$2\",\"args\":$3}"
}

step "setup: team + owner/worker certs + done task"
"$TEAMX" init >/dev/null
"$TEAMX" team create "ImCards" --session s:owner --json >/dev/null
OWNER=$("$TEAMX" team status --session s:owner --json | python3 -c "import json,sys; print(json.load(sys.stdin)['teams'][0]['members'][0]['id'])")
OWNER_DIR="$(mktemp -d /tmp/teamx-imcards-owner-XXXXXX)"
"$TEAMX" cert issue "$OWNER" owner --out "$OWNER_DIR" --json >/dev/null
INV=$("$TEAMX" team invite "contributor: cards" --session s:owner --json)
LETTER=$(echo "$INV" | python3 -c "import json,sys; print(json.load(sys.stdin)['letter'])")
MEMBER_MID=$(echo "$INV" | python3 -c "import json,sys; print(json.load(sys.stdin)['member_id'])")
MEMBER_DIR="$(mktemp -d /tmp/teamx-imcards-member-XXXXXX)"
echo "$LETTER" | python3 -c "
import json,sys,base64
s=sys.stdin.read().strip()[len('teamx-inv:v1:'):]
d=json.loads(base64.b64decode(s))
c=d['certificates']
open('$MEMBER_DIR/member.crt','w').write(c['client_cert'])
open('$MEMBER_DIR/member.key','w').write(c['client_key'])
"
"$TEAMX" serve --addr 127.0.0.1 --port "$PORT" >/dev/null 2>&1 &
SERVE_PID=$!
for _ in $(seq 1 30); do
  curl -sS --max-time 2 --cacert "$HOME_DIR/ca/ca.crt" --cert "$OWNER_DIR/member.crt" \
    --key "$OWNER_DIR/member.key" "https://127.0.0.1:$PORT/health" >/dev/null 2>&1 && break
  sleep 0.2
done
rpc "$MEMBER_DIR" team.import "{\"letter\":\"$LETTER\",\"name\":\"Cards Worker\"}" >/dev/null
TEAM_ID=$(rpc "$OWNER_DIR" team.status "{}" | python3 -c "import json,sys; print(json.load(sys.stdin)['data']['teams'][0]['team']['id'])")
rpc "$OWNER_DIR" task.create "{\"title\":\"Ship it\",\"assignee\":\"$MEMBER_MID\"}" >/dev/null
TASK_ID=$(rpc "$OWNER_DIR" task.list "{}" | python3 -c "import json,sys; print(json.load(sys.stdin)['data']['tasks'][0]['id'])")
rpc "$MEMBER_DIR" task.done "{\"id\":\"$TASK_ID\"}" >/dev/null
pass "team=$TEAM_ID task=$TASK_ID state=done"

step "card.request: owner requests confirm token for im.verify"
OUT=$(rpc "$OWNER_DIR" im.card.request "{\"team\":\"$TEAM_ID\",\"platform\":\"feishu\",\"im_user_id\":\"u-cards\",\"action\":\"im.verify\",\"task_id\":\"$TASK_ID\",\"im_msg_id\":\"cb-1\"}" 2>/dev/null || true)
# The RPC path uses the cert member identity; platform/im_user_id are used for
# the audit trail — request works even without a binding for cert callers.
OUT=$(rpc "$OWNER_DIR" im.card.request "{\"team\":\"$TEAM_ID\",\"action\":\"im.verify\",\"task_id\":\"$TASK_ID\",\"im_msg_id\":\"cb-1\"}")
TOKEN=$(echo "$OUT" | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert d['ok'], d
t=d['data']['confirm_token']
assert t.startswith('ct-'), t
assert d['data']['card']['elements'][1]['actions'][0]['value']['confirm_token'] == t, d
print(t)
")
pass "confirm token issued: $TOKEN"

step "card.requested audited"
python3 - "$DB" <<'PY'
import sqlite3, sys, json
conn = sqlite3.connect(sys.argv[1])
rows = [json.loads(p) for t, p in
        conn.execute("SELECT type, payload_json FROM events WHERE type='im.card.requested'").fetchall()]
assert rows, "expected im.card.requested audit"
assert all(r.get('source') and r.get('im_user') and r.get('confirm_token') for r in rows), rows
print(f"  ok: {len(rows)} im.card.requested events with source+im_user+confirm_token")
PY
pass "card.request audit verified"

step "card.execute: token flips the task to verified"
rpc "$OWNER_DIR" im.card.execute "{\"team\":\"$TEAM_ID\",\"action\":\"im.verify\",\"task_id\":\"$TASK_ID\",\"confirm_token\":\"$TOKEN\"}" \
  | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert d['ok'], d
assert d['data']['result']['task']['state'] == 'verified', d
print('  ok: task state =', d['data']['result']['task']['state'])
" || fail "card.execute failed"

step "replay: same token refused (consumed)"
rpc "$OWNER_DIR" im.card.execute "{\"team\":\"$TEAM_ID\",\"action\":\"im.verify\",\"task_id\":\"$TASK_ID\",\"confirm_token\":\"$TOKEN\"}" \
  | python3 -c "import json,sys; d=json.load(sys.stdin); assert not d.get('ok', False), d" \
  || fail "replay must be refused"
pass "replay refused (token consumed)"

step "non-lead card.request refused"
rpc "$MEMBER_DIR" im.card.request "{\"team\":\"$TEAM_ID\",\"action\":\"im.verify\",\"task_id\":\"$TASK_ID\",\"im_msg_id\":\"cb-2\"}" \
  | python3 -c "import json,sys; d=json.load(sys.stdin); assert not d.get('ok', False), d" \
  || fail "non-lead request must be refused"
pass "non-lead refused (min_role=lead)"

step "unknown action refused"
rpc "$OWNER_DIR" im.card.request "{\"team\":\"$TEAM_ID\",\"action\":\"im.destroy\",\"task_id\":\"$TASK_ID\"}" \
  | python3 -c "import json,sys; d=json.load(sys.stdin); assert not d.get('ok', False), d" \
  || fail "unknown action must be refused"
pass "unknown action refused (fail-closed)"

step "ledger: im.card.requested + im.verify with source + im_user"
python3 - "$DB" <<'PY'
import sqlite3, sys, json
conn = sqlite3.connect(sys.argv[1])
rows = [(t, json.loads(p)) for t, p in
        conn.execute("SELECT type, payload_json FROM events WHERE type LIKE 'im.%'").fetchall()]
kinds = sorted({t for t, _ in rows})
for want in ['im.card.requested', 'im.accepted']:
    assert want in kinds, f"missing {want} in {kinds}"
bad = [t for t, p in rows if 'source' not in p or 'im_user' not in p]
assert not bad, f"im.* events missing source/im_user: {bad}"
print(f"  ok: {len(rows)} im.* events, kinds={kinds}, all with source+im_user")
PY
pass "audit trail verified"

echo
echo "ALL IM CARDS TESTS PASSED"
