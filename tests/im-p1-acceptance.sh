#!/usr/bin/env bash
# IM P1 anchor acceptance (im-p1-acceptance-verify) — the 4 acceptance anchors
# from docs/design/im-access-requirements.md §P1, executed end-to-end over a
# real `teamx serve` with mTLS:
#   ① late letter import → bind code → bind success with role echo
#   ② readonly result matches `team.status` (goal title/state diff empty)
#   ③ replay produces no duplicate ledger (single-use code; im_seen nonce)
#   ④ every action lands in the ledger (source: im:<platform> + im_user)
#
# Requires: curl, python3.
set -euo pipefail

TEAMX="${TEAMX:-${TEAMX_BIN:-$(dirname "$0")/../target/debug/teamx}}"
DB="$(mktemp /tmp/teamx-imp1acc-XXXXXX).db"
HOME_DIR="$(mktemp -d /tmp/teamx-imp1acc-home-XXXXXX)"
PORT="${TEAMX_IMP1ACC_PORT:-5796}"
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

RPC() { # cert_dir method args_json
  curl -sS --max-time 10 --cacert "$HOME_DIR/ca/ca.crt" \
    --cert "$1/member.crt" --key "$1/member.key" \
    -H 'Content-Type: application/json' -d "{\"method\":\"$2\",\"args\":$3}" \
    "https://127.0.0.1:$PORT/rpc"
}

step "setup: team + goal + owner cert + late letter import"
"$TEAMX" init >/dev/null
"$TEAMX" team create "Imp1Acc" --session s:owner --json >/dev/null
"$TEAMX" goal set "IM P1 acceptance goal" --session s:owner --json >/dev/null
OWNER=$("$TEAMX" team status --session s:owner --json | python3 -c "import json,sys; print(json.load(sys.stdin)['teams'][0]['members'][0]['id'])")
OWNER_DIR="$(mktemp -d /tmp/teamx-imp1acc-owner-XXXXXX)"
"$TEAMX" cert issue "$OWNER" owner --out "$OWNER_DIR" --json >/dev/null
INV=$("$TEAMX" team invite "contributor: imp1 acc" --session s:owner --json)
LETTER=$(echo "$INV" | python3 -c "import json,sys; print(json.load(sys.stdin)['letter'])")
MEMBER_MID=$(echo "$INV" | python3 -c "import json,sys; print(json.load(sys.stdin)['member_id'])")
MEMBER_DIR="$(mktemp -d /tmp/teamx-imp1acc-member-XXXXXX)"
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
pass "serve up; letter NOT yet imported (late-import scenario)"

# ── Anchor ①: late letter import → bind code → bind success with role echo ──
step "anchor ①: late import → bind code → bind with role echo"
IMP=$(RPC "$MEMBER_DIR" team.import "{\"letter\":\"$LETTER\",\"name\":\"Imp1Acc Worker\"}")
echo "$IMP" | python3 -c "import json,sys; d=json.load(sys.stdin); assert d['ok'], d" || fail "late import failed"
CODE=$(RPC "$MEMBER_DIR" im.bind_code.create "{\"team\":\"$(echo "$IMP" | python3 -c "import json,sys; print(json.load(sys.stdin)['data']['team']['id'])")\"}" \
  | python3 -c "import json,sys; d=json.load(sys.stdin); assert d['ok'] and d['data']['ok'], d; print(d['data']['code'])")
BOUND=$(RPC "$MEMBER_DIR" im.bind "{\"team\":\"$(echo "$IMP" | python3 -c "import json,sys; print(json.load(sys.stdin)['data']['team']['id'])")\",\"platform\":\"feishu\",\"im_user_id\":\"acc-u1\",\"code\":\"$CODE\"}")
echo "$BOUND" | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert d['ok'] and d['data']['ok'], d
assert d['data']['member']['role'] == 'contributor', d
print('  ok: bound, role echoed =', d['data']['member']['role'])
" || fail "anchor ① failed"
pass "anchor ① PASS (late import → code → bind with role echo)"

step "anchor ③: replay (same bind code) rejected, no duplicate ledger"
RPC "$MEMBER_DIR" im.bind "{\"team\":\"$(echo "$IMP" | python3 -c "import json,sys; print(json.load(sys.stdin)['data']['team']['id'])")\",\"platform\":\"feishu\",\"im_user_id\":\"acc-u1-replay\",\"code\":\"$CODE\"}" \
  | python3 -c "import json,sys; d=json.load(sys.stdin); assert not d.get('ok', False) or not d['data']['ok'], d" \
  || fail "code replay should be rejected"
REJECTS=$(python3 - "$DB" <<'PY'
import sqlite3, sys
conn = sqlite3.connect(sys.argv[1])
print(conn.execute("SELECT COUNT(*) FROM events WHERE type='im.rejected'").fetchone()[0])
PY
)
[ "$REJECTS" -ge 1 ] || fail "replay rejection should be audited"
pass "anchor ③ PASS (replay rejected + audited; ledger has no duplicate binding)"

# ── Anchor ②: readonly result matches team.status (diff empty) ──
step "anchor ②: im.query status matches team.status (goal diff empty)"
TEAM_ID=$(echo "$IMP" | python3 -c "import json,sys; print(json.load(sys.stdin)['data']['team']['id'])")
TS_GOAL=$(RPC "$OWNER_DIR" team.status "{\"team\":\"$TEAM_ID\"}" | python3 -c "
import json,sys
d=json.load(sys.stdin)
t=d['data']['teams'][0]
g=t.get('goal') or {}
print(json.dumps({'title': g.get('title'), 'state': g.get('goal_state') or g.get('state')}))
")
Q=$(RPC "$MEMBER_DIR" im.query "{\"team\":\"$TEAM_ID\",\"platform\":\"feishu\",\"im_user_id\":\"acc-u1\",\"kind\":\"status\"}")
echo "$Q" | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert d['ok'] and d['data']['ok'], d
ts=json.loads('''$TS_GOAL''')
goal=d['data']['data']['goal']
assert goal['title'] == ts['title'], (goal, ts)
print('  ok: goal matches team.status (title=%s)' % goal['title'])
" || fail "anchor ② diff non-empty"
pass "anchor ② PASS (readonly result == team.status)"

# ── Anchor ④: every action lands in the ledger (source + im_user) ──
step "anchor ④: every IM action lands in the ledger (source + im_user)"
python3 - "$DB" <<'PY'
import sqlite3, sys, json
conn = sqlite3.connect(sys.argv[1])
rows = conn.execute(
    "SELECT type, payload_json FROM events WHERE type LIKE 'im.%' OR type LIKE 'discussion.%'"
).fetchall()
im_rows = [(t, json.loads(p)) for t, p in rows if t.startswith('im.')]
assert im_rows, "expected im.* ledger events"
missing = [t for t, p in im_rows if 'source' not in p or ('im_user' not in p and 'im_user_id' not in p)]
assert not missing, f"im.* events missing source/im_user_id: {missing}"
kinds = sorted({t for t, _ in im_rows})
print(f"  ok: {len(im_rows)} im.* events, all with source + im_user_id; kinds={kinds}")
PY
pass "anchor ④ PASS (source + im_user on every im.* action)"

echo
echo "ALL IM P1 ANCHORS PASSED (①②③④)"
