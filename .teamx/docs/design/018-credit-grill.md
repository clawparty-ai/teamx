---
status: active
protocol_version: 1
owner: caishu
created_at: 2026-09-25
updated_at: 2026-09-25
---

# 018 — credit 评分模型（grill 设计访谈）

> grill-with-docs 协议的 Design Session Record。范围：**credit（租户/团队信用）** ——
> 描述**履约行为**的记录与评分（如"分包工作如期支付 member TCoin"）。
> 不含：TCoin 计量与账本（已由 design/017 定稿，见 ADR-0003）。

## Context

`credit` 是 015 与 TCoin 并列留出的两个团队属性之一：

- **015 SD-5**：租户新增两个属性 —— **TCoin**（teamx 代币，租户在 teamx 内的货币）与
  **credit**（租户信用，如"分包工作如期支付 member TCoin"等**行为记录**）。
- **015 SD-21**：右上角用户名菜单 = `我的团队` / `我的申请` / `TCoin（占位）` / **`credit（占位）`** /
  `设置` / `退出` —— 即 credit 的**展示入口已预留**，模型未定。
- **015 Remaining Branches**：`credit 评分模型 | 延后 → 独立 grill | 记什么行为（如如期支付 TCoin）、
  谁能写、可否申诉`。
- **CONTEXT.md**：`credit`｜租户的**信用**记录，用于描述履约行为；**评分模型待独立 grill**。

TCoin 定稿后（017）：

- TCoin 回答"**钱从哪来、按什么花、谁能记、记在哪**"（两级账本、activity 为唯一计量源、只记账不阻断）。
- credit 要回答的是另一个轴：**"这个团队/这个成员可不可信"** ——
  记哪些行为、怎么算分、谁能写、给谁看、**有没有后果**。
- 017 SD-8 已确立平台基调：v1 **只记账/只展示，不阻断任何操作**；软告警而非硬门槛。

## 已核实事实（fact-finding）

- **F1** 代码**零命中** credit 概念：`crates/teamx/src/*.rs` 中仅 3 处命中，全部是无关词
  （`tcoin.rs` 的 "welcome credit" 注释、`platform_console.rs` 的菜单占位 `credit（未启用）`）。
  **无任何评分/信誉/排行/申诉机制**。
- **F2** 行为数据源（真机可查、服务端已有）：
  - **台账 `events`**（最富）：`doc.created/acknowledged/claimed/done/verified/rejected/help_requested/updated`、
    `progress`、`decision`、`blocked`、`resumed`、`achieved`、`refine`…… 带 `team_id/member_id/seq/created_at`。
  - **`activity`**：按 (member, node, 时间窗) 的 LLM `tokens_*` 与 `cost`（**017 已定为唯一计量源**）。
  - **`team_join_requests`**：申请 / 批准 / 拒绝 / 撤回 + **7 天冷却**（015 SD-24）。
  - **`members`**：角色、`state`、`is_lead`、`joined_at` / `left_at`（016 SD-11 离开即释放）。
  - **`goals` / 目标状态机**、**`questions`**（clarification asked/responded）、**`discussion*`**。
  - taskx 任务的状态机在**文档 `.meta.json`**（服务器侧 taskx 文档），台账里有对应 `doc.*` 转换事件 ——
    即"任务履约"可由台账重建，**不必读 git**。
- **F3** **TCoin v1 没有支付链**（017 SD-6：只有平台赠送/发放，无支付入口）→
  015 SD-5 举的"**如期支付 member TCoin**"这个履约信号 **v1 不成立**；
  能用的财务类信号只有"**欠费**"（余额为负，017 允许负余额）与"发放后是否被结算"。
- **F4** 归属现状：`credit` 按 015 SD-5 的措辞属于**租户**（团队）；但 015 SD-21 把入口放在
  **用户名菜单**（用户面）→ **主体（团队 / 成员 / 用户）必须先定**，否则数据落点与展示面都会错。
- **F5** 数据落点问题同 ADR-0001：**列表/目录只读平台库**；若 credit 要在「全部团队」公开，
  平台侧必须有**投影/快照**（团队级分数），成员级明细留在租户侧（与 TCoin 两级账本同构）。
- **F6** 无申诉、无人工调整接口、无防刷设计 —— 均为空白。

## Design Tree

