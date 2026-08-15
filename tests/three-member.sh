#!/usr/bin/env bash
# teamx 三人协作 demo 自动化测试（owner + contributor + reviewer）。
# 覆盖：多成员审批、两类执行角色并行、澄清问答闭环、广播采纳、关闭+归档。
set -euo pipefail

TEAMX="${TEAMX:-$(dirname "$0")/../target/debug/teamx}"
DB="${1:-$(mktemp /tmp/teamx-3p-XXXXXX).db}"
export TEAMX_DB="$DB"
trap 'rm -f "$DB" "$DB-wal" "$DB-shm"' EXIT

step() { printf '\n=== %s ===\n' "$*"; }
pass() { printf '  ok: %s\n' "$*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }
expect_ok() { "$@" >/dev/null 2>&1 || fail "expected success: $*"; pass "$*"; }
expect_fail() { if "$@" >/dev/null 2>&1; then fail "expected failure: $*"; else pass "(rejected) $*"; fi }
jget() { python3 -c "import json,sys; print(json.load(sys.stdin)$1)"; }

step "init"
expect_ok "$TEAMX" init

step "owner creates team + goal"
CREATE=$($TEAMX team create "产品评审组" --session inst:owner \
  --goal-title "根据 requirement.md 完成产品方案设计并通过评审" --json)
[ -n "$CREATE" ] || fail "create"
TEAM_ID=$(echo "$CREATE" | jget "['team']['id']")
TOKEN=$(echo "$CREATE" | jget "['team']['invite_token']")

step "contributor joins + applies role (stays pending)"
JOIN1=$($TEAMX team join "$TOKEN" --name 设计者 --session inst:contrib --json)
CONTRIB=$(echo "$JOIN1" | jget "['member_id']")
"$TEAMX" role set contributor --session inst:contrib --json | jget "['state']" | grep -qx "pending" || fail "contributor should stay pending"

step "reviewer joins + applies role (stays pending)"
JOIN2=$($TEAMX team join "$TOKEN" --name 评审员 --session inst:review --json)
REVIEW=$(echo "$JOIN2" | jget "['member_id']")
"$TEAMX" role set reviewer --session inst:review --json | jget "['state']" | grep -qx "pending" || fail "reviewer should stay pending"

step "owner approves both + shares goal"
expect_ok "$TEAMX" team approve "$CONTRIB" --session inst:owner
expect_ok "$TEAMX" team approve "$REVIEW" --session inst:owner
expect_ok "$TEAMX" goal share --session inst:owner
"$TEAMX" team status --team "$TEAM_ID" --json | python3 -c "
import json,sys
members=json.load(sys.stdin)['teams'][0]['members']
roles=sorted(m['role'] for m in members)
assert roles==['contributor','owner','reviewer'], roles
states=sorted(m['state'] for m in members)
assert states==['active','active','active'], states
print('  members:', roles, states)
"

step "contributor publishes design progress"
expect_ok "$TEAMX" publish progress --data '{"message":"设计方案完成，见 design-plan.md"}' --session inst:contrib

step "reviewer syncs (sees contributor progress) then publishes review progress"
R_SYNC=$($TEAMX sync --session inst:review --json)
echo "$R_SYNC" | python3 -c "import json,sys; d=json.load(sys.stdin); assert any(e['type']=='progress.published' for e in d['new_events']), d"
expect_ok "$TEAMX" publish progress --data '{"message":"评审完成，见 review-plan.md"}' --session inst:review

step "owner asks contributor a clarification; contributor responds"
ASK=$($TEAMX ask "$CONTRIB" --question "存储层为什么选 SQLite?" --session inst:owner --json)
ASK_ID=$(echo "$ASK" | jget "['question_id']")
"$TEAMX" team status --team "$TEAM_ID" --json | python3 -c "import json,sys; d=json.load(sys.stdin)['teams'][0]['members']; assert any(m['id']=='$CONTRIB' and m['state']=='waiting' for m in d)"
expect_ok "$TEAMX" respond "$ASK_ID" --answer "单文件、无运行期依赖" --session inst:contrib

step "owner broadcasts decision (adopt review feedback)"
expect_ok "$TEAMX" publish decision --data '{"message":"采纳评审意见 P0/P1"}' --session inst:owner

step "contributor reports achieved; owner closes + archives"
expect_ok "$TEAMX" publish achieved --data '{"message":"方案定稿"}' --session inst:contrib
expect_ok "$TEAMX" goal close --session inst:owner
expect_ok "$TEAMX" team archive --session inst:owner

step "final state = archived / closed"
FINAL=$($TEAMX team status --team "$TEAM_ID" --json | python3 -c "import json,sys; t=json.load(sys.stdin)['teams'][0]; print(t['team']['state'], t['goal']['state'])")
[ "$FINAL" = "archived closed" ] || fail "expected 'archived closed', got '$FINAL'"
pass "team/goal = $FINAL"

step "event chain includes all key transitions"
EVS=$($TEAMX events --team "$TEAM_ID" --json | python3 -c "import json,sys; print([e['type'] for e in json.load(sys.stdin)['events']])")
echo "  events: $EVS"
for t in membership.pending membership.approved member.role_set goal.shared progress.published clarification.asked clarification.responded decision.broadcast goal.achieved team.completed team.state_changed; do
  echo "$EVS" | grep -q "$t" || fail "missing event type $t"
done
pass "all key event types present"

step "THREE-MEMBER DEMO TEST PASS"
