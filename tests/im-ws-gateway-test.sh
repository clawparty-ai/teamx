#!/usr/bin/env bash
# IM P2 persistent-connection gateway (im-p2-ws-gateway) — end-to-end acceptance.
#
# Real `teamx serve` + mTLS. Covers the two pieces added by this task:
#
#  A. `im.bind` over the SERVICE identity (方案 A): a signed gateway delegation
#     carrying a valid one-time code binds an IM user who is NOT yet a member
#     (member resolution is skipped for this one pre-binding action). The code
#     stays single-use; a non-whitelisted method (`im.unbind`) is still refused.
#
#  B. `teamx im-gateway run` CLI wiring: the long-lived command must refuse to
#     start without provider credentials (fail-closed) rather than silently
#     connecting to nothing.
#
# Requires: curl, python3.
set -euo pipefail

TEAMX="${TEAMX:-${TEAMX_BIN:-$(dirname "$0")/../target/debug/teamx}}"
DB="$(mktemp /tmp/teamx-imws-XXXXXX).db"
HOME_DIR="$(mktemp -d /tmp/teamx-imws-home-XXXXXX)"
PORT="${TEAMX_IMWS_PORT:-5796}"
export TEAMX_DB="$DB"
export TEAMX_HOME="$HOME_DIR"
# The provider secret must be visible to BOTH serve (verify) and the gateway
# process (sign) — key grading: only this env carries it (design §5.4).
export TEAMX_IM_FEISHU_APP_SECRET="${TEAMX_IM_FEISHU_APP_SECRET:-ws-secret}"

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

# Sign a delegation exactly like `im::gateway::sign_delegation`.
sign() { # im_user nonce
  python3 - "$1" "$2" "$TEAMX_IM_FEISHU_APP_SECRET" <<'PY'
import hmac, hashlib, sys, time
im_user, nonce, secret = sys.argv[1], sys.argv[2], sys.argv[3]
ts = int(time.time())
msg = f"feishu|{im_user}||{ts}|{nonce}"
sig = hmac.new(secret.encode(), msg.encode(), hashlib.sha256).hexdigest()
print(f'{{"platform":"feishu","im_user_id":"{im_user}","im_team":null,'
      f'"signature":"{sig}","timestamp":{ts},"nonce":"{nonce}"}}')
PY
}

step "setup: team + owner cert + member letter"
"$TEAMX" init >/dev/null
"$TEAMX" team create "ImWs" --session s:owner --json >/dev/null
OWNER=$("$TEAMX" team status --session s:owner --json | python3 -c "import json,sys; print(json.load(sys.stdin)['teams'][0]['members'][0]['id'])")
OWNER_DIR="$(mktemp -d /tmp/teamx-imws-owner-XXXXXX)"
"$TEAMX" cert issue "$OWNER" owner --out "$OWNER_DIR" --json >/dev/null
INV=$("$TEAMX" team invite "contributor: ws" --session s:owner --json)
LETTER=$(echo "$INV" | python3 -c "import json,sys; print(json.load(sys.stdin)['letter'])")
MEMBER_DIR="$(mktemp -d /tmp/teamx-imws-member-XXXXXX)"
echo "$LETTER" | python3 -c "
import json,sys,base64
s=sys.stdin.read().strip()[len('teamx-inv:v1:'):]
d=json.loads(base64.b64decode(s))
c=d['certificates']
open('$MEMBER_DIR/member.crt','w').write(c['client_cert'])
open('$MEMBER_DIR/member.key','w').write(c['client_key'])
"
pass "owner=$OWNER"

step "owner issues the gateway service certificate"
GW_DIR="$(mktemp -d /tmp/teamx-imws-gw-XXXXXX)"
"$TEAMX" cert issue-service im-gateway ws-1 --out "$GW_DIR" --json >/dev/null
cp "$HOME_DIR/ca/ca.crt" "$GW_DIR/ca.crt"
pass "service cert issued (CN service:im-gateway:ws-1)"

step "start serve (mTLS)"
"$TEAMX" serve --addr 127.0.0.1 --port "$PORT" >/dev/null 2>&1 &
SERVE_PID=$!
for _ in $(seq 1 30); do
  curl -sS --max-time 2 --cacert "$HOME_DIR/ca/ca.crt" --cert "$OWNER_DIR/member.crt" \
    --key "$OWNER_DIR/member.key" "https://127.0.0.1:$PORT/health" >/dev/null 2>&1 && break
  sleep 0.2
