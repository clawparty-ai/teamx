#!/usr/bin/env bash
# UI API test — boots a real `teamx ui` against a seeded temp DB and asserts
# the dashboard API contract (auth, params, structure of every endpoint incl.
# the mockup-aligned /api/map + /api/topology).
#
# Usage: TEAMX_BIN=target/release/teamx bash tests/ui-api-test.sh
# Requires: sqlite3 (to seed presence/activity rows) + curl.
set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${TEAMX_BIN:-$ROOT/target/debug/teamx}"
PORT="${TEAMX_UI_TEST_PORT:-19541}"
BASE="https://127.0.0.1:$PORT"

WORK="$(mktemp -d /tmp/teamx-ui-api-XXXXXX)"
LOG="$WORK/ui.log"
PASS=0; FAIL=0

cleanup() { [ -n "${UI_PID:-}" ] && kill "$UI_PID" 2>/dev/null; rm -rf "$WORK" /tmp/teamx-ui-api-cookie; }
trap cleanup EXIT

ok()   { PASS=$((PASS+1)); echo "  ✓ $1"; }
bad()  { FAIL=$((FAIL+1)); echo "  ✗ $1"; }
check() { # name expected_code actual
  if [ "$2" = "$3" ]; then ok "$1 (HTTP $3)"; else bad "$1 (expected $2 got $3)"; fi
}

echo "== boot ui =="
cd "$WORK"
"$BIN" team create UIAPITeam --session owner >/dev/null 2>&1 || { echo "team create failed"; exit 1; }
# second member (owner can't join; insert directly so map has 2 members)
sqlite3 "$WORK/.teamx/teamx.db" \
  "INSERT OR IGNORE INTO members (id, team_id, session_key, display_name, role, state, joined_at)
   VALUES ('m-extra','$(sqlite3 "$WORK/.teamx/teamx.db" "SELECT id FROM teams LIMIT 1")','s2','bob','contributor','active',datetime('now'));" \
  || bad "seed second member"
# fresh presence -> busy/online
sqlite3 "$WORK/.teamx/teamx.db" \
  "INSERT INTO presence (member_id, active_sessions, running_tools, open_terminals, updated_at)
   SELECT id, 1, 2, 1, strftime('%Y-%m-%dT%H:%M:%f+00:00','now') FROM members WHERE role='contributor';" \
  || bad "seed presence"

# seed activity so /api/kanban has data to aggregate/sort:
#   owner  -> one recent work_session + one fine-grained tool_call (task + detail)
#   bob    -> NO activity (must sink to the bottom)
TEAM_ID="$(sqlite3 "$WORK/.teamx/teamx.db" "SELECT id FROM teams LIMIT 1")"
OWNER_ID="$(sqlite3 "$WORK/.teamx/teamx.db" "SELECT id FROM members WHERE role='owner' LIMIT 1")"
NOW="$(date -u +%Y-%m-%dT%H:%M:%S+00:00)"
sqlite3 "$WORK/.teamx/teamx.db" "INSERT INTO activity
  (team_id, member_id, node_id, node_name, started_at, ended_at, duration_ms, kind, detail, tokens_input, tokens_output, cost, has_human, created_at)
  VALUES
  ('$TEAM_ID','$OWNER_ID','n1','node1', datetime('now','-5 minutes'), datetime('now','-4 minutes'), 60000, 'work_session',
   '{\"task\":\"round2 检索\",\"sessionID\":\"sess-1\"}', 1000, 500, 0.001, 0, datetime('now','-5 minutes')),
  ('$TEAM_ID','$OWNER_ID','n1','node1', datetime('now','-4 minutes'), NULL, 250, 'tool_call',
   '{\"tool\":\"bash\",\"state\":\"completed\",\"arguments\":{\"command\":\"echo hi\"}}', 0, 0, 0.0, 0, datetime('now','-4 minutes'));" \
  || bad "seed activity"

"$BIN" ui --addr 127.0.0.1 --port "$PORT" --session owner --db "$WORK/.teamx/teamx.db" >"$LOG" 2>&1 &
UI_PID=$!
for _ in $(seq 1 30); do
  grep -q "open:" "$LOG" 2>/dev/null && break
  sleep 0.3
done
TOKEN="$(sed -n 's/.*token=\([0-9a-f]*\).*/\1/p' "$LOG" | head -1)"
[ -n "$TOKEN" ] || { echo "ui token not found:"; cat "$LOG"; exit 1; }
CURL="curl -sk --max-time 6"
# login (set cookie)
$CURL -c /tmp/teamx-ui-api-cookie "$BASE/?token=$TOKEN" -o /dev/null

