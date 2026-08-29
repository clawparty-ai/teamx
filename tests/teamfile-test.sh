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

step "TF-201: doc lifecycle — issue created (doc.created) then advanced (doc.reviewed)"
mkdir -p "$WORK/.teamx"
cat > "$WORK/.teamx/TEAM.md" << 'EOF'
# 文档驱动项目

## 成员
### owner
- 姓名: 项目负责人
- 角色: owner
- 分工: 目标定义与总体把关

### lead
- 姓名: 组长
- 角色: team-lead
- 分工: 分诊并指派 issue

### reviewer1
- 姓名: 评审甲
- 角色: reviewer
- 分工: 代码评审

## 文档

### issue
- 标题: 缺陷 / 改进请求
- 创建者: [owner, team-lead, reviewer, contributor]
- 所有者: owner
- 审批者: [team-lead, reviewer]
- 状态流: opened -> triaged -> assigned -> fixing -> verified -> closed
- 变更响应:
    - on created: 通知 team-lead 分析并分诊
    - on reviewed: 通知 reviewer 复核

### requirements
- 标题: 需求文档
- 创建者: [owner, team-lead]
- 所有者: owner
- 审批者: [team-lead, reviewer]
- 状态流: draft -> review -> approved -> done
- 变更响应:
    - on approved: 通知 reviewer 复核设计
EOF
OUT5=$( cd "$WORK" && "$TEAMX" team create "文档驱动项目" --session s:lead5 --json )
LEAD_ID=$( echo "$OUT5" | python3 -c "import json,sys; d=json.load(sys.stdin); print([m['key'] for m in d['teamfile']['members']])" )
echo "$OUT5" | python3 -c "
import json,sys
d=json.load(sys.stdin)
tf=d['teamfile']
keys=[m['key'] for m in tf['members']]
assert 'lead' in keys and 'reviewer1' in keys, keys
" || fail "TF-201 team with members failed"

# find the team id + lead member id
TID=$( echo "$OUT5" | python3 -c "import json,sys; print(json.load(sys.stdin)['team']['id'])" )
LEAD_MID=$( echo "$OUT5" | python3 -c "import json,sys; d=json.load(sys.stdin); print([m['member_id'] if 'member_id' in m else '' for m in d['teamfile']['members']])" )
echo "  team=$TID members=$LEAD_ID"

# Import the team-lead + reviewer letters so reactions can match their roles.
LEAD_LETTER=$( cat "$WORK/.teamx/members/lead/invitation.letter" )
LEAD_IMP=$( cd "$WORK" && "$TEAMX" team import "$LEAD_LETTER" --name 组长 --session s:leadm --json )
LEAD_MID=$( echo "$LEAD_IMP" | python3 -c "import json,sys; print(json.load(sys.stdin).get('member_id',''))" )
REV_LETTER=$( cat "$WORK/.teamx/members/reviewer1/invitation.letter" )
REV_IMP=$( cd "$WORK" && "$TEAMX" team import "$REV_LETTER" --name 评审甲 --session s:reviewerm --json )
REV_MID=$( echo "$REV_IMP" | python3 -c "import json,sys; print(json.load(sys.stdin).get('member_id',''))" )
# Approve them (owner session s:lead5) so they become active and can receive
# directed reaction notifications (pending members are intentionally excluded).
( cd "$WORK" && "$TEAMX" team approve "$LEAD_MID" --session s:lead5 --json ) >/dev/null 2>&1 || fail "approve lead"
( cd "$WORK" && "$TEAMX" team approve "$REV_MID" --session s:lead5 --json ) >/dev/null 2>&1 || fail "approve reviewer"
pass "team-lead + reviewer imported + approved (reaction targets available)"

# 1. create an issue (doc.created) by lead
ISSUE=$( cd "$WORK" && "$TEAMX" publish doc.created --data '{"doc":"issue","id":"042-slow","note":"上传慢"}' --session s:lead5 --json )
echo "$ISSUE" | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert d.get('ok') is True, d
assert d['doc']=='issue' and d['state']=='opened', d
" || fail "TF-201 issue created"
pass "issue doc.created -> opened"

# 2. lead advances opened -> triaged (owner can advance)
ADV=$( cd "$WORK" && "$TEAMX" publish doc.reviewed --data '{"doc":"issue","id":"042-slow","to":"triaged","note":"已分诊"}' --session s:lead5 --json )
echo "$ADV" | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert d.get('ok') is True, d
assert d['state']=='triaged', d
assert d['from_state']=='opened', d
# reaction to reviewer should have been emitted (on triaged -> reviewer)
notified=[n for n in d.get('notified',[]) if n.get('to_role')=='reviewer']
assert len(notified)>=1, d
" || fail "TF-201 issue advanced"
pass "issue doc.reviewed opened->triaged + reviewer notified"

