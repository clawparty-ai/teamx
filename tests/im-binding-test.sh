#!/usr/bin/env bash
# IM identity binding (P1) — local CLI acceptance.
#
# Drives the `teamx im` surface end to end without a server, which is the P1
# path (the gateway runs in-process; design §5.3 option A):
#   bind-code -> bind (role echoed) -> single use -> list (self vs lead)
#   -> resolve -> unbind -> rejections (peer, reuse, cross-team, throttle)
#   -> ledger events (im.binding / im.rejected with source: im:<platform>)
#
# Requires: python3.
set -euo pipefail

# Accept both the TEAMX used by the other suites and run-all.sh's TEAMX_BIN.
TEAMX="${TEAMX:-${TEAMX_BIN:-$(dirname "$0")/../target/debug/teamx}}"
DB="${1:-$(mktemp /tmp/teamx-im-XXXXXX).db}"
HOME_DIR="$(mktemp -d /tmp/teamx-im-home-XXXXXX)"
export TEAMX_DB="$DB"
export TEAMX_HOME="$HOME_DIR"
trap 'rm -f "$DB" "$DB-wal" "$DB-shm"; rm -rf "$HOME_DIR"' EXIT

step() { printf '\n=== %s ===\n' "$*"; }
pass() { printf '  ok: %s\n' "$*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }
expect_ok() { "$@" >/dev/null 2>&1 || fail "expected success: $*"; pass "$*"; }
expect_fail() { if "$@" >/dev/null 2>&1; then fail "expected failure: $*"; else pass "(rejected) $*"; fi }
jget() { python3 -c "import json,sys; print(json.load(sys.stdin)$1)"; }
# error text of a command that must fail
jerr() { "$@" 2>&1 || true; }

step "setup: team + owner + one member"
"$TEAMX" init >/dev/null
CREATE=$("$TEAMX" team create "IMTeam" --session s:owner --json)
TEAM_ID=$(echo "$CREATE" | jget "['team']['id']")
TOKEN=$(echo "$CREATE" | jget "['team']['invite_token']")
"$TEAMX" goal set "IM binding" --session s:owner >/dev/null
"$TEAMX" team join "$TOKEN" --name Worker --session s:worker >/dev/null
"$TEAMX" team join "$TOKEN" --name Peer --session s:peer >/dev/null
"$TEAMX" role set reviewer --session s:worker --team "$TEAM_ID" >/dev/null
STATUS=$("$TEAMX" team status --session s:owner --json)
WORKER_ID=$(echo "$STATUS" | python3 -c "import json,sys; d=json.load(sys.stdin); print([m['id'] for m in d['teams'][0]['members'] if m['display_name']=='Worker'][0])")
WORKER_ROLE=$(echo "$STATUS" | python3 -c "import json,sys; d=json.load(sys.stdin); print([m['role'] for m in d['teams'][0]['members'] if m['display_name']=='Worker'][0])")
pass "team=$TEAM_ID worker=$WORKER_ID role=$WORKER_ROLE"

step "bind-code: self-service, single-use, TTL-bounded"
CODE_JSON=$("$TEAMX" im bind-code --session s:worker --team "$TEAM_ID" --json)
CODE=$(echo "$CODE_JSON" | jget "['code']")
[ "${#CODE}" = "10" ] || fail "code length should be 10, got ${#CODE}"
[ "$(echo "$CODE_JSON" | jget "['member_id']")" = "$WORKER_ID" ] || fail "code must be pre-associated with the creating member"
[ -n "$(echo "$CODE_JSON" | jget "['expires_at']")" ] || fail "expires_at missing"
pass "code issued for $WORKER_ID (10 chars, expires $(echo "$CODE_JSON" | jget "['expires_at']"))"
# the TTL is clamped into [60, 900] rather than trusting the caller
[ "$("$TEAMX" im bind-code --session s:worker --team "$TEAM_ID" --ttl-secs 5 --json | jget "['ttl_secs']")" = "60" ] || fail "ttl must clamp up to 60"
[ "$("$TEAMX" im bind-code --session s:worker --team "$TEAM_ID" --ttl-secs 100000 --json | jget "['ttl_secs']")" = "900" ] || fail "ttl must clamp down to 900"
pass "ttl clamped to [60, 900]"
expect_fail "$TEAMX" im bind-code --session s:stranger --team "$TEAM_ID"

step "bind: identity resolves and the role is echoed"
BIND=$("$TEAMX" im bind "$CODE" --im-user ou_worker --session s:worker --team "$TEAM_ID" --json)
[ "$(echo "$BIND" | jget "['action']")" = "bind" ] || fail "first redemption should be a bind"
[ "$(echo "$BIND" | jget "['member']['id']")" = "$WORKER_ID" ] || fail "bound member mismatch"
[ "$(echo "$BIND" | jget "['member']['role']")" = "$WORKER_ROLE" ] || fail "role must be echoed back"
pass "ou_worker -> $WORKER_ID ($WORKER_ROLE)"

step "bind: the code is single use"
REUSE=$(jerr "$TEAMX" im bind "$CODE" --im-user ou_other --session s:worker --team "$TEAM_ID")
echo "$REUSE" | grep -q "code_used" || fail "expected code_used, got: $REUSE"
pass "second redemption rejected (code_used)"

step "bindings: self scope by default, --all is lead-only"
MINE=$("$TEAMX" im bindings --session s:worker --team "$TEAM_ID" --json)
[ "$(echo "$MINE" | jget "['count']")" = "1" ] || fail "member should see exactly their own binding"
[ "$(echo "$MINE" | jget "['bindings'][0]['im_user_id']")" = "ou_worker" ] || fail "wrong binding listed"
[ "$(echo "$MINE" | jget "['scope']")" = "self" ] || fail "default scope must be self"
pass "self scope shows 1 binding"
expect_fail "$TEAMX" im bindings --all --session s:worker --team "$TEAM_ID"
ALL=$("$TEAMX" im bindings --all --session s:owner --team "$TEAM_ID" --json)
[ "$(echo "$ALL" | jget "['count']")" = "1" ] || fail "owner should see the team's bindings"
[ "$(echo "$ALL" | jget "['scope']")" = "team" ] || fail "--all must report team scope"
pass "owner sees the team scope (1 binding)"

step "resolve: lead-only identity lookup (what the gateway uses)"
expect_fail "$TEAMX" im resolve --im-user ou_worker --session s:worker --team "$TEAM_ID"
R1=$("$TEAMX" im resolve --im-user ou_worker --session s:owner --team "$TEAM_ID" --json)
[ "$(echo "$R1" | jget "['bound']")" = "True" ] || fail "expected bound=true"
[ "$(echo "$R1" | jget "['member']['member_id']")" = "$WORKER_ID" ] || fail "resolve returned the wrong member"
R2=$("$TEAMX" im resolve --im-user ou_nobody --session s:owner --team "$TEAM_ID" --json)
[ "$(echo "$R2" | jget "['bound']")" = "False" ] || fail "unknown IM user must be unbound"
pass "bound identity resolves, unknown identity does not"

step "rebind: the same IM identity moves to another member"
OWNER_CODE=$("$TEAMX" im bind-code --session s:owner --team "$TEAM_ID" --json | jget "['code']")
REBIND=$("$TEAMX" im bind "$OWNER_CODE" --im-user ou_worker --session s:owner --team "$TEAM_ID" --json)
[ "$(echo "$REBIND" | jget "['action']")" = "rebind" ] || fail "expected a rebind"
[ "$(echo "$REBIND" | jget "['previous']['member_id']")" = "$WORKER_ID" ] || fail "rebind must report the previous holder"
[ "$("$TEAMX" im bindings --session s:worker --team "$TEAM_ID" --json | jget "['count']")" = "0" ] || fail "the previous holder must keep no binding"
[ "$("$TEAMX" im resolve --im-user ou_worker --session s:owner --team "$TEAM_ID" --json | jget "['member']['role']")" = "owner" ] || fail "identity should now resolve to the owner"
pass "ou_worker moved $WORKER_ID -> owner (one active binding kept)"

step "cross-team redemption is refused"
CREATE2=$("$TEAMX" team create "IMTeam2" --session s:owner2 --json)
TEAM2=$(echo "$CREATE2" | jget "['team']['id']")
TOKEN2=$(echo "$CREATE2" | jget "['team']['invite_token']")
"$TEAMX" team join "$TOKEN2" --name Worker2 --session s:worker2 >/dev/null
CODE2=$("$TEAMX" im bind-code --session s:worker --team "$TEAM_ID" --json | jget "['code']")
CROSS=$(jerr "$TEAMX" im bind "$CODE2" --im-user ou_cross --session s:worker2 --team "$TEAM2")
echo "$CROSS" | grep -q "team_mismatch" || fail "expected team_mismatch, got: $CROSS"
pass "a code issued for $TEAM_ID cannot be redeemed in $TEAM2"

step "throttle: five failures block the IM identity"
for _ in 1 2 3 4 5; do
  "$TEAMX" im bind AAAAAAAAAA --im-user ou_attacker --session s:worker --team "$TEAM_ID" >/dev/null 2>&1 || true
done
GOOD=$("$TEAMX" im bind-code --session s:worker --team "$TEAM_ID" --json | jget "['code']")
THROTTLED=$(jerr "$TEAMX" im bind "$GOOD" --im-user ou_attacker --session s:worker --team "$TEAM_ID")
echo "$THROTTLED" | grep -q "rate_limited" || fail "expected rate_limited, got: $THROTTLED"
pass "6th attempt throttled even with a valid code"

step "unbind: peer refused, self-service works"
# a same-team peer (neither the bound member nor a lead) may not unbind
expect_fail "$TEAMX" im unbind --im-user ou_worker --session s:peer --team "$TEAM_ID"
expect_fail "$TEAMX" im unbind --im-user ou_worker --session s:worker --team "$TEAM_ID"
UNBIND=$("$TEAMX" im unbind --im-user ou_worker --session s:owner --team "$TEAM_ID" --json)
[ "$(echo "$UNBIND" | jget "['unbound']")" = "True" ] || fail "unbind should report unbound"
R3=$("$TEAMX" im resolve --im-user ou_worker --session s:owner --team "$TEAM_ID" --json)
[ "$(echo "$R3" | jget "['bound']")" = "False" ] || fail "identity must stop resolving after unbind"
expect_fail "$TEAMX" im unbind --im-user ou_worker --session s:owner --team "$TEAM_ID"
pass "unbind revoked the identity (second attempt is an error, not a no-op)"

step "ledger: every bind/unbind/rejection is auditable"
EVENTS=$("$TEAMX" events --team "$TEAM_ID" --json)
echo "$EVENTS" | python3 -c "
import json,sys
events=json.load(sys.stdin)['events']
types={}
for e in events:
    types.setdefault(e['type'],[]).append(e)
assert 'im.binding' in types, f'no im.binding event: {sorted(types)}'
assert 'im.rejected' in types, f'no im.rejected event: {sorted(types)}'
actions=sorted(p['payload']['action'] for p in types['im.binding'])
assert actions==['bind','rebind','unbind'], actions
reasons=sorted({p['payload']['detail'] for p in types['im.rejected']})
assert 'code_used' in reasons and 'team_mismatch' in reasons and 'rate_limited' in reasons, reasons
for e in types['im.binding']+types['im.rejected']:
    if e['type']=='im.binding' or e['payload'].get('platform'):
        assert e['payload'].get('source')=='im:feishu', e['payload']
print('  ok: im.binding actions =', actions)
print('  ok: im.rejected details =', reasons)
"
pass "ledger carries im.binding + im.rejected with source: im:feishu"

printf '\nIM binding (P1) CLI acceptance: all checks passed\n'
