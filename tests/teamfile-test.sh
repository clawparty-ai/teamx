#!/usr/bin/env bash
# teamx TEAM.md bootstrap test (TF-101..TF-108).
# Verifies `team create` detects .teamx/TEAM.md and auto-initializes:
#   - sets the team goal from TEAM.md
#   - issues per-member invitation letters (saved to .teamx/members/<name>/ + printed)
#   - generates member AGENTS.md (merged project-root AGENTS.md + member profile)
#   - creates .teamx/members/<name>/ work directories
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TEAMX="${TEAMX:-$ROOT/target/debug/teamx}"
WORK="$(mktemp -d /tmp/teamx-teamfile-XXXXXX)"
export TEAMX_HOME="$(mktemp -d /tmp/teamx-teamfile-home-XXXXXX)"
export TEAMX_DB="$(mktemp /tmp/teamx-teamfile-XXXXXX).db"

pass() { printf '  ok: %s\n' "$*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }
cleanup() { rm -rf "$WORK" "$TEAMX_HOME" "$TEAMX_DB" "$TEAMX_DB-wal" "$TEAMX_DB-shm"; }
trap cleanup EXIT

step() { printf '\n=== %s ===\n' "$*"; }

step "TF-101: no TEAM.md -> original behavior"
( cd "$WORK" && "$TEAMX" team create "Plain" --session s:owner --json ) >/dev/null
[ ! -d "$WORK/.teamx/members" ] || fail "no TEAM.md should not create members dir"
pass "no TEAM.md, no members dir"

step "TF-102: TEAM.md -> auto-initialize (goal + letters + AGENTS + workdirs)"
mkdir -p "$WORK/.teamx"
cat > "$WORK/.teamx/TEAM.md" << 'EOF'
# 企业数字化平台

## 背景
团队协作平台：任务分派、reverse tunnel、活动分析。

## 目标
- 8 月底交付 v1.0
- 支持跨网络 reverse tunnel

## 成员
### owner
- 姓名: 企业数字化平台
- 角色: owner
- 分工: 架构设计、代码审查
- 技能: Rust, TypeScript
- 输出: 架构文档

### 小明
- 姓名: 小明
- 角色: contributor
- 分工: 前端开发、测试
- 技能: React, TypeScript
- 输出: 看板组件、测试用例

### 小红
- 姓名: 小红
- 角色: reviewer
- 分工: 代码审查
- 技能: Rust, 代码评审
- 输出: 审查报告
EOF
OUT=$( cd "$WORK" && "$TEAMX" team create "企业数字化平台" --session s:lead --json )
echo "$OUT" | python3 -c "import json,sys; d=json.load(sys.stdin); assert d.get('ok') is True, d" || fail "create failed"
echo "$OUT" | python3 -c "
import json,sys
d=json.load(sys.stdin)
tf=d.get('teamfile')
assert tf, 'teamfile info missing'
assert tf.get('team_name')=='企业数字化平台', tf
assert d.get('goal_id'), 'goal_id missing (auto goal set)'
assert len(tf['members'])==3, tf['members']
for m in tf['members']:
    assert m['workdir'], m
print('members:', [(m['key'], m['role']) for m in tf['members']])
" || fail "bootstrap output invalid"
pass "team + goal + 3 members bootstrapped"

step "TF-103: member AGENTS.md generated (with role/duties/skills/outputs)"
[ -f "$WORK/.teamx/members/小明/AGENTS.md" ] || fail "小明 AGENTS.md missing"
[ -f "$WORK/.teamx/members/小红/AGENTS.md" ] || fail "小红 AGENTS.md missing"
grep -q "前端开发、测试" "$WORK/.teamx/members/小明/AGENTS.md" || fail "小明 duties missing in AGENTS.md"
grep -q "React, TypeScript" "$WORK/.teamx/members/小明/AGENTS.md" || fail "小明 skills missing"
grep -q "代码评审" "$WORK/.teamx/members/小红/AGENTS.md" || fail "小红 skills missing"
pass "member AGENTS.md files exist with profile content"

step "TF-104: letter dual output (file + printed)"
[ -f "$WORK/.teamx/members/小明/invitation.letter" ] || fail "小明 letter file missing"
[ -f "$WORK/.teamx/members/小红/invitation.letter" ] || fail "小红 letter file missing"
grep -q "teamx-inv:v1:" "$WORK/.teamx/members/小明/invitation.letter" || fail "小明 letter file not a valid letter"
echo "$OUT" | python3 -c "
import json,sys
d=json.load(sys.stdin)
for m in d['teamfile']['members']:
    if m['key']=='小明':
        assert m.get('letter') and m['letter'].get('ok') is True, m
        assert m['letter_file'].endswith('invitation.letter'), m
" || fail "printed letter info missing"
pass "letters saved and printed"

step "TF-105: letter importable -> seat pending"
LETTER=$( cat "$WORK/.teamx/members/小明/invitation.letter" )
IMP=$( cd "$WORK" && "$TEAMX" team import "$LETTER" --name 小明 --session s:xiaoming --json )
echo "$IMP" | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert d.get('ok') is True, d
assert d['status']=='pending', d
assert d['role']=='contributor', d
" || fail "import failed"
pass "letter imports; 小明 seat pending"

step "TF-106: project-root AGENTS.md merged into member AGENTS.md"
cat > "$WORK/AGENTS.md" << 'EOF'
# Project AGENTS
This is the project-level agent instructions. Build with cargo, run tests.
EOF
mkdir -p "$WORK/.teamx"
cat > "$WORK/.teamx/TEAM.md" << 'EOF'
# 第二个项目

## 成员
### 小王
- 姓名: 小王
- 角色: contributor
- 分工: 后端开发
EOF
OUT2=$( cd "$WORK" && "$TEAMX" team create "第二个项目" --session s:owner2 --json )
grep -q "Project AGENTS" "$WORK/.teamx/members/小王/AGENTS.md" || fail "project AGENTS.md not merged"
pass "project AGENTS.md merged into member AGENTS.md"

step "TF-107: invalid TEAM.md degrades gracefully (no block)"
: > "$WORK/.teamx/TEAM.md"   # empty file -> parse error -> warning, create still ok
OUT3=$( cd "$WORK" && "$TEAMX" team create "Broken" --session s:owner3 --json )
echo "$OUT3" | python3 -c "import json,sys; d=json.load(sys.stdin); assert d.get('ok') is True, d; assert 'error' in d.get('teamfile',{}), d" || fail "create should succeed with warning"
pass "invalid TEAM.md -> create ok + warning"

step "TF-108: TEAM.md ## 文档 -> _spec contract snapshots"
mkdir -p "$WORK/.teamx"
cat > "$WORK/.teamx/TEAM.md" << 'EOF'
# 文档驱动项目

## 文档

### requirements
- 标题: 需求文档
- 用途: 定义产品需求与验收标准
- 模板: 背景 | 目标 | 用户故事 | 验收标准
- 创建者: [pm]
- 所有者: pm
- 审批者: [reviewer, owner]
- 状态流: draft -> review -> approved -> done
- 变更响应:
    - on created: 通知 pm 细化需求
    - on approved: 通知 developer 开始设计

### issue
- 标题: 缺陷 / 改进请求
- 所有者: team-lead
- 状态流: opened -> triaged -> assigned -> fixing -> verified -> closed

### incomplete-doc
- 标题: 缺失字段的文档
EOF
OUT4=$( cd "$WORK" && "$TEAMX" team create "文档驱动项目" --session s:owner4 --json )

# create must succeed and output docs info
echo "$OUT4" | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert d.get('ok') is True, d
docs=d.get('teamfile',{}).get('docs')
assert docs is not None, 'docs info missing in output'
keys=[x['key'] for x in docs]
assert 'requirements' in keys and 'issue' in keys, keys
assert 'incomplete-doc' in keys, keys
# complete docs have spec_file; incomplete doc is flagged
for x in docs:
    if x['key'] in ('requirements','issue'):
        assert x['spec_file'], x
    if x['key']=='incomplete-doc':
        assert x['incomplete'] is True, x
print('docs:', [(x['key'], x.get('incomplete', False)) for x in docs])
" || fail "docs bootstrap output invalid"

# spec snapshot files written to .teamx/docs/_spec/
[ -f "$WORK/.teamx/docs/_spec/requirements.json" ] || fail "requirements.json missing"
[ -f "$WORK/.teamx/docs/_spec/issue.json" ] || fail "issue.json missing"
[ ! -f "$WORK/.teamx/docs/_spec/incomplete-doc.json" ] || fail "incomplete doc should not be snapshotted"
python3 -c "
import json
s=json.load(open('$WORK/.teamx/docs/_spec/requirements.json'))
assert s['doc']=='requirements'
assert s['states']==['draft','review','approved','done'], s['states']
assert s['owner']=='pm'
assert len(s['reactions'])==2, s['reactions']
assert s['reactions'][0]['on']=='created'
assert s['reactions'][0]['to_role']=='pm'
print('requirements spec:', s['states'], s['reactions'])
" || fail "requirements.json content invalid"
python3 -c "
import json
s=json.load(open('$WORK/.teamx/docs/_spec/issue.json'))
assert s['doc']=='issue'
assert s['states'][-1]=='closed', s['states']
assert s['template']==[], s['template']
print('issue spec states:', s['states'])
" || fail "issue.json content invalid"
pass "doc contract snapshots written to .teamx/docs/_spec/"

echo
echo "ALL PASS"
