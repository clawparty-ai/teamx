#!/usr/bin/env bash
# IM P2 gateway process B (im-p2-gateway-process-b) — end-to-end acceptance.
#
# Real `teamx serve` + mTLS:
#   owner issues a service certificate (CN service:im-gateway:<instance>)
#   → member binds an IM identity
#   → `teamx im-gateway check` round trip: service cert mTLS → method
#     whitelist → delegation verify (HMAC + freshness + one-time nonce) →
#     binding resolve → whitelisted read (im.query)
#   negatives: non-whitelisted method via service cert (rejected + audited);
#     tampered delegation signature (rejected); member cert unaffected.
#
# Requires: curl, python3.
set -euo pipefail

TEAMX="${TEAMX:-${TEAMX_BIN:-$(dirname "$0")/../target/debug/teamx}}"
DB="$(mktemp /tmp/teamx-imgw-b-XXXXXX).db"
HOME_DIR="$(mktemp -d /tmp/teamx-imgw-b-home-XXXXXX)"
PORT="${TEAMX_IMGW_B_PORT:-5797}"
export TEAMX_DB="$DB"
export TEAMX_HOME="$HOME_DIR"
# The provider secret must be visible to BOTH serve (verify) and the gateway
# process (sign) — key grading: only this env carries it (design §5.4).
export TEAMX_IM_FEISHU_APP_SECRET="${TEAMX_IM_FEISHU_APP_SECRET:-gw-secret}"

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

step "setup: team + goal + owner cert + member letter (late import later)"
"$TEAMX" init >/dev/null
"$TEAMX" team create "GwB" --session s:owner --json >/dev/null
"$TEAMX" goal set "Gateway B acceptance" --session s:owner --json >/dev/null
OWNER=$("$TEAMX" team status --session s:owner --json | python3 -c "import json,sys; print(json.load(sys.stdin)['teams'][0]['members'][0]['id'])")
OWNER_DIR="$(mktemp -d /tmp/teamx-imgw-b-owner-XXXXXX)"
"$TEAMX" cert issue "$OWNER" owner --out "$OWNER_DIR" --json >/dev/null
INV=$("$TEAMX" team invite "contributor: gw b" --session s:owner --json)
LETTER=$(echo "$INV" | python3 -c "import json,sys; print(json.load(sys.stdin)['letter'])")
MEMBER_MID=$(echo "$INV" | python3 -c "import json,sys; print(json.load(sys.stdin)['member_id'])")
MEMBER_DIR="$(mktemp -d /tmp/teamx-imgw-b-member-XXXXXX)"
echo "$LETTER" | python3 -c "
import json,sys,base64
s=sys.stdin.read().strip()[len('teamx-inv:v1:'):]
d=json.loads(base64.b64decode(s))
c=d['certificates']
open('$MEMBER_DIR/member.crt','w').write(c['client_cert'])
open('$MEMBER_DIR/member.key','w').write(c['client_key'])
"
pass "owner=$OWNER member=$MEMBER_MID"

step "owner issues the gateway service certificate (short TTL)"
GW_DIR="$(mktemp -d /tmp/teamx-imgw-b-gw-XXXXXX)"
ISS=$("$TEAMX" cert issue-service im-gateway gw-1 --out "$GW_DIR" --json)
echo "$ISS" | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert d['ok'] and d['cn'] == 'service:im-gateway:gw-1', d
assert d['cert'].endswith('service.crt') and d['key'].endswith('service.key'), d
print('  ok: issued', d['cn'])
" || fail "service cert issuance failed"
cp "$HOME_DIR/ca/ca.crt" "$GW_DIR/ca.crt"
pass "ca.crt staged into the gateway cert dir"

step "start serve (mTLS)"
"$TEAMX" serve --addr 127.0.0.1 --port "$PORT" >/dev/null 2>&1 &
SERVE_PID=$!
for _ in $(seq 1 30); do
  curl -sS --max-time 2 --cacert "$HOME_DIR/ca/ca.crt" --cert "$OWNER_DIR/member.crt" \
    --key "$OWNER_DIR/member.key" "https://127.0.0.1:$PORT/health" >/dev/null 2>&1 && break
  sleep 0.2
done
pass "serve is up on https://127.0.0.1:$PORT"

