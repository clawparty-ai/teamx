#!/usr/bin/env bash
# local-team server bootstrap test: no server -> up self-boots serve, creates
# team from TEAM.md, activates (import+approve) non-owner members, spawns
# headless+ttyd; stop tears down serve.
set -euo pipefail
TEAMX="${TEAMX:-$(cd "$(dirname "$0")/.." && pwd)/target/debug/teamx}"
WORK="$(mktemp -d /tmp/teamx-lts-XXXXXX)"
pass(){ printf '  ok: %s\n' "$*"; }
fail(){ echo "FAIL: $*" >&2; exit 1; }
cleanup(){
  TEAMX_HOME="$WORK/.teamx" "$TEAMX" local-team stop >/dev/null 2>&1 || true
  pkill -f "opencode serve --hostname 127.0.0.1 --port 4[23]" 2>/dev/null || true
  sleep 0.3
  rm -rf "$WORK"
}
trap cleanup EXIT
mkdir -p "$WORK/.teamx"

cat > "$WORK/.teamx/TEAM.md" <<'EOF'
# 本机团队

## 背景
local-team server 测试

## 目标
- 完成示例

## 成员

### searcher
- 姓名: Searcher
- 角色: searcher
- 分工: 检索整理
- 技能: 检索
- 输出: 清单

### boss
- 姓名: owner
- 角色: owner
- 分工: 统筹
EOF

OPENCODE="$(command -v opencode || true)"; [ -n "$OPENCODE" ] || OPENCODE="$HOME/.opencode/bin/opencode"
[ -x "$OPENCODE" ] || { echo "skipped: opencode not found"; exit 0; }

echo "== up (self-boot server + team + activate searcher) =="
OUT=$(cd "$WORK" && TEAMX_HOME="$WORK/.teamx" "$TEAMX" local-team up --team-file "$WORK/.teamx/TEAM.md" --opencode "$OPENCODE" --json 2>up.err)
echo "$OUT" | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert d['ok'] is True, d
ms=[m for m in d['members'] if m['name']=='searcher']
assert len(ms)==1, d
assert ms[0]['status']=='running' and ms[0]['ttyd_url'], ms
print('  member searcher running', ms[0]['port'], ms[0]['ttyd_url'])
" && pass "up bootstrapped team+member" || { cat "$WORK/up.err"; fail "up"; }

echo "== server pid written =="
[ -f "$WORK/.teamx/local-team/server.pid" ] && pass "server.pid exists" || fail "server.pid"

echo "== member is active on the team =="
ST=$(cd "$WORK" && TEAMX_HOME="$WORK/.teamx" "$TEAMX" team status --json --session local-owner)
echo "$ST" | python3 -c "
import json,sys
d=json.load(sys.stdin); t=d['teams'][0]
ms={m['display_name']:(m['role'],m['state']) for m in t['members']}
assert ms.get('Searcher'), ms
assert ms['Searcher'][1]=='active', ms
print('  members:', ms)
" && pass "searcher active" || fail "active"

echo "== member workspace has letter =="
[ -f "$WORK/.teamx/local-team/searcher/.teamx/letters/current.json" ] && pass "letter stored" || fail "letter"

echo "== ttyd reachable =="
curl -s -o /dev/null -w "%{http_code}" --max-time 4 "http://127.0.0.1:7680/" 2>/dev/null | grep -q 200 && pass "ttyd 200" || fail "ttyd"

echo "== stop tears down serve =="
SPID=$(cat "$WORK/.teamx/local-team/server.pid")
cd "$WORK" && TEAMX_HOME="$WORK/.teamx" "$TEAMX" local-team stop >/dev/null
sleep 0.5
kill -0 "$SPID" 2>/dev/null && fail "serve still alive" || pass "serve stopped"

echo
echo "== result: PASSED =="
