#!/usr/bin/env bash
# teamx V1 dual-session closed-loop smoke test.
# Usage: tests/smoke.sh [DB_PATH]
set -euo pipefail

TEAMX="${TEAMX:-$(dirname "$0")/../target/debug/teamx}"
DB="${1:-$(mktemp /tmp/teamx-smoke-XXXXXX).db}"
export TEAMX_DB="$DB"
trap 'rm -f "$DB" "$DB-wal" "$DB-shm"' EXIT

step() { printf '\n=== %s ===\n' "$*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

step "init"
"$TEAMX" init --json | grep -q '"ok": true' || fail "init"

step "create team (owner Alice)"
CREATE=$("$TEAMX" team create "Test Team" --session inst:alice --json)
echo "$CREATE"
TOKEN=$(echo "$CREATE" | python3 -c "import json,sys; print(json.load(sys.stdin)['team']['invite_token'])")
TEAM_ID=$(echo "$CREATE" | python3 -c "import json,sys; print(json.load(sys.stdin)['team']['id'])")
ALICE_MEMBER=$(echo "$CREATE" | python3 -c "import json,sys; print(json.load(sys.stdin)['owner_member_id'])")

step "join as Bob"
JOIN=$("$TEAMX" team join "$TOKEN" --name Bob --session inst:bob --json)
echo "$JOIN"
BOB_MEMBER=$(echo "$JOIN" | python3 -c "import json,sys; print(json.load(sys.stdin)['member_id'])")

step "Bob sync while pending"
BOB_SYNC=$("$TEAMX" sync --session inst:bob --json)
echo "$BOB_SYNC" | python3 -c "import json,sys; d=json.load(sys.stdin); assert any(t['team']['my_state']=='pending' for t in d['teams']), d"

step "approve Bob (owner)"
"$TEAMX" team approve "$BOB_MEMBER" --session inst:alice --json | python3 -c "import json,sys; d=json.load(sys.stdin); assert d['state']=='active', d"

step "Bob picks role contributor"
"$TEAMX" role set contributor --session inst:bob --json | python3 -c "import json,sys; d=json.load(sys.stdin); assert d['role']=='contributor', d"

step "owner sets goal"
"$TEAMX" goal set "Ship the MVP" --body "auth + api + ui" --session inst:alice --json | python3 -c "import json,sys; assert json.load(sys.stdin)['ok'], sys.stdin"

step "owner shares goal"
"$TEAMX" goal share --session inst:alice --json | python3 -c "import json,sys; d=json.load(sys.stdin); assert d['goal_state']=='shared' and d['team_state']=='active', d"

step "Bob publishes progress (goal -> in_progress)"
"$TEAMX" publish progress --data '{"message":"implemented auth"}' --session inst:bob --json | python3 -c "import json,sys; d=json.load(sys.stdin); assert d['goal_state']=='in_progress', d"

step "owner asks Bob a question (Bob -> waiting)"
ASK=$("$TEAMX" ask "$BOB_MEMBER" --question "Which db layer did you pick?" --session inst:alice --json)
echo "$ASK"
ASK_ID=$(echo "$ASK" | python3 -c "import json,sys; print(json.load(sys.stdin)['question_id'])")

step "Bob responds (Bob -> active)"
"$TEAMX" respond "$ASK_ID" --answer "SQLite WAL" --session inst:bob --json | python3 -c "import json,sys; assert json.load(sys.stdin)['ok'], sys.stdin"

step "Bob publishes achieved"
"$TEAMX" publish achieved --data '{"message":"all tasks done"}' --session inst:bob --json | python3 -c "import json,sys; d=json.load(sys.stdin); assert d['goal_state']=='achieved', d"

step "owner closes goal -> team completed"
"$TEAMX" goal close --session inst:alice --json | python3 -c "import json,sys; d=json.load(sys.stdin); assert d['goal_state']=='closed' and d['team_state']=='completed', d"

step "Alice status"
STATUS=$("$TEAMX" team status --team "$TEAM_ID" --json)
echo "$STATUS" | python3 -c "import json,sys; d=json.load(sys.stdin); t=d['teams'][0]; assert t['team']['state']=='completed', d"

step "Bob sync sees final state + events"
"$TEAMX" sync --session inst:bob --json | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert d['teams'][0]['team']['state']=='completed', d
types=[e['type'] for e in d['new_events']]
assert 'team.completed' in types, types
print('  new events:', types)
"

step "events ledger (seq ascending)"
"$TEAMX" events --team "$TEAM_ID" --json | python3 -c "
import json,sys
evs=json.load(sys.stdin)['events']
seqs=[e['seq'] for e in evs]
assert seqs==sorted(seqs), seqs
print('  total events:', len(seqs))
"

step "ALL PASS"
