---
status: completed
protocol_version: 1
owner: pre-team human leader (caishu)
created_at: 2026-09-04
updated_at: 2026-09-04
---

# Team 定义向导：交互式产出 TEAM.md（增强版 team-start）

## Context

teamx 已有两条"设计会话"路径：
- **team-grill**（`protocols/grill-with-docs.md`）：通用需求/设计访谈，任意团队成员可发起，产出 `docs/design/<slug>.md`。
- **team-start**（`opencode-plugin/assets/command/team-start.md`，手写）：把 grill 文本改输出到 `.teamx/TEAM.md` 的"团队契约"设计，之后 `/team create` 消费。

缺口：客户/用户（可能完全不懂 teamx）要"定义一支团队"时，需要一个**更友好、结构化的引导向导**，逐步把"团队怎么组"问清楚，产出**可校验、可被 `team create` 直接消费的 `.teamx/TEAM.md`**。它复用 grill 的 DQ 决策沉淀方法，但呈现成客户可走的向导。

`team create` 消费的 TEAM.md 契约（`teamfile.rs` 解析）：
```
# Team Name
## 背景   自由文本
## 目标   ≤3 条
## 成员   ### <key>: 姓名/角色/分工/技能/输出
## 文档   （可选）doc 类型 + 状态流 draft→review→approved→done + reactions
```

## Settled Decisions

| ID | 决策 | 状态 |
|---|---|---|
| DQ-0001 | 会话形态 = **分步线性向导**（每步一屏、客户友好）；内部仍用 DQ 记录决策 | ✅ |
| DQ-0002 | 入口 = 增强 **`/team start`**（不新开命令名） | ✅ |
| DQ-0003 | 内置常见团队**模板**（3–4 个）+ 角色白话解释；**同时允许用户自定义** | ✅ |
| DQ-0004 | 产出 TEAM.md 前做**校验**（新增 `teamx teamfile validate` 思路，必填/角色目录/唯一性/状态流） | ✅ |
| DQ-0005 | 主产出 = **`.teamx/TEAM.md`**（可被 `team create` 消费）+ 会话记录 | ✅ |
| DQ-0006 | 已存在 TEAM.md 时：向导完成后**提示用户选择「更新(增量合并) 或 覆盖」**，不静默处理 | ✅ |
| DQ-0007 | 定义完**不自动建队**；结束页提示 `/team create <团队名>` 由用户发起 | ✅ |
| DQ-0008 | 术语**说人话**：向导内嵌白话术语表，跟随用户语言 | ✅ |
| DQ-0009 | 步骤 = **7 步**：①团队名 ②背景 ③目标(≤3) ④角色(模板/目录/自定义) ⑤成员分配(姓名/角色/分工/技能/输出) ⑥文档契约(可选) ⑦预览+校验；每步可跳/稍后，结束页列出缺口 | ✅ |
| DQ-0010 | 模板库 = **4 个**：研发交付 / 内容·用研 / 通用协作 / 从零自定义；只预置角色建议，成员留空待第⑤步 | ✅ |
| DQ-0011 | 校验规则 = 名非空/背景非空/目标≤3/≥1 owner 画像/成员 key 唯一/角色在目录/文档状态流合法；错误**内联到步骤**，预览页列未通过项，全过才可保存 | ✅ |
| DQ-0012 | 白话术语表：团队/目标/角色/成员/owner/co-lead/contributor/reviewer/goal 状态/TEAM.md(章程)/邀请函(letter)；侧栏常驻折叠 | ✅ |
| DQ-0013 | 过程记录放 `docs/design/`；默认建 `<team-slug>-team.md`（可 `--resume`），可选 `--no-record` | ✅ |
| DQ-0014 | 结束页 = 预览+校验+「更新/覆盖」+一行 `/team create <名>`（不自动执行）+ 缺口清单；可选 `--then-create` | ✅ |

## Current Frontier

空（全部 DQ settled）。

## Remaining Branches

无。等待 owner 确认 Shared Understanding 后标记 completed，产出设计文档（`docs/36-…`）再进入实现。

## Fact Requests and Reports

无。

## Related Artifacts

- `docs/23-manual-grill-with-docs-usage.md`、`protocols/grill-with-docs.md`
- `opencode-plugin/assets/command/team-start.md`
- `crates/teamx/src/teamfile.rs`（TEAM.md 契约解析）
- `crates/teamx/src/commands.rs`（`cmd_team_create` 的 TEAM.md bootstrap）
