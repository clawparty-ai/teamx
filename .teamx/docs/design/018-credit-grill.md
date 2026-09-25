---
status: completed
protocol_version: 1
owner: caishu
created_at: 2026-09-25
updated_at: 2026-09-25（Shared Understanding 已确认）
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

## Settled Decisions

（owner 已明确 / 上游已定）
- **SD-1** credit = **租户/团队信用**，记录**履约行为**（015 SD-5）；入口已由 015 SD-21
  预留在**用户名菜单**（占位）。
- **SD-2** credit 与 TCoin 是**两个独立属性**（015 SD-2）—— TCoin 是货币与计量，credit 是可信度；
  本 grill 只做 credit，不重开 TCoin。
- **SD-3** 基调继承 017 SD-8：v1 **只记录、只展示、不阻断**；软提示而非硬门槛。

## Design Tree

| DQ | 问题 | 状态 | 结论 |
|---|---|---|---|
| DQ-0001 | **主体**：credit 属于谁（团队 / 成员 / 用户） | ✅ SD-4 |
| DQ-0002 | **可见性**：谁能看（公开分档 / 成员 / owner） | ✅ SD-5 |
| DQ-0003 | **正向信号**：记哪些履约行为 | ✅ SD-6 |
| DQ-0004 | **负向信号**：记哪些失信行为 | ✅ SD-7 |
| DQ-0005 | **是否纳入 TCoin 财务行为**（欠费 / 结算） | ✅ SD-8 |
| DQ-0006 | **计分形式**：事件累计 → 分数 + 分档 / 多维向量 | ✅ SD-9 |
| DQ-0007 | **时效**：永久累计 / 滚动窗口 / 时间衰减 | ✅ SD-10 |
| DQ-0008 | **冷启动**：新团队基线分与"数据不足" | ✅ SD-11 |
| DQ-0009 | **谁能写**：系统派生 / owner 手动 / 成员互评 | ✅ SD-12 |
| DQ-0010 | **申诉**：v1 有无复核通道 | ✅ SD-13 |
| DQ-0011 | **防刷**：去重 / 冷却 / 限流 | ✅ SD-14 |
| DQ-0012 | **后果**：credit 是否影响排序 / 审批 / 配额 | ✅ SD-15 |
| DQ-0013 | **展示面**：用户名菜单实页 / 团队详情 / 目录卡 | ✅ SD-16 |
| DQ-0014 | **与 TCoin 的关系**：同一计量源还是正交两轴 | ✅ SD-17 |

## Current Frontier

### Round 1 决定（2026-09-25，owner 全接受建议）

- **SD-4（DQ-0001）** **主体 = 团队级**（v1）。成员级/用户级留 v2。
  用户名菜单的 credit 页 = 「**我的团队信用**」列表（团队为主体，用户为视角）。
- **SD-5（DQ-0002）** **可见性**：对外**公开分档**（S/A/B/C/D 徽章）；
  **精确分 + 构成明细只给成员/owner**。
- **SD-6（DQ-0003）** **正向信号**（全部可由台账/TCoin 自动派生）：
  `doc.verified` 完成数 · `doc.done → verified` 转化率 · 批准后成员**实际入场率**（016 批准即拉 Pod）·
  成员留存时长 · TCoin **无欠费**。
- **SD-7（DQ-0004）** **负向信号**：`doc.rejected` 次数 · 申请被拒/冷却（015 SD-24）·
  TCoin **余额为负（欠费）** · 长时间 `blocked` 未处理。
- **SD-8（DQ-0005）** **TCoin 财务行为只纳入"欠费"**（负向）；**不纳入"及时支付"** ——
  v1 无支付链（F3），等支付入口出现再议（Remaining Branch）。
- **SD-9（DQ-0006）** **计分形式**：事件加权累计 → 映射 **0–100 分 + S/A/B/C/D 分档**，
  且**必须保留构成明细**（每个事件贡献多少分）—— 不做黑箱。
- **SD-10（DQ-0007）** **时效**：**滚动 90 天 + 线性时间衰减**（近期权重高）；
  全期累计仅作审计留档，不参与评分。
- **SD-11（DQ-0008）** **冷启动**：基线 **60 分（B 档）**；有效事件 < **5** 条时显示
  「**数据不足（新团队）**」且**不显示分数**（不标签化新团队）。

### Round 2 决定（2026-09-25，owner 全接受建议）

- **SD-12（DQ-0009）** **系统派生 = 唯一写者**：credit 由台账事件/TCoin 余额**重算**得出，
  **任何人不可手改**（owner 也不可，**无手调接口**）。→ 见 ADR-0004。
- **SD-13（DQ-0010）** v1 **无申诉通道**；提供**只读解释**（构成明细：每个事件贡献多少分）。
  申诉留到"有后果"时再议（Remaining Branch）。
- **SD-14（DQ-0011）** **防刷三道防线**：①同一 `doc`/事件**去重只计一次** ②90 天窗口**自然衰减**
  ③单团队单日正向计分**封顶**；拒绝类信号由 **owner 决定**、成员不可自刷。
- **SD-15（DQ-0012）** v1 credit **只展示、不影响**任何排序/审批/配额（与 017 SD-8「只记不拦」同构）。
  未来可用于目录排序/加入审批参考 —— Remaining Branch，需再裁决。
- **SD-16（DQ-0013）** **展示面**：用户名菜单 **credit 实页**（「我的团队信用」：分档 + 分数 + 构成解释）
  + **团队详情块** + **「全部团队」目录卡分档徽章**。
