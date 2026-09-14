#!/usr/bin/env bash
# IM identity binding (P1) — RPC acceptance over mTLS (network-mode teams).
#
# Boots a real `teamx serve`, then drives the `im.*` RPCs exactly as a member
# client / the in-process gateway would:
#   im.bind_code.create -> im.bind (role echoed, ledger source im:feishu)
#   -> single use -> im.bindings.list (self vs lead) -> im.resolve (lead only)
#   -> im.unbind -> unknown method rejected
#
# Requires: curl, openssl, python3.
set -euo pipefail

# Accept both the TEAMX used by the other suites and run-all.sh's TEAMX_BIN.
TEAMX="${TEAMX:-${TEAMX_BIN:-$(dirname "$0")/../target/debug/teamx}}"
DB="$(mktemp /tmp/teamx-imbind-XXXXXX).db"
HOME_DIR="$(mktemp -d /tmp/teamx-imbind-home-XXXXXX)"
PORT="${TEAMX_IM_TEST_PORT:-5794}"
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

if ! command -v curl >/dev/null 2>&1 || ! command -v openssl >/dev/null 2>&1; then
  echo "skipped: curl/openssl not available"; exit 0
fi

step "setup: team + owner cert + member letter"
"$TEAMX" init >/dev/null
"$TEAMX" team create "IMBindTeam" --session s:owner --json >/dev/null
"$TEAMX" goal set "IM binding over RPC" --session s:owner >/dev/null
OWNER=$("$TEAMX" team status --session s:owner --json | python3 -c "import json,sys; print(json.load(sys.stdin)['teams'][0]['members'][0]['id'])")
OWNER_CERT="$(mktemp -d /tmp/teamx-imbind-owner-XXXXXX)"
"$TEAMX" cert issue "$OWNER" owner --out "$OWNER_CERT" --json >/dev/null
INV=$("$TEAMX" team invite "contributor: does work" --session s:owner --json)
LETTER=$(echo "$INV" | python3 -c "import json,sys; print(json.load(sys.stdin)['letter'])")
MEMBER_MID=$(echo "$INV" | python3 -c "import json,sys; print(json.load(sys.stdin)['member_id'])")
MEMBER_DIR="$(mktemp -d /tmp/teamx-imbind-member-XXXXXX)"
echo "$LETTER" | python3 -c "
import json,sys,base64
s=sys.stdin.read().strip()[len('teamx-inv:v1:'):]
d=json.loads(base64.b64decode(s))
c=d['certificates']
open('$MEMBER_DIR/member.crt','w').write(c['client_cert'])
open('$MEMBER_DIR/member.key','w').write(c['client_key'])
"
pass "owner=$OWNER member=$MEMBER_MID"

step "start serve"
"$TEAMX" serve --addr 127.0.0.1 --port "$PORT" >/dev/null 2>&1 &
SERVE_PID=$!
for _ in $(seq 1 30); do
  if curl -sS --max-time 2 --cacert "$HOME_DIR/ca/ca.crt" --cert "$OWNER_CERT/member.crt" --key "$OWNER_CERT/member.key" "https://127.0.0.1:$PORT/health" >/dev/null 2>&1; then break; fi
  sleep 0.2
done
pass "serve is up on $PORT"

TEAM_ID=$("$TEAMX" team status --session s:owner --json | python3 -c "import json,sys; print(json.load(sys.stdin)['teams'][0]['team']['id'])")
CA="$HOME_DIR/ca/ca.crt"
RPC() { # cert_dir ca method args
  curl -sS --max-time 10 --cacert "$2" --cert "$1/member.crt" --key "$1/member.key" \
    -H 'Content-Type: application/json' -d "{\"method\":\"$3\",\"args\":$4}" \
    "https://127.0.0.1:$PORT/rpc"
}
jget() { python3 -c "import json,sys; print(json.load(sys.stdin)$1)"; }

step "member joins the team over RPC, owner approves"
IMPORT=$(RPC "$MEMBER_DIR" "$CA" team.import "{\"letter\":\"$LETTER\",\"name\":\"Worker\"}")
echo "$IMPORT" | python3 -c "import json,sys; d=json.load(sys.stdin); assert d.get('ok') is True, d" || fail "import failed"
RPC "$OWNER_CERT" "$CA" team.approve "{\"member_id\":\"$MEMBER_MID\"}" >/dev/null
pass "member active"

step "im.bind_code.create: self-service code is pre-associated with the member"
CODE_JSON=$(RPC "$MEMBER_DIR" "$CA" im.bind_code.create "{\"team\":\"$TEAM_ID\"}")
CODE=$(echo "$CODE_JSON" | python3 -c "import json,sys; d=json.load(sys.stdin); assert d['ok'] and d['data']['ok'], d; print(d['data']['code'])")
ASSOC=$(echo "$CODE_JSON" | jget "['data']['member_id']")
[ "$ASSOC" = "$MEMBER_MID" ] || fail "code should be associated with $MEMBER_MID, got $ASSOC"
[ "${#CODE}" = "10" ] || fail "unexpected code length ${#CODE}"
pass "code issued for $ASSOC"

