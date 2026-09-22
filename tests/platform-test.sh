#!/usr/bin/env bash
# 平台面 E2E（design/014 §6/§7）：一次性 bootstrap token + 平台管理员 + 独立审计。
#
# 断言：
#   P1 生成 token（明文只返回一次；库里只存 SHA-256 哈希）
#   P2 兑现 → 绑定平台管理员（users.id 为「人」的权威键）
#   P3 **一次性**：同一 token 复用被拒，且不授予管理员
#   P4 吊销未兑现 token → 兑现被拒
#   P5 过期 token 被拒
#   P6 审计独立留痕（issued / redeemed / rejected）
#   P7 最后一个平台管理员不可撤
#
# Requires: python3 + sqlite3。
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TEAMX="${TEAMX:-$ROOT/target/debug/teamx}"
WORK="$(mktemp -d /tmp/teamx-platform-XXXXXX)"
export TEAMX_DB="$WORK/teamx.db"
export TEAMX_HOME="$WORK/home"
unset TEAMX_SERVER_URL TEAMX_PROVISION_FAKE || true
mkdir -p "$TEAMX_HOME"

pass() { printf '  ok: %s\n' "$*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT
jq() { python3 -c "import sys,json;d=json.load(sys.stdin);print($1)"; }
cd "$WORK"

echo "== platform plane E2E (design/014 §6/§7) =="
"$TEAMX" init >/dev/null

# P1: 生成 token
OUT="$("$TEAMX" --json platform bootstrap-token --ttl 3600)"
TOKEN="$(echo "$OUT" | jq "d['token']")"
TID="$(echo "$OUT" | jq "d['token_id']")"
[ -n "$TOKEN" ] && [ -n "$TID" ] || fail "P1: 未返回 token"
# 库里不得有明文，只有 64 hex 哈希
STORED="$(sqlite3 "$TEAMX_DB" "SELECT token_hash FROM platform_tokens WHERE id='$TID';")"
[ "$STORED" != "$TOKEN" ] || fail "P1: token 明文入库"
[ ${#STORED} -eq 64 ] || fail "P1: 哈希长度异常（${#STORED}）"
pass "P1 token 生成且只存哈希（${STORED:0:12}…）"

# P2: 兑现 → 管理员
"$TEAMX" --json platform redeem --token "$TOKEN" --user-id "gh:alice" --user-name alice >/dev/null
ADMINS="$("$TEAMX" --json platform admins | jq "[a['user_name'] for a in d['admins']]")"
echo "$ADMINS" | grep -q "alice" || fail "P2: 兑现后未成为管理员"
pass "P2 兑现 → 平台管理员 alice"

# P3: 一次性
if "$TEAMX" --json platform redeem --token "$TOKEN" --user-id "gh:bob" --user-name bob >/dev/null 2>&1; then
  fail "P3: 同一 token 复用未被拒"
fi
if "$TEAMX" --json platform admins | grep -q "bob"; then fail "P3: 复用不得授予管理员"; fi
pass "P3 一次性（复用被拒且不授权）"

# P4: 吊销未兑现 token
R="$("$TEAMX" --json platform bootstrap-token)"
RT="$(echo "$R" | jq "d['token']")"
RID="$(echo "$R" | jq "d['token_id']")"
"$TEAMX" --json platform revoke "$RID" >/dev/null
if "$TEAMX" --json platform redeem --token "$RT" --user-id "gh:carol" --user-name carol >/dev/null 2>&1; then
  fail "P4: 已吊销 token 仍可兑现"
fi
pass "P4 吊销生效"

# P5: 过期
E="$("$TEAMX" --json platform bootstrap-token --ttl 60)"
ET="$(echo "$E" | jq "d['token']")"
EID="$(echo "$E" | jq "d['token_id']")"
sqlite3 "$TEAMX_DB" "UPDATE platform_tokens SET expires_at='2000-01-01T00:00:00+00:00' WHERE id='$EID';"
if "$TEAMX" --json platform redeem --token "$ET" --user-id "gh:dave" --user-name dave >/dev/null 2>&1; then
  fail "P5: 过期 token 仍可兑现"
fi
pass "P5 过期拒绝"

# P6: 审计独立留痕
ACTIONS="$("$TEAMX" --json platform audit --limit 100 | jq "[e['action'] for e in d['audit']]")"
for a in bootstrap_token.issued bootstrap_token.redeemed bootstrap_token.rejected bootstrap_token.revoked; do
  echo "$ACTIONS" | grep -q "$a" || fail "P6: 审计缺 $a"
done
pass "P6 审计独立留痕（issued/redeemed/rejected/revoked）"

# P7: 最后一个管理员不可撤
if "$TEAMX" --json platform admin-revoke --user-id "gh:alice" >/dev/null 2>&1; then
  fail "P7: 最后一个管理员被撤掉了"
fi
pass "P7 最后一个管理员不可撤"

printf '\nALL PASSED (platform plane E2E)\n'
