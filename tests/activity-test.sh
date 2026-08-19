#!/usr/bin/env bash
# teamx enterprise activity analytics — network mode test (A1/A5).
# Verifies activity.push via RPC (mTLS cert identity attribution), owner
# full-read, and member self-scoped read.
#
# Requires: curl, openssl, python3.
set -euo pipefail

TEAMX="${TEAMX:-$(dirname "$0")/../target/debug/teamx}"
DB="$(mktemp /tmp/teamx-act-XXXXXX).db"
HOME_DIR="$(mktemp -d /tmp/teamx-act-home-XXXXXX)"
PORT="${TEAMX_TEST_PORT:-5789}"
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
  echo "skipped: curl/openssl not available"
  exit 0
fi

step() { printf '\n=== %s ===\n' "$*"; }

step "setup: team + owner cert + member invitation"
"$TEAMX" init
"$TEAMX" team create "Act" --session s:owner --json >/dev/null
OWNER=$("$TEAMX" team status --session s:owner --json | python3 -c "import json,sys; print(json.load(sys.stdin)['teams'][0]['members'][0]['id'])")
OWNER_CERT="$(mktemp -d /tmp/teamx-act-owner-XXXXXX)"
"$TEAMX" cert issue "$OWNER" owner --out "$OWNER_CERT" --json >/dev/null
INV=$("$TEAMX" team invite "contributor: does work" --session s:owner --json)
LETTER=$(echo "$INV" | python3 -c "import json,sys; print(json.load(sys.stdin)['letter'])")
ALICE_MID=$(echo "$INV" | python3 -c "import json,sys; print(json.load(sys.stdin)['member_id'])")
ALICE_DIR="$(mktemp -d /tmp/teamx-act-alice-XXXXXX)"
echo "$LETTER" | python3 -c "
import json,sys,base64
s=sys.stdin.read().strip()[len('teamx-inv:v1:'):]
d=json.loads(base64.b64decode(s))
c=d['certificates']
open('$ALICE_DIR/client.crt','w').write(c['client_cert'])
open('$ALICE_DIR/client.key','w').write(c['client_key'])
open('$ALICE_DIR/ca.crt','w').write(c['ca_cert'])
"

step "start serve"
"$TEAMX" serve --addr 127.0.0.1 --port "$PORT" >/dev/null 2>&1 &
SERVE_PID=$!
for _ in $(seq 1 30); do
  if curl -sS --max-time 2 --cacert "$HOME_DIR/ca/ca.crt" --cert "$OWNER_CERT/member.crt" --key "$OWNER_CERT/member.key" "https://127.0.0.1:$PORT/health" >/dev/null 2>&1; then
    break
  fi
  sleep 0.2
done
pass "serve is up"

TEAM_ID=$("$TEAMX" team status --session s:owner --json | python3 -c "import json,sys; print(json.load(sys.stdin)['teams'][0]['team']['id'])")

RPC() { # cert_dir method args
  curl -sS --max-time 10 --cacert "$1/ca.crt" --cert "$1/client.crt" --key "$1/client.key" \
    -H 'Content-Type: application/json' -d "{\"method\":\"$2\",\"args\":$3}" \
    "https://127.0.0.1:$PORT/rpc"
}

step "alice imports the letter, then pushes her own activity"
IMPORT=$(RPC "$ALICE_DIR" team.import "{\"letter\":\"$LETTER\",\"name\":\"Alice\"}")
echo "$IMPORT" | python3 -c "import json,sys; d=json.load(sys.stdin); assert d.get('ok') is True, d" || fail "import failed"
pass "imported"

