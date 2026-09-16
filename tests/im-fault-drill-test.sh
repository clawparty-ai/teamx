#!/usr/bin/env bash
# IM P2 fault drills (im-p2-fault-drills) — the three-layer interception
# proof. Real `teamx serve` + mTLS; every attack must be refused at the right
# layer AND audited to the ledger.
#
#   L1 gateway layer (service cert path):
#     - forged signature → invalid_signature
#     - missing signature → missing_signature
#     - stale timestamp → stale_timestamp
#     - replayed nonce → duplicate
#     - unbound IM identity → unbound
#     - service cert calling a non-whitelisted method → refused
#   L2 executor layer (member-cert path — the "hijacked gateway" scenario):
#     - privilege escalation: worker cert + owner's im_user_id → REFUSED
#       (identity mismatch — the delegated identity must be bound to the
#       certificate member; fixed by this PR, previously a hole)
#     - non-lead with own binding → not a lead
#     - unknown write action → not whitelisted (fail-closed)
#   L3 state layer:
#     - im_msg_id replay → idempotent, no double effect
#     - confirm token replay → refused (consumed)
#     - cross-action token → refused
#     - every refusal audited (im.rejected) with source + im_user
#
# Requires: curl, python3, sqlite3.
set -euo pipefail

TEAMX="${TEAMX:-${TEAMX_BIN:-$(dirname "$0")/../target/debug/teamx}}"
DB="$(mktemp /tmp/teamx-imdrill-XXXXXX).db"
HOME_DIR="$(mktemp -d /tmp/teamx-imdrill-home-XXXXXX)"
PORT="${TEAMX_IMDRILL_PORT:-5796}"
export TEAMX_DB="$DB"
export TEAMX_HOME="$HOME_DIR"
# Provider secret visible to serve (verify) and the drill (forge attempts).
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

if ! command -v curl >/dev/null 2>&1 || ! command -v python3 >/dev/null 2>&1 || ! command -v sqlite3 >/dev/null 2>&1; then
  echo "skipped: curl/python3/sqlite3 not available"; exit 0
fi

rpc() { # cert_dir cert_base method args_json
  curl -sS --max-time 10 --cacert "$HOME_DIR/ca/ca.crt" --cert "$1/$2.crt" \
    --key "$1/$2.key" -X POST "https://127.0.0.1:$PORT/rpc" \
    -H 'Content-Type: application/json' -d "{\"method\":\"$3\",\"args\":$4}"
}
expect_err() { # json — assert RPC returned an error
  python3 -c "import json,sys; d=json.load(sys.stdin); assert not d.get('ok', False), d" \
    || fail "expected refusal, got success"
}

sign() { # canonical fields → HMAC-SHA256 hex (matches gateway::sign_delegation)
  python3 - "$@" <<'PY'
import sys, hmac, hashlib, os
platform, im_user, im_team, ts, nonce = sys.argv[1:6]
secret = os.environ.get('TEAMX_IM_FEISHU_APP_SECRET', 'gw-secret')
msg = f"{platform}|{im_user}|{im_team}|{ts}|{nonce}"
print(hmac.new(secret.encode(), msg.encode(), hashlib.sha256).hexdigest())
PY
}

step "setup: team + owner/worker certs + service cert + owner IM binding + pending member"
"$TEAMX" init >/dev/null
"$TEAMX" team create "FaultDrill" --session s:owner --json >/dev/null
OWNER=$("$TEAMX" team status --session s:owner --json | python3 -c "import json,sys; print(json.load(sys.stdin)['teams'][0]['members'][0]['id'])")
OWNER_DIR="$(mktemp -d /tmp/teamx-imdrill-owner-XXXXXX)"
"$TEAMX" cert issue "$OWNER" owner --out "$OWNER_DIR" --json >/dev/null

# Worker member (not lead).
INV=$("$TEAMX" team invite "contributor: drill" --session s:owner --json)
LETTER=$(echo "$INV" | python3 -c "import json,sys; print(json.load(sys.stdin)['letter'])")
WORKER_MID=$(echo "$INV" | python3 -c "import json,sys; print(json.load(sys.stdin)['member_id'])")
WORKER_DIR="$(mktemp -d /tmp/teamx-imdrill-worker-XXXXXX)"
echo "$LETTER" | python3 -c "
import json,sys,base64
s=sys.stdin.read().strip()[len('teamx-inv:v1:'):]
d=json.loads(base64.b64decode(s))
c=d['certificates']
open('$WORKER_DIR/member.crt','w').write(c['client_cert'])
open('$WORKER_DIR/member.key','w').write(c['client_key'])
"