step "member late-imports + binds the IM identity"
rpc_member() {
  curl -sS --max-time 10 --cacert "$HOME_DIR/ca/ca.crt" --cert "$MEMBER_DIR/member.crt" \
    --key "$MEMBER_DIR/member.key" -X POST "https://127.0.0.1:$PORT/rpc" \
    -H 'Content-Type: application/json' -d "{\"method\":\"$1\",\"args\":$2}"
}
rpc_member team.import "{\"letter\":\"$LETTER\",\"name\":\"GwB Worker\"}" >/dev/null
TEAM_ID=$(rpc_member team.status "{}" | python3 -c "import json,sys; print(json.load(sys.stdin)['data']['teams'][0]['team']['id'])")
CODE=$(rpc_member im.bind_code.create "{\"team\":\"$TEAM_ID\"}" | python3 -c "import json,sys; d=json.load(sys.stdin); assert d['ok'], d; print(d['data']['code'])")
rpc_member im.bind "{\"team\":\"$TEAM_ID\",\"platform\":\"feishu\",\"im_user_id\":\"u-gwb\",\"code\":\"$CODE\"}" \
  | python3 -c "import json,sys; d=json.load(sys.stdin); assert d['ok'] and d['data']['ok'], d" \
  || fail "bind failed"
pass "bound u-gwb (team $TEAM_ID)"

step "gateway process round trip: service cert → whitelist → delegation → im.query"
OUT=$("$TEAMX" im-gateway check --json --cert-dir "$GW_DIR" --server "https://127.0.0.1:$PORT" \
  --team "$TEAM_ID" --im-user u-gwb --kind status)
echo "$OUT" | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert d['ok'], d
assert d['cn'] == 'service:im-gateway:gw-1', d
r=d['response']
assert r['ok'] and r['data']['ok'], r
assert r['data']['member']['member_id'] == '$MEMBER_MID', r
print('  ok: gateway round trip resolved member', r['data']['member']['member_id'])
" || fail "gateway check round trip failed"
pass "anchor: gateway B end-to-end PASS (identity from binding, never the cert)"

step "negative: service cert calling a non-whitelisted method is rejected + audited"
curl -sS --max-time 10 --cacert "$HOME_DIR/ca/ca.crt" --cert "$GW_DIR/service.crt" \
  --key "$GW_DIR/service.key" -X POST "https://127.0.0.1:$PORT/rpc" \
  -H 'Content-Type: application/json' -d '{"method":"team.status","args":{}}' \
  | python3 -c "import json,sys; d=json.load(sys.stdin); assert not d.get('ok', False), d" \
  || fail "non-whitelisted method must be rejected"
pass "non-whitelisted method rejected (fail-closed whitelist)"

step "negative: tampered delegation signature is rejected by the pipeline"
BAD=$(python3 -c "
import json
print(json.dumps({'team':'$TEAM_ID','platform':'feishu','im_user_id':'u-gwb','kind':'status','timestamp':0,'nonce':'forged-nonce','signature':'deadbeef'}))
")
curl -sS --max-time 10 --cacert "$HOME_DIR/ca/ca.crt" --cert "$GW_DIR/service.crt" \
  --key "$GW_DIR/service.key" -X POST "https://127.0.0.1:$PORT/rpc" \
  -H 'Content-Type: application/json' -d "{\"method\":\"im.query\",\"args\":$BAD}" \
  | python3 -c "import json,sys; d=json.load(sys.stdin); assert not d.get('ok', False), d" \
  || fail "forged delegation must be rejected"
pass "forged delegation rejected (HMAC verify)"

step "member cert is unaffected by the service whitelist"
rpc_member im.query "{\"team\":\"$TEAM_ID\",\"platform\":\"feishu\",\"im_user_id\":\"u-gwb\",\"kind\":\"status\"}" \
  | python3 -c "import json,sys; d=json.load(sys.stdin); assert d['ok'] and d['data']['ok'], d" \
  || fail "member cert im.query should still work"
pass "member cert path unchanged"

step "ledger: im.accepted / im.rejected / im.query all with source + im_user"
python3 - "$DB" <<'PY'
import sqlite3, sys, json
conn = sqlite3.connect(sys.argv[1])
rows = [(t, json.loads(p)) for t, p in
        conn.execute("SELECT type, payload_json FROM events WHERE type LIKE 'im.%'").fetchall()]
kinds = sorted({t for t, _ in rows})
for want in ['im.accepted', 'im.query', 'im.rejected']:
    assert want in kinds, f"missing {want} in {kinds}"
bad = [t for t, p in rows if 'source' not in p or ('im_user' not in p and 'im_user_id' not in p)]
assert not bad, f"im.* events missing source/im_user: {bad}"
print(f"  ok: {len(rows)} im.* events, kinds={kinds}, all with source + im_user")
PY
pass "audit trail verified"

echo
echo "ALL IM GATEWAY-B TESTS PASSED"