# 3. meta file persisted
[ -f "$WORK/.teamx/docs/issue/042-slow.meta.json" ] || fail "issue meta missing"
python3 -c "
import json
m=json.load(open('$WORK/.teamx/docs/issue/042-slow.meta.json'))
assert m['state']=='triaged', m
assert len(m['history'])==2, m['history']
" || fail "issue meta state/history invalid"
pass "issue meta.json persisted (state=triaged, 2 history steps)"

step "TF-202: doc validation — duplicate create + illegal transition rejected"
# duplicate create must fail (instance already exists)
DUP=$( cd "$WORK" && "$TEAMX" publish doc.created --data '{"doc":"issue","id":"042-slow"}' --session s:lead5 --json 2>&1 || true )
echo "$DUP" | grep -q "already exists" || fail "duplicate doc.create should be rejected"
pass "duplicate doc.created rejected"

# illegal: move backward (verified -> opened) with a forward event
BAD=$( cd "$WORK" && "$TEAMX" publish doc.reviewed --data '{"doc":"issue","id":"042-slow","to":"opened"}' --session s:lead5 --json 2>&1 || true )
echo "$BAD" | grep -q "illegal transition" || fail "backward move should be rejected: $BAD"
pass "illegal backward transition rejected"

# unknown state rejected
BADSTATE=$( cd "$WORK" && "$TEAMX" publish doc.reviewed --data '{"doc":"issue","id":"042-slow","to":"bogus"}' --session s:lead5 --json 2>&1 || true )
echo "$BADSTATE" | grep -q "not in declared flow" || fail "unknown state should be rejected: $BADSTATE"
pass "unknown state rejected"

# missing doc key in payload rejected
NOKEY=$( cd "$WORK" && "$TEAMX" publish doc.created --data '{"id":"x"}' --session s:lead5 --json 2>&1 || true )
echo "$NOKEY" | grep -q "requires a \`doc\` key" || fail "missing doc key should be rejected"
pass "missing doc key rejected"

step "TF-203: unregistered doc type rejected"
UNREG=$( cd "$WORK" && "$TEAMX" publish doc.created --data '{"doc":"unknown-type","id":"x"}' --session s:lead5 --json 2>&1 || true )
echo "$UNREG" | grep -q "not recognized" || fail "unregistered doc type should be rejected: $UNREG"
pass "unregistered doc type rejected"

step "TF-204: full lifecycle — issue through all states to closed"
# Create a fresh issue to avoid state contamination
FULL=$( cd "$WORK" && "$TEAMX" publish doc.created --data '{"doc":"issue","id":"099-full-lifecycle","note":"full lifecycle test"}' --session s:lead5 --json )
echo "$FULL" | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert d.get('ok') is True and d['state']=='opened', d
" || fail "TF-204 issue created"
pass "full lifecycle: opened"

# opened -> triaged
S2=$( cd "$WORK" && "$TEAMX" publish doc.reviewed --data '{"doc":"issue","id":"099-full-lifecycle","to":"triaged"}' --session s:lead5 --json )
echo "$S2" | python3 -c "
import json,sys; d=json.load(sys.stdin)
assert d.get('ok') is True and d['state']=='triaged', d
" || fail "TF-204 triaged"
pass "full lifecycle: triaged"

# triaged -> assigned
S3=$( cd "$WORK" && "$TEAMX" publish doc.reviewed --data '{"doc":"issue","id":"099-full-lifecycle","to":"assigned"}' --session s:lead5 --json )
echo "$S3" | python3 -c "
import json,sys; d=json.load(sys.stdin)
assert d.get('ok') is True and d['state']=='assigned', d
" || fail "TF-204 assigned"
pass "full lifecycle: assigned"

# assigned -> fixing
S4=$( cd "$WORK" && "$TEAMX" publish doc.reviewed --data '{"doc":"issue","id":"099-full-lifecycle","to":"fixing"}' --session s:lead5 --json )
echo "$S4" | python3 -c "
import json,sys; d=json.load(sys.stdin)
assert d.get('ok') is True and d['state']=='fixing', d
" || fail "TF-204 fixing"
pass "full lifecycle: fixing"

# fixing -> verified
S5=$( cd "$WORK" && "$TEAMX" publish doc.reviewed --data '{"doc":"issue","id":"099-full-lifecycle","to":"verified"}' --session s:lead5 --json )
echo "$S5" | python3 -c "
import json,sys; d=json.load(sys.stdin)
assert d.get('ok') is True and d['state']=='verified', d
" || fail "TF-204 verified"
pass "full lifecycle: verified"

