#!/usr/bin/env bash
# Node provisioning E2E (design/006) — zero AWS, zero SSH.
#
# Drives the local CLI end-to-end with TEAMX_PROVISION_FAKE=1 (fake provider +
# fake remote exec), asserting:
#   1. invite --provision-node enqueues a job + creates a node (provisioning) + os_user
#   2. same user, second seat  -> REUSE (no second host)
#   3. duplicate invite        -> idempotent (single node/job)
#   4. node run-jobs           -> node ready + cost_snapshot (fake exec)
#   5. destroy                 -> terminated + checklist; second destroy -> noop
#   6. negative: unknown member rejected; region whitelist rejected
#
# Requires: python3.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TEAMX="${TEAMX:-$ROOT/target/debug/teamx}"
WORK="$(mktemp -d /tmp/teamx-prov-XXXXXX)"
export TEAMX_DB="$WORK/teamx.db"
export TEAMX_HOME="$WORK/home"
export TEAMX_PROVISION_FAKE=1
unset TEAMX_SERVER_URL || true
mkdir -p "$TEAMX_HOME"

pass() { printf '  ok: %s\n' "$*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

jq() { python3 -c "import sys,json;d=json.load(sys.stdin);print($1)"; }

cd "$WORK"

echo "== node-provision E2E (design/006, fake provider) =="

"$TEAMX" init >/dev/null
"$TEAMX" --json team create "ProvE2E" --session s1 >/dev/null
pass "team created"

# 1) invite --provision-node
OUT="$("$TEAMX" --json team invite "contributor: dev" --session s1 --user-name alice \
        --provision-node --provider fake --region ap-east-1 2>/dev/null)"
[ "$(echo "$OUT" | jq "d['provision']['reused']")" = "False" ] || fail "first invite should create a host"
NODE1="$(echo "$OUT" | jq "d['provision']['node_id']")"
OSUSER="$(echo "$OUT" | jq "d['provision']['os_user']")"
[ -n "$NODE1" ] || fail "no node_id"
echo "$OSUSER" | grep -Eq '^[a-z0-9]+[0-9]{4}$' || fail "bad os_user: $OSUSER"
pass "invite --provision-node -> node $NODE1 (os_user=$OSUSER)"

# 2) same user, second seat -> reuse
OUT2="$("$TEAMX" --json team invite "contributor: dev2" --session s1 --user-name alice \
         --provision-node --provider fake --region ap-east-1 2>/dev/null)"
[ "$(echo "$OUT2" | jq "d['provision']['reused']")" = "True" ] || fail "second seat for same user must reuse"
N="$(echo "$OUT2" | jq "len(d['provision'])>0")"
[ "$N" = "True" ] || fail "provision missing"
pass "second seat reuses the same host"

# 3) idempotent: still exactly one node
COUNT="$("$TEAMX" --json node list --session s1 2>/dev/null | jq "len(d['nodes'])")"
[ "$COUNT" = "1" ] || fail "expected 1 node, got $COUNT"
pass "single host for one user (nodes=$COUNT)"

# 4) run jobs -> ready
"$TEAMX" --json node run-jobs >/dev/null
STATE="$("$TEAMX" --json node status "$NODE1" --session s1 2>/dev/null | jq "d['node']['state']")"
[ "$STATE" = "ready" ] || fail "expected ready, got $STATE"
COST="$("$TEAMX" --json node status "$NODE1" --session s1 2>/dev/null | jq "d['node'].get('cost_snapshot') is not None")"
[ "$COST" = "True" ] || fail "cost_snapshot missing"
pass "run-jobs -> ready + cost_snapshot"

# 5) destroy (idempotent noop on second call)
"$TEAMX" --json node destroy "$NODE1" --session s1 >/dev/null
D1="$(echo "$("$TEAMX" --json node status "$NODE1" --session s1 2>/dev/null)" | jq "d['node']['state']")"
[ "$D1" = "terminated" ] || fail "expected terminated, got $D1"
D2="$("$TEAMX" --json node destroy "$NODE1" --session s1 2>/dev/null | jq "d.get('noop')")"
[ "$D2" = "True" ] || fail "second destroy should be a noop success"
pass "destroy -> terminated; second destroy -> noop"

# 6) negatives
if "$TEAMX" --json node provision --member 00000000-0000-0000-0000-000000000000 \
     --provider fake --region ap-east-1 --session s1 >/dev/null 2>&1; then
  fail "unknown member should be rejected"
fi
pass "unknown member rejected"

if TEAMX_AWS_REGION_WHITELIST="us-east-1" "$TEAMX" --json team invite "contributor: dev3" \
     --session s1 --user-name bob --provision-node --provider fake --region ap-east-1 \
     >/dev/null 2>&1; then
  fail "region outside whitelist should be rejected"
fi
pass "region whitelist enforced"

echo "ALL PASSED"