PUSH=$(RPC "$ALICE_DIR" activity.push "{\"rows\":[
  {\"node_id\":\"node-alice\",\"node_name\":\"alice-mbp\",\"started_at\":\"2026-08-19T10:00:00Z\",\"kind\":\"tool_call\",\"detail\":{\"tool\":\"bash\",\"arguments\":{\"command\":\"ls\"}}},
  {\"node_id\":\"node-alice\",\"started_at\":\"2026-08-19T11:00:00Z\",\"kind\":\"human_input\",\"detail\":{\"sessionID\":\"s1\",\"text\":\"hi\"}}
]}")
echo "$PUSH" | python3 -c "import json,sys; d=json.load(sys.stdin); assert d.get('ok') is True and d['data']['inserted']==2, d" || fail "push failed"
pass "alice pushed 2 rows (member_id from cert)"

step "alice's row is attributed to her cert identity (not self-claimed)"
PUSH2=$(RPC "$ALICE_DIR" activity.push "{\"rows\":[
  {\"member_id\":\"SOMEONE_ELSE\",\"node_id\":\"node-alice\",\"started_at\":\"2026-08-19T12:00:00Z\",\"kind\":\"file_edit\",\"detail\":{\"file\":\"x.rs\"}}
]}")
echo "$PUSH2" | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert d.get('ok') is True, d
" || fail "push should succeed (member_id is forced from cert)"
pass "self-claimed member_id is overridden by cert identity"

step "alice (member) reads only her own rows"
ROWS=$(RPC "$ALICE_DIR" activity.rows "{\"team\":\"$TEAM_ID\"}")
echo "$ROWS" | python3 -c "
import json,sys
d=json.load(sys.stdin)
rows=d['data']
assert isinstance(rows,list) and len(rows)>=1, d
assert all(r['member_id']=='$ALICE_MID' for r in rows), rows
" || fail "member should only see her own rows"
pass "member sees only own rows"

step "owner sees all rows for the team"
OROWS=$(curl -sS --max-time 10 --cacert "$HOME_DIR/ca/ca.crt" --cert "$OWNER_CERT/member.crt" --key "$OWNER_CERT/member.key" \
  -H 'Content-Type: application/json' -d "{\"method\":\"activity.rows\",\"args\":{\"team\":\"$TEAM_ID\"}}" \
  "https://127.0.0.1:$PORT/rpc")
echo "$OROWS" | python3 -c "
import json,sys
d=json.load(sys.stdin)
rows=d['data']
assert isinstance(rows,list) and len(rows)>=3, rows
assert all(r['member_id']=='$ALICE_MID' for r in rows), rows
" || fail "owner should see all rows"
pass "owner sees all rows"

step "owner summary shows human vs ai split"
SUM=$(curl -sS --max-time 10 --cacert "$HOME_DIR/ca/ca.crt" --cert "$OWNER_CERT/member.crt" --key "$OWNER_CERT/member.key" \
  -H 'Content-Type: application/json' -d "{\"method\":\"activity.summary\",\"args\":{\"team\":\"$TEAM_ID\"}}" \
  "https://127.0.0.1:$PORT/rpc")
echo "$SUM" | python3 -c "
import json,sys
d=json.load(sys.stdin)
s=d['data']
assert s['overall']['count']>=3, s
assert s['human']['count']>=1, s
assert s['ai']['count']>=1, s
assert s['active_nodes']==1, s
" || fail "summary should split human/ai"
pass "summary splits human vs ai"

step "member cannot read another team's activity"
OTHER=$("$TEAMX" team create "OtherAct" --session s:owner2 --json | python3 -c "import json,sys; print(json.load(sys.stdin)['team']['id'])")
if RPC "$ALICE_DIR" activity.rows "{\"team\":\"$OTHER\"}" 2>/dev/null | grep -q '"ok":true'; then
  fail "alice should not read another team's activity"
fi
pass "cross-team activity read rejected"

step "activity.push without node_id is rejected (audit)"
if RPC "$ALICE_DIR" activity.push "{\"rows\":[{\"started_at\":\"2026-08-19T13:00:00Z\",\"kind\":\"command\",\"detail\":{\"name\":\"x\"}}]}" 2>/dev/null | grep -q '"ok":true'; then
  fail "push without node_id should be rejected"
fi
pass "push without node_id rejected"

step "non-owner cannot start teamx ui"
UI_OUT=$("$TEAMX" ui --session s:nobody --port 9532 2>&1 || true)
if echo "$UI_OUT" | grep -q "owner-only"; then
  pass "ui owner check rejected non-owner"
else
  echo "ui output was: $UI_OUT" >&2
  fail "ui should reject non-owner session"
fi

echo
echo "ALL PASS"