done
pass "serve is up on https://127.0.0.1:$PORT"

rpc_member() {
  curl -sS --max-time 10 --cacert "$HOME_DIR/ca/ca.crt" --cert "$MEMBER_DIR/member.crt" \
    --key "$MEMBER_DIR/member.key" -X POST "https://127.0.0.1:$PORT/rpc" \
    -H 'Content-Type: application/json' -d "{\"method\":\"$1\",\"args\":$2}"
}
rpc_service() { # method args-json
  curl -sS --max-time 10 --cacert "$HOME_DIR/ca/ca.crt" --cert "$GW_DIR/service.crt" \
    --key "$GW_DIR/service.key" -X POST "https://127.0.0.1:$PORT/rpc" \
    -H 'Content-Type: application/json' -d "{\"method\":\"$1\",\"args\":$2}"
}

rpc_member team.import "{\"letter\":\"$LETTER\",\"name\":\"Ws Worker\"}" >/dev/null
TEAM_ID=$(rpc_member team.status "{}" | python3 -c "import json,sys; print(json.load(sys.stdin)['data']['teams'][0]['team']['id'])")
pass "team=$TEAM_ID"

step "A. member creates a one-time binding code"
CODE=$(rpc_member im.bind_code.create "{\"team\":\"$TEAM_ID\"}" \
  | python3 -c "import json,sys; d=json.load(sys.stdin); assert d['ok'], d; print(d['data']['code'])")
pass "code=$CODE"

