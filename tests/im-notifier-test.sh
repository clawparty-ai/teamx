#!/usr/bin/env bash
# IM outbound notifier acceptance test (docs/57, network mode).
#
# Seeds whitelisted / non-whitelisted / heartbeat ledger events directly into
# the team DB, then boots a real `teamx serve` with the notifier enabled
# (dry-run feishu provider) and asserts:
#   T-1 collect: only whitelisted non-heartbeat events enter im_outbox
#       (heartbeat progress.published[kind=session.idle] and team.created
#       are skipped — closed enum).
#   T-2 direct:  doc.help_requested is pushed immediately (逐条必达),
#       audited as im.push (digest=false).
#   T-3 digest:  informational events aggregate into ONE im.push
#       (digest=true) after the (shrunk) 60 s window.
#   T-4 no loop: im.push audit events are never re-collected.
#
# Requires: sqlite3. Offline-safe: the provider runs with TEAMX_IM_DRY_RUN=1
# (never touches the network).
set -euo pipefail

TEAMX="${TEAMX:-$(cd "$(dirname "$0")/.." && pwd)/target/debug/teamx}"
DB="$(mktemp /tmp/teamx-im-notifier-XXXXXX).db"
HOME_DIR="$(mktemp -d /tmp/teamx-im-notifier-home-XXXXXX)"
PORT="${TEAMX_IM_TEST_PORT:-5795}"
export TEAMX_DB="$DB"
export TEAMX_HOME="$HOME_DIR"

pass(){ printf '  ok: %s\n' "$*"; }
fail(){ echo "FAIL: $*" >&2; exit 1; }
cleanup(){ [ -n "${SERVE_PID:-}" ] && kill "$SERVE_PID" 2>/dev/null || true; rm -f "$DB" "$DB-wal" "$DB-shm"; rm -rf "$HOME_DIR"; }
trap cleanup EXIT
step(){ printf '\n=== %s ===\n' "$*"; }
# sqlite reads must tolerate the server's WAL write locks.
dbq(){ sqlite3 -cmd ".timeout 5000" "$DB" "$1"; }

command -v sqlite3 >/dev/null 2>&1 || { echo "skipped: sqlite3 not available"; exit 0; }
[ -x "$TEAMX" ] || fail "teamx binary missing (cargo build -p teamx first)"

step "setup: team + seeded ledger events"
"$TEAMX" init >/dev/null
"$TEAMX" team create "ImNotifierTeam" --session s:owner --json >/dev/null
TEAM_ID=$("$TEAMX" team status --session s:owner --json | python3 -c "import json,sys; print(json.load(sys.stdin)['teams'][0]['team']['id'])")
[ -n "$TEAM_ID" ] || fail "team id"

# events.id must be strictly increasing (collection cursor); seq per team —
# continue after whatever `team create` already emitted.
BASE=$(dbq "SELECT COALESCE(MAX(seq),0) FROM events WHERE team_id='$TEAM_ID';")
NOW=$(date -u +%Y-%m-%dT%H:%M:%S+00:00)
dbq "
INSERT INTO events (team_id, member_id, seq, type, payload_json, created_at) VALUES
 ('$TEAM_ID','m1',$((BASE+1)),'progress.published','{\"kind\":\"session.idle\"}','$NOW'),
 ('$TEAM_ID','m1',$((BASE+2)),'progress.published','{\"message\":\"real progress\"}','$NOW'),
 ('$TEAM_ID','m1',$((BASE+3)),'team.created',NULL,'$NOW'),
 ('$TEAM_ID','m1',$((BASE+4)),'doc.help_requested','{\"title\":\"救火\"}','$NOW'),
 ('$TEAM_ID','m1',$((BASE+5)),'decision.broadcast','{\"message\":\"广播\"}','$NOW'),
 ('$TEAM_ID','m1',$((BASE+6)),'budget.alert','{\"team_id\":\"$TEAM_ID\",\"member_id\":\"m1\",\"period\":\"cumulative\",\"usage_cost_usd\":12.5,\"threshold_usd\":10.0}','$NOW');"
pass "seeded 6 ledger events (1 heartbeat, 1 non-whitelisted, 2 digest, 2 direct)"

step "start serve with notifier (dry-run, interval 1s, digest window 3s)"
TEAMX_IM_NOTIFY_GROUPS="oc_test" \
TEAMX_IM_DRY_RUN=1 \
TEAMX_IM_NOTIFY_INTERVAL_SECS=1 \
TEAMX_IM_DIGEST_SECS=3 \
  "$TEAMX" serve --addr 127.0.0.1 --port "$PORT" >/dev/null 2>&1 &
SERVE_PID=$!

wait_rows(){ # state expected description timeout_secs
  local want="$2" desc="$3" N=""
  for _ in $(seq 1 "$4"); do
    N=$(dbq "SELECT COUNT(*) FROM im_outbox WHERE state='$1';")
    [ "$N" = "$want" ] && return 0
    sleep 1
  done
  fail "$desc (expected $want rows state=$1, got ${N:-0})"
}

step "T-1/T-2 collect + immediate direct push"
wait_rows pushed 2 "direct rows pushed (逐条必达)" 15
N=$(dbq "SELECT COUNT(*) FROM im_outbox;")
[ "$N" = "4" ] || fail "T-1 closed enum: expected 4 outbox rows, got $N (heartbeat/team.created must be skipped)"
P=$(dbq "SELECT COUNT(*) FROM events WHERE type='im.push';")
[ "$P" -ge 1 ] || fail "T-2 im.push audit missing"
PUSHED=$(dbq "SELECT type FROM events WHERE type='im.push' AND payload_json LIKE '%\"digest\":false%' LIMIT 1;")
[ "$PUSHED" = "im.push" ] || fail "T-2 direct im.push (digest=false) missing"
pass "heartbeat + non-whitelisted skipped; 2 direct rows pushed (help_requested + budget.alert)"

step "T-3 digest aggregation after the (shrunk) window"
wait_rows pushed 4 "digest rows aggregated and pushed" 20
D=$(dbq "SELECT COUNT(*) FROM events WHERE type='im.push' AND payload_json LIKE '%\"digest\":true%';")
# one digest message per (team, event_type) group: progress.published + decision.broadcast
[ "$D" = "2" ] || fail "T-3 expected 2 digest im.push (one per event_type group), got $D"
pass "digest rows aggregated per (team, event_type) group after the window"
B=$(dbq "SELECT COUNT(*) FROM im_outbox WHERE event_type='budget.alert';")
[ "$B" -ge 1 ] || fail "budget.alert outbox row missing"

step "T-4 no loop: audit events are not re-collected"
sleep 4
N=$(dbq "SELECT COUNT(*) FROM im_outbox;")
[ "$N" = "4" ] || fail "T-4 outbox grew to $N — im.push events must never be re-collected"
F=$(dbq "SELECT COUNT(*) FROM events WHERE type='im.push_failed';")
[ "$F" = "0" ] || fail "unexpected im.push_failed in dry-run"
pass "outbox stable at 4 rows, zero failures"

echo
echo "IM NOTIFIER TEST PASSED"