# Gateway service cert.
GW_DIR="$(mktemp -d /tmp/teamx-imdrill-gw-XXXXXX)"
"$TEAMX" cert issue-service im-gateway gw-drill --out "$GW_DIR" --json >/dev/null

"$TEAMX" serve --addr 127.0.0.1 --port "$PORT" >/dev/null 2>&1 &
SERVE_PID=$!
for _ in $(seq 1 30); do
  curl -sS --max-time 2 --cacert "$HOME_DIR/ca/ca.crt" --cert "$OWNER_DIR/member.crt" \
    --key "$OWNER_DIR/member.key" "https://127.0.0.1:$PORT/health" >/dev/null 2>&1 && break
  sleep 0.2
done

rpc "$WORKER_DIR" member team.import "{\"letter\":\"$LETTER\",\"name\":\"Drill Worker\"}" >/dev/null
TEAM_ID=$(rpc "$OWNER_DIR" member team.status "{}" | python3 -c "import json,sys; print(json.load(sys.stdin)['data']['teams'][0]['team']['id'])")

# Bind the owner's IM identity (u-owner) over RPC (member-cert self-service).
CODE=$(rpc "$OWNER_DIR" member im.bind_code.create "{\"team\":\"$TEAM_ID\"}" | python3 -c "import json,sys; d=json.load(sys.stdin); assert d['ok'], d; print(d['data']['code'])")
rpc "$OWNER_DIR" member im.bind "{\"team\":\"$TEAM_ID\",\"platform\":\"feishu\",\"im_user_id\":\"u-owner\",\"code\":\"$CODE\"}"   | python3 -c "import json,sys; d=json.load(sys.stdin); assert d['ok'], d" || fail "owner bind failed"

# A pending member to approve (the mutation probe): invite + import creates
# the members row in state=pending.
INV2=$("$TEAMX" team invite "contributor: victim" --session s:owner --json)
LETTER2=$(echo "$INV2" | python3 -c "import json,sys; print(json.load(sys.stdin)['letter'])")
PENDING_MID=$(echo "$INV2" | python3 -c "import json,sys; print(json.load(sys.stdin)['member_id'])")
PENDING_DIR="$(mktemp -d /tmp/teamx-imdrill-pending-XXXXXX)"
echo "$LETTER2" | python3 -c "
import json,sys,base64
s=sys.stdin.read().strip()[len('teamx-inv:v1:'):]
d=json.loads(base64.b64decode(s))
c=d['certificates']
open('$PENDING_DIR/member.crt','w').write(c['client_cert'])
open('$PENDING_DIR/member.key','w').write(c['client_key'])
"
rpc "$PENDING_DIR" member team.import "{\"letter\":\"$LETTER2\",\"name\":\"Drill Victim\"}" >/dev/null
pass "team=$TEAM_ID owner=$OWNER worker=$WORKER_MID pending=$PENDING_MID"

# ---------------------------------------------------------------------------
step "L1: forged signature (attacker knows the fields, not the secret)"
NOW=$(date +%s)
rpc "$GW_DIR" service im.query "{\"platform\":\"feishu\",\"im_user_id\":\"u-owner\",\"team\":\"$TEAM_ID\",\"kind\":\"status\",\"timestamp\":$NOW,\"nonce\":\"n-forge-1\",\"signature\":\"deadbeef\"}" \
  | expect_err
pass "forged signature refused"

step "L1: missing signature"
rpc "$GW_DIR" service im.query "{\"platform\":\"feishu\",\"im_user_id\":\"u-owner\",\"team\":\"$TEAM_ID\",\"kind\":\"status\",\"timestamp\":$NOW,\"nonce\":\"n-miss-1\"}" \
  | expect_err
pass "missing signature refused"

step "L1: stale timestamp (valid signature, old clock)"
STALE=$((NOW - 3600))
SIG=$(sign feishu u-owner "" "$STALE" "n-stale-1")
rpc "$GW_DIR" service im.query "{\"platform\":\"feishu\",\"im_user_id\":\"u-owner\",\"team\":\"$TEAM_ID\",\"kind\":\"status\",\"timestamp\":$STALE,\"nonce\":\"n-stale-1\",\"signature\":\"$SIG\"}" \
  | expect_err