- **SD-17（DQ-0014）** credit 与 TCoin **正交两轴、共享底座**（同一 `events`/`activity` 源）；
  credit **只读不写账**；「欠费」= credit **读** TCoin 余额（**单向依赖**，credit 不反写 TCoin）。

**Frontier 空**（本 session 范围内全部 settled）。

## Open Questions

（空 —— Round 2 后 Frontier 已清空；owner 已确认 Shared Understanding）

## Fact Requests and Reports

（暂无）

## Remaining Branches（本 session 之外）

- 成员级 / 用户级 credit（v2）
- credit 影响目录排序与加入审批（若 DQ-0012 选"有后果"）
- credit ↔ 支付链（等 TCoin 有支付入口后，"如期支付"才可计量）

## 实现状态（2026-09-25）

**已实现**（代码仓 `main`，3 个提交）：

| 层 | 内容 |
|---|---|
| 打分核心 `credit.rs` | **纯函数** `score()`：事件加权累计 → 0–100 + S/A/B/C/D + **构成明细**；基线 60；**滚动 90 天线性衰减**；`doc.*` 按 **doc id 去重**；**单日正向封顶**（防刷）；`<5` 事件 → `insufficient`；权重表 `W` + `MODEL_VERSION`（调参须升版本） |
| 租户侧 | `tenant_signals`（只读租户库）：verified/done 去重计数、`doc.rejected`、长期 `blocked`（>14 天）、在册成员数与平均在册时长 |
| 平台侧 | `platform_signals`：加入申请批准/拒绝、**TCoin 余额（欠费，单向读）**；`merge` 合并两侧 |
| 物化 | 平台库 `team_credit`（团队级快照）→ 满足 **ADR-0001「目录只读平台库」**；`public_grade` 在 `insufficient` 时返回 null |
| service 车道 | `credit.stats`（白名单，**只回原始计数**——评分唯一发生在平台侧，SD-12） |
| 平台编排 | `refresh_credit`（租户 stats → 平台信号 → 打分 → 物化）、`credit_view` |
| CLI | `teamx credit refresh [--team|--all]` / `credit show [--team|--all]` + RPC `credit.*` |
| 控制台 | `GET /api/my/credit`（成员可见精确分+构成）、`GET /api/my/credit/{team}`（归属校验）、`POST /api/my/credit/{team}/refresh`（owner-only）；目录列表/详情带 **`credit_grade`（只给分档）** |
| UI | 用户名菜单 **credit 实页**（列表→构成明细分栏 + 刷新）、团队详情 **credit 块**、目录卡 **信用徽章** |
| 测试 | 单测 **11 组**（分档阈值/基线+insufficient/正信号与转化率/分类封顶/欠费差/确定性与下限/去重与事件总数/窗口衰减/物化与公开分档/平台信号与欠费/merge 入场率）；E2E **I0–I5**（52 断言） |

### 实现中发现并修正的问题

**负向全局封顶导致 D 档不可达**：初版把负向扣分统一封顶 30，配合基线 60 → 分数下限 30，
**D 档（<40）几乎不可达**。改为**按类封顶**（欠费 10 / 拒绝 12 / 驳回 9 / 阻塞 12，合计 −43）
→ 极端可到 **17 分 = D 档**，同时不至于一次失误把团队打到谷底。

### 真机验证（k8e 单节点，`teamx-server:dev` 重建导入）

建团队 → 自动通道 bootstrap（等待 Pod 就绪）→ 导入 owner 信认领座位 → 租户侧注入 6 个
`doc.verified`（去重）+ 8 个 `doc.done` + 1 个 `doc.rejected` + 1 个 14 天前 `blocked` 目标
→ TCoin `grant 100` + `settle 250`（**余额 −150 = 欠费**）→ 控制台 `credit refresh`：

```
score 59 · grade C · insufficient false · events 18
  完成（doc.verified） 6 个            +10.0   （被单日正向封顶 10 截断 → 防刷生效）
  完成转化率          6/8 = 75%        +7.5
  成员留存            1 人 · 平均 0 天  +0.0   （刚加入，正确）
  欠费                余额 −150        −10.0
  任务被驳回          1 次             −3.0
  长期阻塞            1 个目标         −6.0
```

- **TCoin 单向影响**得证：补款 500（余额 350）后刷新 → **59 → 73（C → B）**，负向「欠费 −10」
  变为正向「无欠费 +4」；**重复刷新同分（幂等）**。
- 目录列表与详情 **公开徽章 = C**（`insufficient=0`）；`insufficient=1` 时徽章为 null（不贴标签）。

### 遗留 / 未决

- **权重与阈值是拍出来的**（基线 60、<5 事件、90 天、各类封顶、单日 +10）：属**可调参数**，
  需按真实数据回看；调整时必须升 `MODEL_VERSION`，否则历史分数不可解释。
- **credit 目前只由 owner 手动刷新**（v1 无定时任务）；目录徽章可能是**陈旧快照**，
  UI 已显示「上次刷新时间」。自动刷新周期待定（可复用 worker）。
- 成员级/用户级 credit、申诉通道、credit 用于排序/审批 → 均为 Remaining Branches。

## Related Artifacts

- `adr/ADR-0004-credit-derived-score.md`（本 session 产出）
- `design/015`（SD-5 / SD-21 / SD-24：credit 属性、菜单占位、拒绝 7 天冷却）
- `design/016` + `adr/ADR-0002`（批准即拉 Pod、离开即释放 —— 履约信号的挂载点）
- `design/017` + `adr/ADR-0003`（TCoin 两级账本、activity 唯一计量源、只记不拦）
- `requirements/002`（NFR-7：v1 计量但不计费、不阻断）
- `CONTEXT.md`（术语：credit 现状为"评分模型待设计"）
