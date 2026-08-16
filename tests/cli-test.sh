#!/usr/bin/env bash
# teamx V1 CLI edge & negative test suite.
# Covers: illegal transitions, authorization, duplicate joins, bad tokens,
# role validation, publish type validation, multi-team disambiguation,
# cursor behavior, leave/deny flows, and the loopx bridge.
set -euo pipefail

TEAMX="${TEAMX:-$(dirname "$0")/../target/debug/teamx}"
DB="${1:-$(mktemp /tmp/teamx-cli-XXXXXX).db}"
export TEAMX_DB="$DB"
trap 'rm -f "$DB" "$DB-wal" "$DB-shm"' EXIT

step() { printf '\n=== %s ===\n' "$*"; }
pass() { printf '  ok: %s\n' "$*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }
expect_ok() { "$@" >/dev/null 2>&1 || fail "expected success: $*"; pass "$*"; }
expect_fail() { if "$@" >/dev/null 2>&1; then fail "expected failure: $*"; else pass "(rejected) $*"; fi }
jget() { python3 -c "import json,sys; print(json.load(sys.stdin)$1)"; }

step "init (idempotent)"
expect_ok "$TEAMX" init
expect_ok "$TEAMX" init

step "create team + goal"
CREATE=$($TEAMX team create "Edge" --session s:owner --json)
[ -n "$CREATE" ] || fail "create"
TEAM_ID=$(echo "$CREATE" | jget "['team']['id']")
TOKEN=$(echo "$CREATE" | jget "['team']['invite_token']")
OWNER=$(echo "$CREATE" | jget "['owner_member_id']")
expect_ok "$TEAMX" goal set "Goal" --session s:owner

step "create_team same name reuses (idempotency regression)"
REUSED=$($TEAMX team create "Edge" --session s:owner --json)
[ "$(echo "$REUSED" | jget "['reused']")" = "True" ] || fail "same-name create should reuse"
[ "$(echo "$REUSED" | jget "['team']['id']")" = "$TEAM_ID" ] || fail "reuse should return the same team id"

step "owner cannot leave (orphan protection regression)"
expect_fail "$TEAMX" team leave --session s:owner

step "join with bad token"
expect_fail "$TEAMX" team join "bogus-token" --name X --session s:x

step "duplicate join by the same session"
expect_ok "$TEAMX" team join "$TOKEN" --name Bob --session s:bob
expect_fail "$TEAMX" team join "$TOKEN" --name Bob2 --session s:bob

step "non-owner cannot approve / share / close / set role on behalf"
BOB=$($TEAMX team status --team "$TEAM_ID" --json | jget "['teams'][0]['members'][1]['id']")
expect_fail "$TEAMX" team approve "$BOB" --session s:bob
expect_fail "$TEAMX" goal share --session s:bob
expect_fail "$TEAMX" goal close --session s:bob
expect_fail "$TEAMX" role set contributor --member "$BOB" --session s:bob

step "owner approve; then deny of a non-pending member"
expect_ok "$TEAMX" team approve "$BOB" --session s:owner
expect_fail "$TEAMX" team deny "$BOB" --session s:owner

step "pending member can apply a role but stays pending (regression: demo reviewer flow)"
expect_ok "$TEAMX" team join "$TOKEN" --name Eve --session s:eve
EVE=$($TEAMX team status --team "$TEAM_ID" --json | jget "['teams'][0]['members'][2]['id']")
"$TEAMX" role set reviewer --session s:eve --json | jget "['state']" | grep -qx "pending" || fail "pending set_role must stay pending (approval bypass)"
"$TEAMX" team status --team "$TEAM_ID" --json | jget "['teams'][0]['members'][2]['role']" | grep -qx "reviewer" || fail "role should be recorded while pending"
expect_ok "$TEAMX" team approve "$EVE" --session s:owner
"$TEAMX" team status --team "$TEAM_ID" --json | jget "['teams'][0]['members'][2]['state']" | grep -qx "active" || fail "eve should be active after approval"
"$TEAMX" team status --team "$TEAM_ID" --json | jget "['teams'][0]['members'][2]['role']" | grep -qx "reviewer" || fail "reviewer role retained after approval"

step "deny flow (second member)"
expect_ok "$TEAMX" team join "$TOKEN" --name Carol --session s:carol
CAROL=$($TEAMX team status --team "$TEAM_ID" --json | jget "['teams'][0]['members'][3]['id']")
expect_ok "$TEAMX" team deny "$CAROL" --session s:owner
DENIED=$($TEAMX team status --team "$TEAM_ID" --json | jget "['teams'][0]['members'][3]['state']")
[ "$DENIED" = "denied" ] || fail "carol should be denied, got $DENIED"
# denied member cannot sync as a member
expect_fail "$TEAMX" sync --session s:carol

step "unknown role"
expect_fail "$TEAMX" role set wizard --session s:bob

step "custom role: propose → not usable → approve → granted + usable → update → deny"
# member proposes a custom role
expect_ok "$TEAMX" role propose devops "DevOps" "负责 CI/CD 与基础设施" --session s:bob
# role not usable until approved
expect_fail "$TEAMX" role set devops --session s:bob
# key conflicts with built-in role
expect_fail "$TEAMX" role propose owner "Owner2" "dup" --session s:bob
# duplicate propose
expect_fail "$TEAMX" role propose devops "DevOps2" "dup" --session s:bob
# non-owner cannot approve/deny/update
expect_fail "$TEAMX" role approve devops --session s:bob
expect_fail "$TEAMX" role deny devops --session s:bob
expect_fail "$TEAMX" role update devops --description "hack" --session s:bob
# owner approves; proposer (bob) is auto-granted devops
expect_ok "$TEAMX" role approve devops --session s:owner
"$TEAMX" team status --team "$TEAM_ID" --json | jget "['teams'][0]['members'][1]['role']" | grep -qx "devops" || fail "bob should be auto-granted devops after approval"
# after approval the custom role is usable by any member (eve)
"$TEAMX" role set devops --session s:eve --json | jget "['role']" | grep -qx "devops" || fail "approved custom role should be usable"
"$TEAMX" role set reviewer --session s:eve || true
# owner updates the role description (label preserved)
"$TEAMX" role update devops --description "负责 CI/CD、基础设施与监控" --session s:owner --json | jget "['description']" | grep -qx "负责 CI/CD、基础设施与监控" || fail "description should update"
# owner edits a member's role back to contributor
expect_ok "$TEAMX" role set contributor --member "$BOB" --session s:owner
# another proposed role can be denied
expect_ok "$TEAMX" role propose tmp-role "Tmp" "temp" --session s:eve
expect_ok "$TEAMX" role deny tmp-role --session s:owner
expect_fail "$TEAMX" role approve tmp-role --session s:owner

step "set role + share goal"
expect_ok "$TEAMX" role set contributor --session s:bob
expect_ok "$TEAMX" goal share --session s:owner

step "neutral broadcasts succeed in ANY goal state (regression)"
# decision/update/activity must not attempt goal/team transitions
expect_ok "$TEAMX" publish decision --data '{"m":"shared 状态下广播"}' --session s:owner
expect_ok "$TEAMX" publish update --data '{"m":"update"}' --session s:bob
expect_ok "$TEAMX" publish activity --data '{"m":"activity"}' --session s:bob

step "publish validation"
# unknown type
expect_fail "$TEAMX" publish "teleport" --session s:bob
# bare (non-JSON) --data falls back to {"message": ...} (regression)
expect_ok "$TEAMX" publish decision --data "plain text not json" --session s:bob
# start while goal already in_progress-ish (start from shared is fine; a second start from in_progress is also fine)
expect_ok "$TEAMX" publish start --session s:owner
# blocked -> resumed
expect_ok "$TEAMX" publish blocked --data '{"why":"ci red"}' --session s:bob
BLOCKED_STATE=$($TEAMX team status --team "$TEAM_ID" --json | jget "['teams'][0]['team']['state']")
[ "$BLOCKED_STATE" = "blocked" ] || fail "team should be blocked, got $BLOCKED_STATE"
expect_ok "$TEAMX" publish resumed --data '{"why":"ci green"}' --session s:bob

step "publish --assignee directed task (regression: only assignee auto-executes)"
# assignee must be a valid team member
expect_fail "$TEAMX" publish decision --assignee "nonexistent-id" --session s:owner
# directed publish carries assignee_member_id in the event payload
expect_ok "$TEAMX" publish decision --data '{"message":"do X"}' --assignee "$BOB" --session s:owner
LAST_EVENT=$($TEAMX events --team "$TEAM_ID" --json | jget "['events'][-1]")
echo "$LAST_EVENT" | grep -q "assignee_member_id" || fail "directed publish must carry assignee_member_id"
echo "$LAST_EVENT" | grep -q "$BOB" || fail "assignee_member_id should equal target member"
# unassigned publish carries no assignee
expect_ok "$TEAMX" publish decision --data '{"message":"general notice"}' --session s:owner
LAST_EVENT2=$($TEAMX events --team "$TEAM_ID" --json | jget "['events'][-1]")
if echo "$LAST_EVENT2" | grep -q "assignee_member_id"; then
  fail "unassigned publish must NOT carry assignee_member_id"
fi

step "ask/respond validation"
expect_fail "$TEAMX" ask "$OWNER" --question "?" --session s:owner   # ask self
expect_ok "$TEAMX" ask "$BOB" --question "clarify scope?" --session s:owner
ASK_ID=$($TEAMX events --team "$TEAM_ID" --json | jget "['events'][-1]['payload']['question_id']")
expect_fail "$TEAMX" respond "$ASK_ID" --answer "x" --session s:owner  # not the target
expect_ok "$TEAMX" respond "$ASK_ID" --answer "within scope" --session s:bob
expect_fail "$TEAMX" respond "$ASK_ID" --answer "again" --session s:bob  # already answered
# respond to an unknown id
expect_fail "$TEAMX" respond "no-such-id" --answer "x" --session s:bob

step "publish before goal set fails (fresh team)"
CREATE2=$($TEAMX team create "NoGoal" --session s:o2 --json)
TOKEN2=$(echo "$CREATE2" | jget "['team']['invite_token']")
expect_ok "$TEAMX" team join "$TOKEN2" --name M --session s:m2
expect_fail "$TEAMX" publish progress --session s:m2   # no goal set

step "multi-team session requires --team"
TEAM2_ID=$(echo "$CREATE2" | jget "['team']['id']")
# join BOTH teams so the session has two memberships
expect_ok "$TEAMX" team join "$TOKEN" --name Multi --session s:multi
expect_ok "$TEAMX" team join "$TOKEN2" --name Multi --session s:multi
expect_fail "$TEAMX" team status --session s:multi
expect_fail "$TEAMX" publish decision --session s:multi
expect_fail "$TEAMX" publish decision --session s:multi --team "not-a-team"
# with explicit --team it works
expect_ok "$TEAMX" publish decision --session s:multi --team "$TEAM2_ID"

step "one session cannot create two teams (regression: owner-uniqueness)"
# s:o3 owns one team; creating a second team with a different name must fail.
expect_ok "$TEAMX" team create "OnlyTeam" --session s:o3
expect_fail "$TEAMX" team create "SecondTeam" --session s:o3
# after archiving the owned team the same session may create again
ONLY_ID=$($TEAMX team status --session s:o3 --json | jget "['teams'][0]['team']['id']")
expect_ok "$TEAMX" goal set "G" --session s:o3
expect_ok "$TEAMX" goal share --session s:o3
expect_ok "$TEAMX" publish start --session s:o3
expect_ok "$TEAMX" publish achieved --session s:o3
expect_ok "$TEAMX" goal close --session s:o3
expect_ok "$TEAMX" team archive --session s:o3
expect_ok "$TEAMX" team create "AfterArchive" --session s:o3

step "multi-team owner can approve with --team (regression)"
# a session may still be a MEMBER of another team while owning one; the owner
# session must pass --team when its membership spans multiple teams.
CREATE5=$($TEAMX team create "OwnerTeamA" --session s:owner2 --json)
TEAM5A=$(echo "$CREATE5" | jget "['team']['id']")
TOKEN5A=$(echo "$CREATE5" | jget "['team']['invite_token']")
CREATE6=$($TEAMX team create "MemberTeamB" --session s:owner3 --json)
TEAM6B=$(echo "$CREATE6" | jget "['team']['id']")
TOKEN6B=$(echo "$CREATE6" | jget "['team']['invite_token']")
# s:owner2 joins MemberTeamB as a plain member (allowed: owner of A, member of B)
expect_ok "$TEAMX" team join "$TOKEN6B" --name OwnerAsMember --session s:owner2
expect_ok "$TEAMX" team join "$TOKEN5A" --name Zed --session s:zed
ZED=$($TEAMX team status --team "$TEAM5A" --json | jget "['teams'][0]['members'][1]['id']")
expect_fail "$TEAMX" team approve "$ZED" --session s:owner2               # ambiguous (two teams)
expect_ok "$TEAMX" team approve "$ZED" --session s:owner2 --team "$TEAM5A"

step "sync cursor behavior"
SYNC1=$($TEAMX sync --session s:bob --json)
N1=$(echo "$SYNC1" | jget "['new_events']")
SYNC2=$($TEAMX sync --session s:bob --json)
N2=$(echo "$SYNC2" | jget "['new_events']")
[ "$N1" != "[]" ] || fail "first sync should return events"
[ "$N2" = "[]" ] || fail "second sync should return nothing (cursor advanced)"

# --no-advance must NOT advance the cursor: emit a new event, then two
# consecutive --no-advance syncs must both return it.
expect_ok "$TEAMX" publish decision --data '{"cursor":"probe"}' --session s:owner
N3=$($TEAMX sync --session s:bob --no-advance --json | jget "['new_events']")
N4=$($TEAMX sync --session s:bob --no-advance --json | jget "['new_events']")
[ "$N3" != "[]" ] || fail "--no-advance should return the new event"
[ "$N4" != "[]" ] || fail "--no-advance must not advance; same event again expected"
# a normal sync advances, so the following one is empty again
N5=$($TEAMX sync --session s:bob --json | jget "['new_events']")
N6=$($TEAMX sync --session s:bob --json | jget "['new_events']")
[ "$N5" != "[]" ] || fail "normal sync should return the pending event"
[ "$N6" = "[]" ] || fail "normal sync should advance the cursor"

step "events requires --team"
expect_fail "$TEAMX" events

step "teamx log (audit replay) resolves member names"
LOG_OUT=$($TEAMX log --team "$TEAM_ID" --limit 1 --json)
echo "$LOG_OUT" | jget "['events'][0]['member']" | grep -q "Edge" || fail "log should resolve owner display name"
# log --session resolves single-team session
expect_ok "$TEAMX" log --session s:owner
# log requires --team or --session
expect_fail "$TEAMX" log

step "leave flow"
expect_ok "$TEAMX" team leave --session s:bob
# leaving again fails (no membership)
expect_fail "$TEAMX" team leave --session s:bob
LEFT_STATE=$($TEAMX team status --team "$TEAM_ID" --json | jget "['teams'][0]['members'][1]['state']")
[ "$LEFT_STATE" = "left" ] || fail "bob should be left, got $LEFT_STATE"

step "member rejoin reactivates the same row (regression)"
expect_ok "$TEAMX" team join "$TOKEN" --name Bob2 --session s:bob
# bob re-applies → still exactly one member row for (team, s:bob)
BOB_ROWS=$($TEAMX team status --team "$TEAM_ID" --json | python3 -c "
import json,sys
d=json.load(sys.stdin)['teams'][0]['members']
print(sum(1 for m in d if m['display_name'] in ('Bob','Bob2')))")
[ "$BOB_ROWS" = "1" ] || fail "rejoin must reactivate the existing member row, got $BOB_ROWS rows"

step "member set-state idle/active (regression)"
expect_ok "$TEAMX" member set-state idle --session s:owner
"$TEAMX" team status --team "$TEAM_ID" --json | jget "['teams'][0]['members'][0]['state']" | grep -qx "idle" || fail "owner should be idle"
expect_ok "$TEAMX" member set-state active --session s:owner
"$TEAMX" team status --team "$TEAM_ID" --json | jget "['teams'][0]['members'][0]['state']" | grep -qx "active" || fail "owner should be active again"
# non-owner member cannot set another member's state
expect_fail "$TEAMX" member set-state idle --member "$OWNER" --session s:eve

step "close goal by owner after leaving member"
expect_ok "$TEAMX" publish achieved --session s:owner
expect_ok "$TEAMX" goal close --session s:owner
FINAL=$($TEAMX team status --team "$TEAM_ID" --json | jget "['teams'][0]['team']['state']")
[ "$FINAL" = "completed" ] || fail "team should be completed, got $FINAL"

step "archive a completed team (regression)"
expect_ok "$TEAMX" team archive --session s:owner
"$TEAMX" team status --team "$TEAM_ID" --json | jget "['teams'][0]['team']['state']" | grep -qx "archived" || fail "team should be archived"

step "completed team rejects new joins"
expect_fail "$TEAMX" team join "$TOKEN" --name Dave --session s:dave

step "loopx bridge (unavailable path)"
LOOPX_ERR=$($TEAMX loopx report /nonexistent-dir --session s:owner --json | jget "['ok']")
[ "$LOOPX_ERR" = "False" ] || fail "loopx report should be unavailable"
# non-member session cannot report
expect_fail "$TEAMX" loopx report /nonexistent-dir --session s:stranger

step "ALL EDGE TESTS PASS"