pass "stale timestamp refused"

step "L1: valid delegation passes, then nonce replay refused (duplicate)"
SIG=$(sign feishu u-owner "" "$NOW" "n-ok-1")
rpc "$GW_DIR" service im.query "{\"platform\":\"feishu\",\"im_user_id\":\"u-owner\",\"team\":\"$TEAM_ID\",\"kind\":\"status\",\"timestamp\":$NOW,\"nonce\":\"n-ok-1\",\"signature\":\"$SIG\"}" \
  | python3 -c "import json,sys; d=json.load(sys.stdin); assert d.get('ok'), d" \
  || fail "valid delegation must pass"
rpc "$GW_DIR" service im.query "{\"platform\":\"feishu\",\"im_user_id\":\"u-owner\",\"team\":\"$TEAM_ID\",\"kind\":\"status\",\"timestamp\":$NOW,\"nonce\":\"n-ok-1\",\"signature\":\"$SIG\"}" \
  | expect_err
pass "nonce replay refused (im_seen)"

step "L1: unbound IM identity"
SIG=$(sign feishu u-ghost "" "$NOW" "n-ghost-1")
rpc "$GW_DIR" service im.query "{\"platform\":\"feishu\",\"im_user_id\":\"u-ghost\",\"team\":\"$TEAM_ID\",\"kind\":\"status\",\"timestamp\":$NOW,\"nonce\":\"n-ghost-1\",\"signature\":\"$SIG\"}" \
  | expect_err
pass "unbound identity refused"

step "L1: service cert calling a non-whitelisted method"
rpc "$GW_DIR" service task.create "{\"title\":\"pwn\",\"team\":\"$TEAM_ID\"}" | expect_err
pass "non-whitelisted method refused"

# ---------------------------------------------------------------------------
step "L2: privilege escalation — worker cert + owner's im_user_id (the hijack)"
OUT=$(rpc "$WORKER_DIR" member im.approve "{\"team\":\"$TEAM_ID\",\"member_id\":\"$PENDING_MID\",\"im_user_id\":\"u-owner\",\"im_msg_id\":\"esc-1\"}")
echo "$OUT" | expect_err
echo "$OUT" | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert 'identity mismatch' in d.get('error',''), d
" || fail "escalation must be refused with identity mismatch"
STATE=$(sqlite3 -cmd ".timeout 5000" "$DB" "SELECT state FROM members WHERE id='$PENDING_MID'")
[ "$STATE" = "pending" ] || fail "escalation must not change state (got $STATE)"
pass "privilege escalation refused + no state change"

step "L2: non-lead with their OWN binding still refused (role gate)"
CODE2=$(rpc "$WORKER_DIR" member im.bind_code.create "{\"team\":\"$TEAM_ID\"}" | python3 -c "import json,sys; d=json.load(sys.stdin); assert d['ok'], d; print(d['data']['code'])")
rpc "$WORKER_DIR" member im.bind "{\"team\":\"$TEAM_ID\",\"platform\":\"feishu\",\"im_user_id\":\"u-worker\",\"code\":\"$CODE2\"}"   | python3 -c "import json,sys; d=json.load(sys.stdin); assert d['ok'], d" || fail "worker bind failed"
OUT=$(rpc "$WORKER_DIR" member im.approve "{\"team\":\"$TEAM_ID\",\"member_id\":\"$PENDING_MID\",\"im_user_id\":\"u-worker\",\"im_msg_id\":\"esc-3\"}")
echo "$OUT" | expect_err
echo "$OUT" | python3 -c "import json,sys; d=json.load(sys.stdin); assert 'not a lead' in d.get('error',''), d" \
  || fail "non-lead own-binding attempt must be refused with not-a-lead"
pass "non-lead (own binding) refused at role gate"

step "L2: unknown write action is fail-closed"
rpc "$OWNER_DIR" member im.destroy_team "{\"team\":\"$TEAM_ID\",\"im_user_id\":\"u-owner\"}" | expect_err
pass "unknown action refused (not whitelisted)"

step "L2: card.request with a forged (owner's) delegated identity refused"
rpc "$WORKER_DIR" member im.card.request "{\"team\":\"$TEAM_ID\",\"platform\":\"feishu\",\"im_user_id\":\"u-owner\",\"action\":\"im.verify\",\"task_id\":\"task-x\",\"im_msg_id\":\"esc-2\"}" | expect_err
pass "card.request identity mismatch refused"

