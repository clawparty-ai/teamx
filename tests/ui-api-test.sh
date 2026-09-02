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

echo "== params =="
code=$($CURL -b /tmp/teamx-ui-api-cookie -o /dev/null -w '%{http_code}' "$BASE/api/map?team=does-not-exist"); check "map unknown team -> 200 empty" 200 "$code"
$CURL -b /tmp/teamx-ui-api-cookie "$BASE/api/map?team=does-not-exist" | python3 -c "import json,sys;d=json.load(sys.stdin);assert d['nodes']==[] or len([n for n in d['nodes'] if n['kind']=='member'])==0" && ok "map unknown team: no members" || bad "map unknown team: no members"
code=$($CURL -b /tmp/teamx-ui-api-cookie -o /dev/null -w '%{http_code}' "$BASE/api/overview?from=bogus&to=also"); check "overview invalid from/to -> 200" 200 "$code"
code=$($CURL -o /dev/null -w '%{http_code}' "$BASE/api/nope"); check "unknown path -> 404" 404 "$code"

echo "== locations config =="
printf '{"server":{"city":"Hong Kong","lat":22.32,"lng":114.17},"members":{"bob":{"city":"Shenzhen","lat":22.54,"lng":114.06}}}' > "$WORK/.teamx/ui-locations.json"
$CURL -b /tmp/teamx-ui-api-cookie "$BASE/api/map" | python3 -c "import json,sys;d=json.load(sys.stdin);b=[n for n in d['nodes'] if n.get('member_name')=='bob'];assert b and b[0]['city']=='Shenzhen' and b[0]['lat']==22.54;(b)" && ok "locations applied to bob" || bad "locations applied to bob"

echo
echo "== result: PASS=$PASS FAIL=$FAIL =="
[ "$FAIL" = 0 ]
