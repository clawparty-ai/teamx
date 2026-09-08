#!/usr/bin/env bash
# local-team lifecycle test: TEAM.md -> up (persona + headless + ttyd) -> ps -> stop.
set -euo pipefail
TEAMX="${TEAMX:-$(cd "$(dirname "$0")/.." && pwd)/target/debug/teamx}"
WORK="$(mktemp -d /tmp/teamx-lt-XXXXXX)"
export TEAMX_HOME="$WORK/home"
pass(){ printf '  ok: %s\n' "$*"; }
fail(){ echo "FAIL: $*" >&2; exit 1; }
cleanup(){
  TEAMX_HOME="$WORK/home" "$TEAMX" local-team stop >/dev/null 2>&1 || true
  rm -rf "$WORK"
}
trap cleanup EXIT
mkdir -p "$WORK/proj/.teamx" "$WORK/home"

cat > "$WORK/proj/.teamx/TEAM.md" <<'EOF'
# 测试团队

## 背景
local-team 集成测试

## 目标
- 完成示例目标

## 成员

### searcher
- 姓名: Searcher
- 角色: searcher
- 分工: 检索整理
- 技能: 检索
- 输出: 清单

### writer
- 姓名: Writer
- 角色: writer
- 分工: 撰写
- 技能: 写作
- 输出: 报告

### boss
- 姓名: owner
- 角色: owner
- 分工: 统筹
EOF

OPENCODE="$(command -v opencode || true)"
[ -n "$OPENCODE" ] || OPENCODE="$HOME/.opencode/bin/opencode"
[ -x "$OPENCODE" ] || { echo "skipped: opencode not found"; exit 0; }

echo "== up (only searcher+writer, owner excluded) =="
OUT=$(cd "$WORK/proj" && TEAMX_HOME="$WORK/home" "$TEAMX" local-team up --opencode "$OPENCODE" --json)
echo "$OUT" | python3 -c "
import json,sys
d=json.load(sys.stdin)
ks=[m['key'] for m in d['members']]
assert d['ok'] is True
assert ks==['searcher','writer'], ks
assert all(m['port'] for m in d['members'])
for m in d['members']: print('  member', m['key'], 'port', m['port'], 'ttyd', m.get('ttyd'))
" && pass "up launched non-owner members" || fail "up members wrong"

echo "== persona written =="
[ -f "$WORK/proj/.teamx/local-team/searcher/AGENTS.md" ] && grep -q "Searcher" "$WORK/proj/.teamx/local-team/searcher/AGENTS.md" && pass "searcher persona" || fail "searcher persona"
[ -f "$WORK/proj/.teamx/local-team/writer/AGENTS.md" ] && pass "writer persona" || fail "writer persona"

echo "== ps lists them running with ttyd url =="
P=$(cd "$WORK/proj" && TEAMX_HOME="$WORK/home" "$TEAMX" local-team ps --json)
echo "$P" | python3 -c "
import json,sys
d=json.load(sys.stdin); ms=d['members']
assert len(ms)>=2, ms
assert any(m['name']=='searcher' and m['status']=='running' for m in ms), ms
assert any(m['name']=='searcher' and m.get('ttyd_url') for m in ms), ms
print('  ps members:', [m['name'] for m in ms])
" && pass "ps shows running members" || fail "ps"

echo "== stop one member =="
cd "$WORK/proj" && TEAMX_HOME="$WORK/home" "$TEAMX" local-team stop --name writer >/dev/null
P2=$(TEAMX_HOME="$WORK/home" "$TEAMX" local-team ps --json)
echo "$P2" | python3 -c "
import json,sys
ms=json.load(sys.stdin)['members']
w=[m for m in ms if m['name']=='writer'][0]
assert w['status']=='stopped', w
assert w.get('ttyd_running') is False, w
" && pass "writer stopped" || fail "stop writer"

echo "== stop all =="
TEAMX_HOME="$WORK/home" "$TEAMX" local-team stop >/dev/null
P3=$(TEAMX_HOME="$WORK/home" "$TEAMX" local-team ps --json)
echo "$P3" | python3 -c "
import json,sys
ms=json.load(sys.stdin)['members']
assert all(m['status']=='stopped' for m in ms), ms
" && pass "all stopped" || fail "stop all"

echo
echo "== result: PASSED =="