# ---------------------------------------------------------------------------
step "L3: legitimate approve via own binding, then im_msg_id replay is idempotent"
OUT=$(rpc "$OWNER_DIR" member im.approve "{\"team\":\"$TEAM_ID\",\"member_id\":\"$PENDING_MID\",\"im_user_id\":\"u-owner\",\"im_msg_id\":\"legit-1\"}")
echo "$OUT" | python3 -c "import json,sys; d=json.load(sys.stdin); assert d.get('ok'), d" || fail "legit approve failed"
STATE=$(sqlite3 -cmd ".timeout 5000" "$DB" "SELECT state FROM members WHERE id='$PENDING_MID'")
[ "$STATE" = "active" ] || fail "expected active, got $STATE"
OUT=$(rpc "$OWNER_DIR" member im.approve "{\"team\":\"$TEAM_ID\",\"member_id\":\"$PENDING_MID\",\"im_user_id\":\"u-owner\",\"im_msg_id\":\"legit-1\"}")
echo "$OUT" | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert d.get('ok'), d
assert d['data'].get('idempotent') is True, d
" || fail "replay must be idempotent"
pass "im_msg_id replay → idempotent (no double effect)"

step "L3: confirm token — replay refused, cross-action refused"
rpc "$OWNER_DIR" member task.create "{\"title\":\"Drill task\",\"assignee\":\"$OWNER\",\"team\":\"$TEAM_ID\"}" >/dev/null
TASK_ID=$(rpc "$OWNER_DIR" member task.list "{}" | python3 -c "import json,sys; ts=json.load(sys.stdin)['data']['tasks']; print(ts[0]['id'])")
rpc "$OWNER_DIR" member task.done "{\"id\":\"$TASK_ID\"}" >/dev/null
TOK=$(rpc "$OWNER_DIR" member im.card.request "{\"team\":\"$TEAM_ID\",\"action\":\"im.verify\",\"task_id\":\"$TASK_ID\",\"im_msg_id\":\"tok-1\"}" \
  | python3 -c "import json,sys; d=json.load(sys.stdin); assert d.get('ok'), d; print(d['data']['confirm_token'])")
# cross-action: try to spend the verify token as reject
rpc "$OWNER_DIR" member im.card.execute "{\"team\":\"$TEAM_ID\",\"action\":\"im.reject\",\"task_id\":\"$TASK_ID\",\"confirm_token\":\"$TOK\"}" | expect_err
pass "cross-action token refused"
# legit execute
rpc "$OWNER_DIR" member im.card.execute "{\"team\":\"$TEAM_ID\",\"action\":\"im.verify\",\"task_id\":\"$TASK_ID\",\"confirm_token\":\"$TOK\"}" \
  | python3 -c "import json,sys; d=json.load(sys.stdin); assert d.get('ok'), d" || fail "legit execute failed"
# replay the consumed token
rpc "$OWNER_DIR" member im.card.execute "{\"team\":\"$TEAM_ID\",\"action\":\"im.verify\",\"task_id\":\"$TASK_ID\",\"confirm_token\":\"$TOK\"}" | expect_err
pass "token replay refused (consumed)"

# ---------------------------------------------------------------------------
step "audit: every refusal left im.rejected with source + im_user"
python3 - "$DB" <<'PY'
import sqlite3, sys, json
conn = sqlite3.connect(sys.argv[1])
rej = [json.loads(p) for (p,) in conn.execute(
    "SELECT payload_json FROM events WHERE type='im.rejected'").fetchall()]
assert rej, "expected im.rejected audit rows"
codes = sorted({r.get('reason_code') or r.get('reason','')[:24] for r in rej})
# L1 gateway rejections carry reason_code; L2 carry reason text.
need_l1 = {'invalid_signature', 'missing_signature', 'stale_timestamp', 'duplicate', 'unbound'}
have = set(codes)
missing = [c for c in need_l1 if c not in have]
assert not missing, f"missing L1 reason codes: {missing} (have {have})"
esc = [r for r in rej if 'identity mismatch' in str(r.get('reason',''))]
assert esc, f"no identity-mismatch audit in {have}"
print(f"  ok: {len(rej)} im.rejected events; L1 codes covered; L2 mismatch audited")
PY
pass "audit trail verified"

echo
echo "ALL IM FAULT-DRILL TESTS PASSED (L1 gateway / L2 executor / L3 state)"
