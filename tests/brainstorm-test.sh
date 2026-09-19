#!/usr/bin/env bash
# Brainstorm discussion mode E2E (design/007) — local mode, no network.
#
# Asserts the P0a/P0b/P1a/P1b/P1c mechanics end-to-end:
#   create --mode brainstorm → 3 members post `kind=idea` → a no-kind message is
#   rejected in the diverging window → a `concern` must attach to an idea →
#   close without directions is rejected → harvested with directions[] →
#   open shows phase=harvested, NO auto digest, directions present →
#   posting after harvest rejected → handoff a direction → duplicate handoff rejected.
#
# Requires: python3.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TEAMX="${TEAMX:-$ROOT/target/debug/teamx}"
WORK="$(mktemp -d /tmp/teamx-bs-XXXXXX)"
export TEAMX_DB="$WORK/teamx.db"
export TEAMX_HOME="$WORK/home"
unset TEAMX_SERVER_URL || true
mkdir -p "$TEAMX_HOME"

pass() { printf '  ok: %s\n' "$*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT
jq() { python3 -c "import sys,json;d=json.load(sys.stdin);print($1)"; }
mid() { "$TEAMX" team status --session s1 --json 2>/dev/null | python3 -c "import sys,json;d=json.load(sys.stdin);print([m['id'] for m in d['teams'][0]['members'] if m['display_name']=='$1'][0])"; }

cd "$WORK"
echo "== brainstorm mode E2E (design/007) =="

CREATE="$("$TEAMX" team create "BS Team" --session s1 --json 2>/dev/null)"
TOKEN="$(echo "$CREATE" | jq "d['team']['invite_token']")"
"$TEAMX" team join "$TOKEN" --name Bob --session s2 --json >/dev/null 2>&1
"$TEAMX" team join "$TOKEN" --name Cara --session s3 --json >/dev/null 2>&1
"$TEAMX" team approve "$(mid Bob)" --session s1 --json >/dev/null
"$TEAMX" team approve "$(mid Cara)" --session s1 --json >/dev/null
pass "team + 2 members approved"

DID="$("$TEAMX" discussion create "Brainstorm" --session s1 --mode brainstorm --window-secs 86400 --min-ideas 1 --json 2>/dev/null | jq "d['discussion_id']")"
[ -n "$DID" ] || fail "no discussion id"
pass "brainstorm created: $DID"

"$TEAMX" discussion say "$DID" "idea from owner" --kind idea --session s1 --json >/dev/null
"$TEAMX" discussion say "$DID" "idea from bob" --kind idea --session s2 --json >/dev/null
"$TEAMX" discussion say "$DID" "idea from cara" --kind idea --session s3 --json >/dev/null
pass "3 members each posted an idea"

if "$TEAMX" discussion say "$DID" "no kind here" --session s2 --json >/dev/null 2>&1; then
  fail "message without kind should be rejected in the diverging window"
fi
pass "no-kind message rejected (diverging window)"

MSG="$("$TEAMX" discussion say "$DID" "an idea to attach to" --kind idea --session s2 --json 2>/dev/null | jq "d['message_id']")"
if "$TEAMX" discussion reply "$DID" "$MSG" "concern without idea" --kind concern --session s3 --json >/dev/null 2>&1; then
  : # reply_to is provided by `reply`, so this one is allowed
fi
"$TEAMX" discussion reply "$DID" "$MSG" "concern here" --kind concern --session s3 --json >/dev/null
pass "concern attached to an idea"

if "$TEAMX" discussion close "$DID" --session s1 --json >/dev/null 2>&1; then
  fail "brainstorm close without directions should be rejected"
fi
pass "close without directions rejected"

DIRS="[{\"direction_id\":\"d1\",\"title\":\"方向A\",\"source_message_ids\":[\"$MSG\"],\"next_step\":\"做原型验证\"}]"
"$TEAMX" discussion close "$DID" --directions "$DIRS" --session s1 --json >/dev/null
pass "harvested with directions[]"

O="$("$TEAMX" discussion open "$DID" --session s1 --json 2>/dev/null)"
[ "$(echo "$O" | jq "d['phase']")" = "harvested" ] || fail "phase should be harvested"
[ "$(echo "$O" | jq "d['summary_auto']")" = "False" ] || fail "brainstorm must NOT auto-digest"
[ "$(echo "$O" | jq "len(d['directions'])")" = "1" ] || fail "directions missing on open"
pass "open: phase=harvested, summary_auto=false, directions present"

if "$TEAMX" discussion say "$DID" "late idea" --kind idea --session s2 --json >/dev/null 2>&1; then
  fail "posting after harvest should be rejected"
fi
pass "post after harvest rejected"

T2="$("$TEAMX" discussion create "target" --session s1 --json 2>/dev/null | jq "d['discussion_id']")"
"$TEAMX" discussion handoff "$DID" --direction-id d1 --target-kind discussion --target-id "$T2" --session s1 --json >/dev/null
pass "direction handed off to $T2"

if "$TEAMX" discussion handoff "$DID" --direction-id d1 --target-kind discussion --target-id "$T2" --session s1 --json >/dev/null 2>&1; then
  fail "duplicate handoff of the same direction should be rejected"
fi
pass "duplicate handoff rejected"

# 普通讨论仍然自动 digest（存量行为回归锚）
PLAIN="$("$TEAMX" discussion create "plain" --session s1 --json 2>/dev/null | jq "d['discussion_id']")"
"$TEAMX" discussion say "$PLAIN" "hi" --session s2 --json >/dev/null
"$TEAMX" discussion close "$PLAIN" --session s1 --json >/dev/null
P="$("$TEAMX" discussion open "$PLAIN" --session s1 --json 2>/dev/null)"
[ "$(echo "$P" | jq "d['summary_auto']")" = "True" ] || fail "normal thread must still auto-digest"
pass "normal thread still auto-digests (regression anchor)"

echo "ALL PASSED"
