#!/usr/bin/env bash
# teamx V1 concurrency test (TC-301):
# several sessions publish in parallel; every team's event seq must stay
# strictly ascending and unique (single-writer + busy retry + in-tx seq).
set -euo pipefail

TEAMX="${TEAMX:-$(dirname "$0")/../target/debug/teamx}"
DB="${1:-$(mktemp /tmp/teamx-conc-XXXXXX).db}"
export TEAMX_DB="$DB"
trap 'rm -f "$DB" "$DB-wal" "$DB-shm"' EXIT

jget() { python3 -c "import json,sys; print(json.load(sys.stdin)$1)"; }

"$TEAMX" init >/dev/null
CREATE=$($TEAMX team create "Conc" --session inst:a --json)
TEAM_ID=$(echo "$CREATE" | jget "['team']['id']")
TOKEN=$(echo "$CREATE" | jget "['team']['invite_token']")
"$TEAMX" goal set "Goal" --session inst:a >/dev/null
"$TEAMX" goal share --session inst:a >/dev/null

N=5
for i in $(seq 1 "$N"); do
  "$TEAMX" team join "$TOKEN" --name "M$i" --session "inst:m$i" >/dev/null
done
STATUS=$("$TEAMX" team status --team "$TEAM_ID" --json)
for MID in $(echo "$STATUS" | python3 -c "
import json,sys
d=json.load(sys.stdin)
print(' '.join(m['id'] for m in d['teams'][0]['members'] if m['display_name'].startswith('M')))"); do
  "$TEAMX" team approve "$MID" --session inst:a >/dev/null
done

# parallel publishes: N sessions x 3 rounds
PIDS=()
for i in $(seq 1 "$N"); do
  for r in 1 2 3; do
    "$TEAMX" publish progress --data "{\"n\":$i$r}" --session "inst:m$i" >/dev/null &
    PIDS+=($!)
  done
done
for p in "${PIDS[@]}"; do wait "$p"; done

"$TEAMX" events --team "$TEAM_ID" --json | python3 -c "
import json,sys
evs=[e for e in json.load(sys.stdin)['events'] if e['type']=='progress.published']
seqs=[e['seq'] for e in evs]
assert len(evs)==$((N*3)), f'expected $((N*3)) progress events, got {len(evs)}'
assert seqs==sorted(seqs) and len(seqs)==len(set(seqs)), f'seq not strictly ascending: {seqs}'
print(f'  concurrent progress events: {len(evs)}, seq strictly ascending: True')
"
echo "CONCURRENCY TEST PASS"
