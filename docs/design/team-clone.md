---
status: completed
protocol_version: 1
owner: e31a5b04-29d3-4e74-8aed-68d643bc9b2a (teamx-core-dev / teamx-core-dev)
created_at: 2026-09-04
updated_at: 2026-09-04
---

# 团队 Clone 设计与实现

## Context

一个 team 同一时间只能执行一个 goal（`teams.goal_id` 单指针）。需要「同一批人 / 同构组织 / 干净上下文」并行执行新目标时，直接在旧团队开第二个 goal 会混乱。

方案：**clone** —— 复制团队结构（成员+角色+空仓库），创建完全独立的新团队，从零状态起步。

## Settled Decisions

| ID | 决策 | 来源 | 状态 |
|---|---|---|---|
| DQ-001 | Owner 不变：clone.owner = 源 owner（同一人，新 member uuid） | 需求确认 | ✅ settled |
| DQ-002 | 成员不变：复制源 team 所有 active 成员（user/role/is_lead 一一对应） | 需求确认 | ✅ settled |
| DQ-003 | auto_approve：所有邀请函 auto_approve，导入即 active | 需求确认 | ✅ settled |
| DQ-004 | 空仓库：为源每个仓库建同名空 bare 仓库 | 需求确认 | ✅ settled |
| DQ-005 | 每 clone 一套独立 workspace + 进程组 | 需求确认 | ✅ settled |
| DQ-006 | 名字用户输入，全库唯一 | 需求确认 | ✅ settled |
| DQ-007 | 完全独立：不继承 events/taskx/questions/nudge/tunnels/goal 历史/git 内容 | 需求确认 | ✅ settled |
| DQ-008 | serve dispatch 按 CN 特判（同 task/inbox/force_leave 模式） | Review A - 用户确认 | ✅ settled |
| DQ-009 | 外层单事务：建队+角色+invitations+owner 回写在一个 with_write 中 | Review B - 用户确认 | ✅ settled |
| DQ-010 | mint_invite 直接接收 role_key+label，不再 label→key 反推 | Review #2 | ✅ settled |
| DQ-011 | mint_invite 含 member_id_override（owner 用预生成 uuid） | Review #3 | ✅ settled |
| DQ-012 | mint_invite 不含 "仅 primary owner 可 as_lead" 检查（clone 有意绕过） | Review #4 | ✅ settled |
| DQ-013 | server_url 用 resolve_server_url()（非 127.0.0.1:5781） | Review #5 | ✅ settled |
| DQ-014 | 单测需设临时 TEAMX_HOME + pki::ensure（避免往真实 ~/.teamx 签证书） | Review #9 | ✅ settled |

## Current Frontier

所有设计问题已 settled。Frontier 为空。

## Remaining Branches

无未决分支。

## Fact Requests and Reports

无 Fact Request。

## Related Artifacts

- `docs/34-team-clone.cn.md`：完整设计文档（v3，含 review 修正）
- `docs/34-team-clone.cn.md` §4 接口清单：CLI 1 + RPC 1 + 内部函数 3
- `docs/34-team-clone.cn.md` §5 实现逻辑：cmd_team_clone 九步
- `docs/34-team-clone.cn.md` §6 mint_invite 重构
- `docs/34-team-clone.cn.md` §10 测试方案：11 条单元 + e2e