echo "== auth =="
code=$($CURL -o /dev/null -w '%{http_code}' "$BASE/api/teams");                 check "no cookie -> 401" 401 "$code"
code=$($CURL -b /tmp/teamx-ui-api-cookie -o /dev/null -w '%{http_code}' "$BASE/api/teams"); check "cookie teams -> 200" 200 "$code"

echo "== core endpoints =="
for ep in teams goal overview by_member by_node tools files rows human members kanban timeline map topology; do
  code=$($CURL -b /tmp/teamx-ui-api-cookie -o /dev/null -w '%{http_code}' "$BASE/api/$ep"); check "/api/$ep -> 200" 200 "$code"
done

echo "== map / topology structure =="
MAP="$($CURL -b /tmp/teamx-ui-api-cookie "$BASE/api/map")"
TOP="$($CURL -b /tmp/teamx-ui-api-cookie "$BASE/api/topology")"
echo "$MAP" | python3 -c "import json,sys;d=json.load(sys.stdin);ns=d['nodes'];assert ns and ns[0]['kind']=='server',ns;assert any(n['kind']=='member' for n in ns);print('map nodes:',len(ns))" && ok "map: server + members" || bad "map: server + members"
echo "$TOP" | python3 -c "import json,sys;d=json.load(sys.stdin);assert d['server']['kind']=='server';assert len(d['nodes'])>=1;assert all(l['type']=='mTLS' for l in d['links']);print('topo links:',len(d['links']))" && ok "topology: server/nodes/mTLS links" || bad "topology"

echo "== kanban: sort + detail events (K-01..03) =="
KB="$($CURL -b /tmp/teamx-ui-api-cookie "$BASE/api/kanban")"
echo "$KB" | python3 -c "
import json,sys
d=json.load(sys.stdin)
cols=d['columns']
# Both members should be present; owner has activity, bob has none.
names=[c['display_name'] for c in cols]
assert 'UIAPITeam' in names and 'bob' in names, ('members', names)
# K-01: active column first, no-activity column last.
assert cols[0]['display_name']=='UIAPITeam', ('first col', cols[0]['display_name'])
assert cols[-1]['display_name']=='bob', ('last col', cols[-1]['display_name'])
# K-01: active column carries a task (work_session).
owner=[c for c in cols if c['display_name']=='UIAPITeam'][0]
assert len(owner['tasks'])>=1, ('tasks', owner['tasks'])
# K-03: fine-grained tool_call lands in events, not tasks.
assert any(e['kind']=='tool_call' for e in owner['events']), ('events', owner['events'])
assert all(t['kind']=='work_session' for t in owner['tasks']), ('tasks kinds', owner['tasks'])
# K-02: no-activity column has empty tasks & events.
bob=[c for c in cols if c['display_name']=='bob'][0]
assert bob['tasks']==[] and bob['events']==[], ('bob empty', bob['tasks'], bob['events'])
print('kanban columns:', len(cols), '| owner tasks:', len(owner['tasks']), 'events:', len(owner['events']))
" && ok "kanban: active-first / empty-sunk / events carry tool_call" || bad "kanban sort/aggregation"

echo "== params =="
code=$($CURL -b /tmp/teamx-ui-api-cookie -o /dev/null -w '%{http_code}' "$BASE/api/map?team=does-not-exist"); check "map unknown team -> 403 owner-scoped" 403 "$code"
code=$($CURL -b /tmp/teamx-ui-api-cookie -o /dev/null -w '%{http_code}' "$BASE/api/overview?from=bogus&to=also"); check "overview invalid from/to -> 200" 200 "$code"
code=$($CURL -o /dev/null -w '%{http_code}' "$BASE/api/nope"); check "unknown path -> 404" 404 "$code"

echo "== locations config =="
printf '{"server":{"city":"Hong Kong","lat":22.32,"lng":114.17},"members":{"bob":{"city":"Shenzhen","lat":22.54,"lng":114.06}}}' > "$WORK/.teamx/ui-locations.json"
$CURL -b /tmp/teamx-ui-api-cookie "$BASE/api/map" | python3 -c "import json,sys;d=json.load(sys.stdin);b=[n for n in d['nodes'] if n.get('member_name')=='bob'];assert b and b[0]['city']=='Shenzhen' and b[0]['lat']==22.54;(b)" && ok "locations applied to bob" || bad "locations applied to bob"

echo
echo "== result: PASS=$PASS FAIL=$FAIL =="
[ "$FAIL" = 0 ]