step "A. service identity binds a NOT-yet-member IM user (方案 A)"
DELEG=$(sign "ou-ws-newbie" "nonce-ws-bind")
ARGS=$(python3 -c "
import json,sys
d=json.loads('''$DELEG''')
d['team']='$TEAM_ID'; d['code']='$CODE'
print(json.dumps(d))
")
rpc_service im.bind "$ARGS" \
  | python3 -c "import json,sys; d=json.load(sys.stdin); assert d['ok'] and d['data']['ok'], d" \
  || fail "im.bind over the service identity must succeed for an unbound user"
pass "im.bind succeeded (member resolution skipped for the pre-binding action)"

step "A. the new binding is live (im.query via the same service identity)"
DELEG2=$(sign "ou-ws-newbie" "nonce-ws-query")
ARGS2=$(python3 -c "
import json,sys
d=json.loads('''$DELEG2''')
d['team']='$TEAM_ID'; d['kind']='status'
print(json.dumps(d))
")
rpc_service im.query "$ARGS2" \
  | python3 -c "import json,sys; d=json.load(sys.stdin); assert d['ok'] and d['data']['ok'], d" \
  || fail "the freshly bound identity must resolve"
pass "bound identity resolves"

step "A. negative: the code is single-use"
DELEG3=$(sign "ou-ws-other" "nonce-ws-replay")
ARGS3=$(python3 -c "
import json,sys
d=json.loads('''$DELEG3''')
d['team']='$TEAM_ID'; d['code']='$CODE'
print(json.dumps(d))
")
rpc_service im.bind "$ARGS3" \
  | python3 -c "import json,sys; d=json.load(sys.stdin); assert not d.get('ok', False), d" \
  || fail "a used code must not bind again"
pass "replayed code refused"

step "A. negative: non-whitelisted method (im.unbind) still refused for services"
DELEG4=$(sign "ou-ws-newbie" "nonce-ws-unbind")
ARGS4=$(python3 -c "
import json,sys
d=json.loads('''$DELEG4''')
d['team']='$TEAM_ID'
print(json.dumps(d))
")
rpc_service im.unbind "$ARGS4" \
  | python3 -c "import json,sys; d=json.load(sys.stdin); assert not d.get('ok', False), d" \
  || fail "im.unbind must stay off the service whitelist"
pass "im.unbind refused (fail-closed whitelist)"

step "A. negative: a forged signature is refused"
FORGED=$(python3 -c "
import json,time
print(json.dumps({'platform':'feishu','im_user_id':'ou-ws-newbie','im_team':None,
  'signature':'deadbeef','timestamp':int(time.time()),'nonce':'nonce-ws-forged',
  'team':'$TEAM_ID','kind':'status'}))
")
rpc_service im.query "$FORGED" \
  | python3 -c "import json,sys; d=json.load(sys.stdin); assert not d.get('ok', False), d" \
  || fail "a forged delegation must be rejected"
pass "forged delegation rejected"

step "A. chat audit: im.chat.log writes im.chat.in / im.chat.out"
DELEG5=$(sign "ou-ws-newbie" "nonce-ws-chat-in")
ARGS5=$(python3 -c "
import json,sys
d=json.loads('''$DELEG5''')
d['team']='$TEAM_ID'; d['direction']='in'; d['text']='帮我看看今天的进展'
print(json.dumps(d))
")
rpc_service im.chat.log "$ARGS5" \
  | python3 -c "import json,sys; d=json.load(sys.stdin); assert d['ok'] and d['data']['ok'], d" \
  || fail "im.chat.log(in) must succeed for a bound member"
DELEG6=$(sign "ou-ws-newbie" "nonce-ws-chat-out")
ARGS6=$(python3 -c "
import json,sys
d=json.loads('''$DELEG6''')
d['team']='$TEAM_ID'; d['direction']='out'; d['text']='今天完成了 X'
print(json.dumps(d))
")
rpc_service im.chat.log "$ARGS6" \
  | python3 -c "import json,sys; d=json.load(sys.stdin); assert d['ok'] and d['data']['ok'], d" \
  || fail "im.chat.log(out) must succeed"
python3 - "$DB" <<'PY'
import sqlite3, sys, json
conn = sqlite3.connect(sys.argv[1])
rows = [(t, json.loads(p)) for t, p in
        conn.execute("SELECT type, payload_json FROM events WHERE type LIKE 'im.chat.%'").fetchall()]
kinds = sorted({t for t, _ in rows})
assert kinds == ['im.chat.in', 'im.chat.out'], kinds
for t, p in rows:
    assert p.get('source') == 'im:feishu' and p.get('im_user') and 'direction' in p, (t, p)
print(f"  ok: {len(rows)} chat audit events, kinds={kinds}")
PY
pass "chat audit verified"

step "A. negative: an unknown direction is refused"
DELEG7=$(sign "ou-ws-newbie" "nonce-ws-chat-bad")
BAD_DIR=$(python3 -c "
import json,sys
d=json.loads('''$DELEG7''')
d['team']='$TEAM_ID'; d['direction']='sideways'; d['text']='x'
print(json.dumps(d))
")
rpc_service im.chat.log "$BAD_DIR" \
  | python3 -c "import json,sys; d=json.load(sys.stdin); assert not d.get('ok', False), d" \
  || fail "an unknown direction must be refused"
pass "unknown direction refused"

step "B. im-gateway run refuses to start without provider credentials"
OUT=$(env -u TEAMX_IM_FEISHU_APP_ID -u TEAMX_IM_FEISHU_APP_SECRET \
  "$TEAMX" im-gateway run --cert-dir "$GW_DIR" --server "https://127.0.0.1:$PORT" \
  --team "$TEAM_ID" 2>&1 || true)
echo "$OUT" | grep -q "app_id missing" \
  || fail "im-gateway run must fail-closed without app_id (got: $OUT)"
pass "run fails closed without credentials"

step "ledger: im.accepted / im.rejected present with source + im_user"
python3 - "$DB" <<'PY'
import sqlite3, sys, json
conn = sqlite3.connect(sys.argv[1])
rows = [(t, json.loads(p)) for t, p in
        conn.execute("SELECT type, payload_json FROM events WHERE type LIKE 'im.%'").fetchall()]
kinds = sorted({t for t, _ in rows})
for want in ['im.accepted', 'im.rejected']:
    assert want in kinds, f"missing {want} in {kinds}"
bad = [t for t, p in rows if 'source' not in p and 'im_user' not in p and 'im_user_id' not in p]
assert not bad, f"im.* events missing source/im_user: {bad}"
print(f"  ok: {len(rows)} im.* events, kinds={kinds}")
PY
pass "audit trail verified"

echo
echo "ALL IM WS-GATEWAY TESTS PASSED"