# verified -> closed
S6=$( cd "$WORK" && "$TEAMX" publish doc.reviewed --data '{"doc":"issue","id":"099-full-lifecycle","to":"closed"}' --session s:lead5 --json )
echo "$S6" | python3 -c "
import json,sys; d=json.load(sys.stdin)
assert d.get('ok') is True and d['state']=='closed', d
" || fail "TF-204 closed"
pass "full lifecycle: closed"

# meta shows full 6-step history
python3 -c "
import json
m=json.load(open('$WORK/.teamx/docs/issue/099-full-lifecycle.meta.json'))
assert m['state']=='closed', m
assert len(m['history'])==6, 'expected 6 history steps, got %d' % len(m['history'])
print('full lifecycle meta:', m['state'], 'history:', len(m['history']), 'steps')
" || fail "TF-204 meta invalid"
pass "full lifecycle meta: closed + 6 history steps"

step "TF-205: multi-doc-type independence — issue + requirements coexist"
# Create a requirements doc (separate doc type, separate instance)
REQ=$( cd "$WORK" && "$TEAMX" publish doc.created --data '{"doc":"requirements","id":"req-v1","note":"v1 需求"}' --session s:lead5 --json )
echo "$REQ" | python3 -c "
import json,sys; d=json.load(sys.stdin)
assert d.get('ok') is True and d['state']=='draft', d
" || fail "TF-205 requirements created"
pass "multi-doc: requirements created (state=draft)"

# Issue and requirements have independent states
ISSUE_STATE=$( cd "$WORK" && "$TEAMX" publish doc.created --data '{"doc":"issue","id":"099-full-lifecycle"}' --session s:lead5 --json 2>&1 || true )
echo "$ISSUE_STATE" | grep -q "already exists" || fail "issue should still exist"
pass "multi-doc: issue state independent (still exists)"

# Advance requirements: draft -> review
REQ_ADV=$( cd "$WORK" && "$TEAMX" publish doc.reviewed --data '{"doc":"requirements","id":"req-v1","to":"review"}' --session s:lead5 --json )
echo "$REQ_ADV" | python3 -c "
import json,sys; d=json.load(sys.stdin)
assert d.get('ok') is True and d['state']=='review', d
" || fail "TF-205 requirements advanced"
pass "multi-doc: requirements advanced draft->review"

# Both meta files exist independently
[ -f "$WORK/.teamx/docs/issue/099-full-lifecycle.meta.json" ] || fail "issue meta missing"
[ -f "$WORK/.teamx/docs/requirements/req-v1.meta.json" ] || fail "requirements meta missing"
python3 -c "
import json
issue_meta=json.load(open('$WORK/.teamx/docs/issue/099-full-lifecycle.meta.json'))
req_meta=json.load(open('$WORK/.teamx/docs/requirements/req-v1.meta.json'))
assert issue_meta['state']=='closed', issue_meta
assert req_meta['state']=='review', req_meta
" || fail "multi-doc meta independence broken"
pass "multi-doc: independent meta files"

step "TF-206: path-traversal ids/keys rejected (CR-022 S1)"
# malicious doc id with path separators / traversal
TRAV_ID=$( cd "$WORK" && "$TEAMX" publish doc.created --data '{"doc":"issue","id":"../../etc/evil"}' --session s:lead5 --json 2>&1 || true )
echo "$TRAV_ID" | grep -q "not a safe identifier" || fail "traversal doc id should be rejected: $TRAV_ID"
pass "doc id path traversal rejected"
# malicious doc key
TRAV_KEY=$( cd "$WORK" && "$TEAMX" publish doc.created --data '{"doc":"../escaped","id":"x"}' --session s:lead5 --json 2>&1 || true )
echo "$TRAV_KEY" | grep -q "not a safe identifier" || fail "traversal doc key should be rejected: $TRAV_KEY"
pass "doc key path traversal rejected"

step "TF-207: doc.reaction cannot be published as lifecycle event (CR-022 S2)"
REACT=$( cd "$WORK" && "$TEAMX" publish doc.reaction --data '{"doc":"issue","id":"042-slow","on":"created"}' --session s:lead5 --json 2>&1 || true )
echo "$REACT" | grep -q "system notification" || fail "doc.reaction must be rejected as lifecycle event: $REACT"
pass "doc.reaction rejected as lifecycle event"

step "TF-208: unknown doc event rejected (CR-022 S4)"
UNKNOWN=$( cd "$WORK" && "$TEAMX" publish doc.foobar --data '{"doc":"issue","id":"042-slow","to":"triaged"}' --session s:lead5 --json 2>&1 || true )
echo "$UNKNOWN" | grep -q "unknown doc event" || fail "unknown doc event should be rejected: $UNKNOWN"
pass "unknown doc event rejected"

echo
echo "ALL PASS"