| DQ | 问题 | 状态 | 结论 |
|---|---|---|---|
| DQ-0001 | **主体**：credit 属于谁（团队 / 成员 / 用户） | ⬜ open | — |
| DQ-0002 | **可见性**：谁能看（公开分档 / 成员 / owner） | ⬜ open | — |
| DQ-0003 | **正向信号**：记哪些履约行为 | ⬜ open | — |
| DQ-0004 | **负向信号**：记哪些失信行为 | ⬜ open | — |
| DQ-0005 | **是否纳入 TCoin 财务行为**（欠费 / 结算） | ⬜ open | — |
| DQ-0006 | **计分形式**：事件累计 → 分数 + 分档 / 多维向量 | ⬜ open | — |
| DQ-0007 | **时效**：永久累计 / 滚动窗口 / 时间衰减 | ⬜ open | — |
| DQ-0008 | **冷启动**：新团队基线分与"数据不足" | ⬜ open | — |
| DQ-0009 | **谁能写**：系统派生 / owner 手动 / 成员互评 | ⬜ open | — |
| DQ-0010 | **申诉**：v1 有无复核通道 | ⬜ open | — |
| DQ-0011 | **防刷**：去重 / 冷却 / 限流 | ⬜ open | — |
| DQ-0012 | **后果**：credit 是否影响排序 / 审批 / 配额 | ⬜ open | — |
| DQ-0013 | **展示面**：用户名菜单实页 / 团队详情 / 目录卡 | ⬜ open | — |
| DQ-0014 | **与 TCoin 的关系**：同一计量源还是正交两轴 | ⬜ open | — |

## Frontier（Round 1）

**主题：主体与范围 + 信号清单 + 算法形态**（DQ-0001…DQ-0008）

建议（待 owner 裁决）：

1. **DQ-0001 主体 = 团队级**（v1）。理由：015 SD-5 措辞是"租户信用"；「全部团队」目录需要
   *团队可不可信* 的信号；成员级评分涉及隐私互评，v2 再说。
2. **DQ-0002 可见性 = 公开分档**（目录卡/详情显示 S/A/B/C/D），**精确分与构成只给成员/owner**。
3. **DQ-0003 正向信号（v1 只用台账 + TCoin，可自动派生）**：`doc.verified` 完成数、
   `doc.done → verified` 转化率、批准后成员**实际入场率**（016 批准即拉 Pod）、成员留存时长、
   TCoin **无欠费**。
4. **DQ-0004 负向信号**：`doc.rejected` 次数、申请被拒/冷却（015 SD-24）、
   TCoin **余额为负**（欠费）、长时间 `blocked` 未处理。
5. **DQ-0005 = 只纳入"欠费"**（负向），**不纳入"及时支付"**（v1 无支付链，F3）。
6. **DQ-0006 = 事件加权累计 → 映射 0–100 分 + S/A/B/C/D 分档**，同时保留**构成明细**
   （哪些事件贡献多少分）以便解释与申诉 —— 不做黑箱。
7. **DQ-0007 = 滚动窗口 90 天 + 线性时间衰减**（近期行为权重高）；全期累计仅作审计。
8. **DQ-0008 = 基线 60 分（B 档）**；事件数 < N（建议 5）时显示"数据不足（新团队）"，
   **不显示分数**（避免新团队被标签化）。

**Round 2（待 Round 1 settled）**：DQ-0009…DQ-0014（治理、后果、展示、与 TCoin 关系）。

## Open Questions

（owner 待答：Round 1 八问）

## Fact Requests and Reports

（暂无）

## Remaining Branches（本 session 之外）

- 成员级 / 用户级 credit（v2）
- credit 影响目录排序与加入审批（若 DQ-0012 选"有后果"）
- credit ↔ 支付链（等 TCoin 有支付入口后，"如期支付"才可计量）

## Related Artifacts

- `design/015`（SD-5 / SD-21 / SD-24：credit 属性、菜单占位、拒绝 7 天冷却）
- `design/016` + `adr/ADR-0002`（批准即拉 Pod、离开即释放 —— 履约信号的挂载点）
- `design/017` + `adr/ADR-0003`（TCoin 两级账本、activity 唯一计量源、只记不拦）
- `requirements/002`（NFR-7：v1 计量但不计费、不阻断）
- `CONTEXT.md`（术语：credit 现状为"评分模型待设计"）