step "im.bind: the IM identity binds and the role is echoed"
BOUND=$(RPC "$MEMBER_DIR" "$CA" im.bind "{\"team\":\"$TEAM_ID\",\"platform\":\"feishu\",\"im_user_id\":\"ou_rpc_worker\",\"code\":\"$CODE\"}")
echo "$BOUND" | python3 -c "
import json,sys
d=json.load(sys.stdin); data=d['data']
assert d['ok'] and data['ok'], d
assert data['action']=='bind', data
assert data['member']['id']=='$MEMBER_MID', data
assert data['member']['role'], data
print('  ok: role echoed =', data['member']['role'])
" || fail "bind failed"
pass "ou_rpc_worker -> $MEMBER_MID"

step "im.bind: single use (replay is rejected and audited)"
REPLAY=$(RPC "$MEMBER_DIR" "$CA" im.bind "{\"team\":\"$TEAM_ID\",\"platform\":\"feishu\",\"im_user_id\":\"ou_rpc_other\",\"code\":\"$CODE\"}")
echo "$REPLAY" | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert not d.get('ok'), d
assert 'code_used' in d.get('error',''), d
" || fail "replay should be rejected with code_used"
pass "replay rejected (code_used)"

step "im.bindings.list: self scope for the member, team scope for the lead"
SELF=$(RPC "$MEMBER_DIR" "$CA" im.bindings.list "{\"team\":\"$TEAM_ID\"}")
echo "$SELF" | jget "['data']['count']" | grep -qx 1 || fail "member should see exactly one binding"
NONLEAD=$(RPC "$MEMBER_DIR" "$CA" im.bindings.list "{\"team\":\"$TEAM_ID\",\"all\":true}")
echo "$NONLEAD" | python3 -c "import json,sys; d=json.load(sys.stdin); assert not d.get('ok'), d" || fail "non-lead --all must be rejected"
TEAMVIEW=$(RPC "$OWNER_CERT" "$CA" im.bindings.list "{\"team\":\"$TEAM_ID\",\"all\":true}")
echo "$TEAMVIEW" | jget "['data']['count']" | grep -qx 1 || fail "owner should see the team's binding"
pass "self scope = 1, team scope = 1, non-lead --all rejected"

step "im.resolve: lead-only identity lookup"
NONLEAD=$(RPC "$MEMBER_DIR" "$CA" im.resolve "{\"team\":\"$TEAM_ID\",\"platform\":\"feishu\",\"im_user_id\":\"ou_rpc_worker\"}")
echo "$NONLEAD" | python3 -c "import json,sys; d=json.load(sys.stdin); assert not d.get('ok'), d" || fail "resolve must be lead-only"
RESOLVED=$(RPC "$OWNER_CERT" "$CA" im.resolve "{\"team\":\"$TEAM_ID\",\"platform\":\"feishu\",\"im_user_id\":\"ou_rpc_worker\"}")
echo "$RESOLVED" | python3 -c "
import json,sys
d=json.load(sys.stdin); data=d['data']
assert data['bound'] is True, d
assert data['member']['member_id']=='$MEMBER_MID', d
" || fail "resolve failed"
UNKNOWN=$(RPC "$OWNER_CERT" "$CA" im.resolve "{\"team\":\"$TEAM_ID\",\"platform\":\"feishu\",\"im_user_id\":\"ou_nobody\"}")
echo "$UNKNOWN" | jget "['data']['bound']" | grep -qx False || fail "unknown identity must not resolve"
pass "bound resolves, unknown does not"

step "im.unbind: owner (lead) unbinds, identity stops resolving"
OUT=$(RPC "$OWNER_CERT" "$CA" im.unbind "{\"team\":\"$TEAM_ID\",\"platform\":\"feishu\",\"im_user_id\":\"ou_rpc_worker\"}")
echo "$OUT" | jget "['data']['unbound']" | grep -qx True || fail "unbind failed"
AFTER=$(RPC "$OWNER_CERT" "$CA" im.resolve "{\"team\":\"$TEAM_ID\",\"platform\":\"feishu\",\"im_user_id\":\"ou_rpc_worker\"}")
echo "$AFTER" | jget "['data']['bound']" | grep -qx False || fail "identity should be unbound"
pass "unbound"

step "unknown im method is rejected"
BOGUS=$(RPC "$OWNER_CERT" "$CA" im.nope "{}")
echo "$BOGUS" | python3 -c "import json,sys; d=json.load(sys.stdin); assert not d.get('ok'), d; assert 'unknown im method' in d.get('error',''), d" || fail "unknown method should be rejected"
pass "unknown im method rejected"

step "ledger: bind / unbind / rejection all carry the IM source"
EVENTS=$("$TEAMX" events --team "$TEAM_ID" --json)
echo "$EVENTS" | python3 -c "
import json,sys
ev=json.load(sys.stdin)['events']
binds=[e for e in ev if e['type']=='im.binding']
rej=[e for e in ev if e['type']=='im.rejected']
assert binds, 'no im.binding event'
assert rej, 'no im.rejected event'
actions=sorted(p['payload']['action'] for p in binds)
assert actions==['bind','unbind'], actions
assert all(p['payload'].get('source')=='im:feishu' for p in binds), binds
assert any(p['payload']['detail']=='code_used' for p in rej), rej
print('  ok: im.binding actions =', actions)
print('  ok: im.rejected details =', sorted({p['payload']['detail'] for p in rej}))
" || fail "ledger audit"
pass "ledger audited"

printf '\nIM binding (P1) RPC acceptance: all checks passed\n'
