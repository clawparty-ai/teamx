#!/usr/bin/env bash
# Discussion RPC + console-data channel test (network mode).
#
# Boots a real `teamx serve`, owner + one member (alice) via mTLS, then drives
# the discussion.* RPCs over /rpc exactly as the console / agent would:
#   create -> say -> reply(reference) -> open(assert floor/reply snapshot)
#   -> non-lead close rejected -> owner close -> say-after-close rejected
#   -> unknown discussion action rejected (R-02).
#
# Requires: curl, openssl, python3.
set -euo pipefail

TEAMX="${TEAMX:-$(dirname "$0")/../target/debug/teamx}"
DB="$(mktemp /tmp/teamx-disc-XXXXXX).db"
HOME_DIR="$(mktemp -d /tmp/teamx-disc-home-XXXXXX)"
PORT="${TEAMX_DISC_TEST_PORT:-5793}"
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
"$TEAMX" team create "DiscTeam" --session s:owner --json >/dev/null
OWNER=$("$TEAMX" team status --session s:owner --json | python3 -c "import json,sys; print(json.load(sys.stdin)['teams'][0]['members'][0]['id'])")
OWNER_CERT="$(mktemp -d /tmp/teamx-disc-owner-XXXXXX)"
"$TEAMX" cert issue "$OWNER" owner --out "$OWNER_CERT" --json >/dev/null
INV=$("$TEAMX" team invite "contributor: does work" --session s:owner --json)
LETTER=$(echo "$INV" | python3 -c "import json,sys; print(json.load(sys.stdin)['letter'])")
ALICE_MID=$(echo "$INV" | python3 -c "import json,sys; print(json.load(sys.stdin)['member_id'])")
ALICE_DIR="$(mktemp -d /tmp/teamx-disc-alice-XXXXXX)"
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

TEAM_ID=$("$TEAMX" team status --session s:owner --json | python3 -c "import json,sys; print(json.load(sys.stdin)['teams'][0]['team']['id'])")

RPC() { # cert_dir ca method args
  curl -sS --max-time 10 --cacert "$2" --cert "$1/member.crt" --key "$1/member.key" \
    -H 'Content-Type: application/json' -d "{\"method\":\"$3\",\"args\":$4}" \
    "https://127.0.0.1:$PORT/rpc"
}
CA="$HOME_DIR/ca/ca.crt"

step "alice imports the letter, owner approves (member active)"
IMPORT=$(RPC "$ALICE_DIR" "$CA" team.import "{\"letter\":\"$LETTER\",\"name\":\"Alice\"}")
echo "$IMPORT" | python3 -c "import json,sys; d=json.load(sys.stdin); assert d.get('ok') is True, d" || fail "import failed"
APPROVE=$(RPC "$OWNER_CERT" "$CA" team.approve "{\"member_id\":\"$ALICE_MID\"}")
echo "$APPROVE" | python3 -c "import json,sys; d=json.load(sys.stdin); assert d.get('ok') is True, d" || fail "approve failed"
pass "imported + approved"

step "owner creates a discussion"
CR=$(RPC "$OWNER_CERT" "$CA" discussion.create "{\"title\":\"review round2\",\"body\":\"开个线程\",\"team\":\"$TEAM_ID\"}")
DID=$(echo "$CR" | python3 -c "import json,sys; d=json.load(sys.stdin); assert d['ok'] and d['data']['ok'], d; print(d['data']['discussion_id'])")
pass "created $DID"

step "alice posts a message"
SAY=$(RPC "$ALICE_DIR" "$CA" discussion.say "{\"id\":\"$DID\",\"body\":\"DOI 抽查 3 条 OK\",\"team\":\"$TEAM_ID\"}")
MSG1=$(echo "$SAY" | python3 -c "import json,sys; d=json.load(sys.stdin); assert d['ok'] and d['data']['ok'], d; print(d['data']['message_id'])")
pass "alice said $MSG1"

step "owner replies with a reference to alice's message"
REP=$(RPC "$OWNER_CERT" "$CA" discussion.reply "{\"id\":\"$DID\",\"message_id\":\"$MSG1\",\"body\":\"收到\",\"team\":\"$TEAM_ID\"}")
echo "$REP" | python3 -c "import json,sys; d=json.load(sys.stdin); assert d['ok'] and d['data']['ok'], d" || fail "reply failed"
pass "replied"

step "open returns floors with reply snapshot (message_id + body text)"
OPEN=$(RPC "$OWNER_CERT" "$CA" discussion.open "{\"id\":\"$DID\",\"team\":\"$TEAM_ID\"}")
echo "$OPEN" | python3 -c "
import json,sys
d=json.load(sys.stdin); data=d['data']
msgs=data['messages']
assert len(msgs)>=2, msgs
# newest-first: index 0 is the owner reply, carrying reply_to snapshot.
top=msgs[0]
assert top['reply_to'] and top['reply_to']['message_id'], top
assert top['reply_to']['text']=='DOI 抽查 3 条 OK', top
assert any(m['body']=='DOI 抽查 3 条 OK' for m in msgs), msgs
print('floors:', len(msgs))
" && pass "open: floors + reply snapshot" || fail "open structure"

step "list shows the thread (1 discussion, messages>=2)"
LIST=$(RPC "$OWNER_CERT" "$CA" discussion.list "{\"team\":\"$TEAM_ID\"}")
echo "$LIST" | python3 -c "
import json,sys
d=json.load(sys.stdin); ds=d['data']['discussions']
assert any(x['discussion_id']=='$DID' and x['messages']>=2 for x in ds), ds
" && pass "list shows thread" || fail "list"

step "non-lead member cannot close"
CLOSE_M=$(RPC "$ALICE_DIR" "$CA" discussion.close "{\"id\":\"$DID\",\"team\":\"$TEAM_ID\"}")
echo "$CLOSE_M" | python3 -c "import json,sys; d=json.load(sys.stdin); assert not d.get('ok') or not d['data'].get('ok'), d" || fail "member close should fail"
pass "member close rejected"

step "owner closes"
CLOSE=$(RPC "$OWNER_CERT" "$CA" discussion.close "{\"id\":\"$DID\",\"team\":\"$TEAM_ID\"}")
echo "$CLOSE" | python3 -c "import json,sys; d=json.load(sys.stdin); assert d['ok'] and d['data']['ok'], d" || fail "close failed"
pass "closed"

step "say after close is rejected"
LATE=$(RPC "$ALICE_DIR" "$CA" discussion.say "{\"id\":\"$DID\",\"body\":\"late\",\"team\":\"$TEAM_ID\"}")
echo "$LATE" | python3 -c "import json,sys; d=json.load(sys.stdin); assert not d.get('ok') or not d['data'].get('ok'), d" || fail "late say should fail"
pass "late say rejected"

step "unknown discussion action rejected (R-02)"
UNK=$(curl -sS --max-time 6 --cacert "$CA" --cert "$OWNER_CERT/member.crt" --key "$OWNER_CERT/member.key" \
  -H 'Content-Type: application/json' -d "{\"method\":\"discussion.bogus\",\"args\":{\"team\":\"$TEAM_ID\"}}" \
  "https://127.0.0.1:$PORT/rpc")
echo "$UNK" | python3 -c "import json,sys; d=json.load(sys.stdin); assert not d.get('ok'), d; assert 'unknown' in str(d.get('error','')).lower() or 'unknown' in str(d).lower(), d" || fail "unknown action should error"
pass "unknown action rejected"

echo
echo "== result: PASSED =="
