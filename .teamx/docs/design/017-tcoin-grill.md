---
status: completed
protocol_version: 1
owner: caishu
created_at: 2026-09-24
updated_at: 2026-09-24（Shared Understanding 已确认）
---

# 017 — TCoin 经济模型（grill 设计访谈）

> grill-with-docs 协议的 Design Session Record。范围：**TCoin（teamx 代币）** —— 租户在
> teamx 内的货币；后续承载"用户使用代币"的能力（计量资源、结算、配额）。
> 不含：credit 评分模型（另一个独立 grill）。

## Context

TCoin 被 owner 定义为：**租户在 teamx 里的货币**；用途之一是把"资源占用"货币化 ——
例如 015 SD-22 定的"批准加入即拉起 member Pod，**资源算 owner 的**"，当时明确
"v1 不设硬配额、**只做 TCoin 记账**（记但不拦）"。因此 TCoin 要回答"**钱从哪来、按什么花、
谁能记、记在哪**"。

已核实事实：

- **F1** 全仓**不存在** TCoin / credit / 余额 / 账目任何概念（零命中）。
- **F2** 但已有**计量与定价底座**（LLM 维度）：
  - `activity` 表按 (member, node, 时间窗) 记 `tokens_input/output/reasoning`、`tokens_cache_*`、
    `model_id`、`provider_id`、`cost_*`；
  - `model_pricing` 表按 (provider_id, model_id) 存单价（input/output/cache，含 over200k 分档）；
  - `activity::member_cost_sum(team_id, member_id)` 已能算某成员的成本；
  - 平台控制台已有"成本"看板（`team.metrics` / activity 上传）。
- **F3** 配额现状：`platform_quotas.tenant_quota`（我拥有的**团队数**，默认 3）；
  **成员数/资源无硬配额**（015 SD-22 明确 v1 不设，只记账）。
- **F4** 资源占用的**归属**已定：成员 Pod/工作区**算 owner 的**（015 SD-22/SD-23）；
  **离开/回收即释放**（ADR-0002）。
- **F5** 设计已明确 **v1 不计费不计量**（design/012 D14/D15：v1 不做成本兜底、不做计量埋点；
  `requirements/002` NFR-7 降级 N/A）—— 本 grill 正是**开启**这件事，需与 012/015 的
  "v1 不计费"口径协调（是否改口径、还是只做"记账但不扣费"）。
- **F6** 租户侧数据面在租户 server（每租户独立库）；平台库只有元数据/归属/配额。
  **钱相关的数据放哪一侧，是本次的核心架构选择**（与 ADR-0001 同类的分层问题）。

## Settled Decisions

（owner 已明确）
- **SD-1** TCoin = **租户在 teamx 内的货币**；后续会承载"用户使用代币"的功能。
- **SD-2** credit 是**另一个属性/另一个 grill**（记录租户信用：如分包工作如期支付 member TCoin）。
- **SD-3** 015 已定：v1 成员资源**不设硬配额**，**只做 TCoin 记账（记但不拦）**。

## Design Tree

| DQ | 问题 | 依赖 | 状态 |
|---|---|---|---|
| DQ-0001 | 账本位置（两级） | — | ✅ SD-4 → ADR-0003 |
| DQ-0002 | 谁写账/路径（平台自动 + owner 手动，服务端写） | DQ-0001 | ✅ SD-5 |
| DQ-0003 | 铸币来源（v1 平台赠送额度） | — | ✅ SD-6 |
| DQ-0004 | 消耗口径（v1 只算 LLM tokens） | — | ✅ SD-7 |
| DQ-0005 | 计价与汇率（固定 + 整数记账） | DQ-0004 | ✅ SD-9 |
| DQ-0006 | 与既有成本体系（唯一计量源） | DQ-0004 | ✅ SD-10 |
| DQ-0007 | 与配额关系（双轨、互不触发） | DQ-0002 | ✅ SD-11 |
| DQ-0008 | 余额不足策略（软告警不阻断） | DQ-0007 | ✅ SD-8 |
| DQ-0009 | 账本不可变性（append-only + 冲正） | DQ-0001 | ✅ SD-12 |
| DQ-0010 | 展示面（余额+流水；团队累计） | DQ-0004 | ✅ SD-13 |
| DQ-0011 | 结算周期（实时 + 幂等） | DQ-0004 | ✅ SD-14 |
| DQ-0012 | 成员可见性（自己 / owner 全租户） | DQ-0010 | ✅ SD-15 |
| DQ-0013 | v1 范围 + 012 口径修订 | — | ✅ SD-8 |

**Frontier 空**（本 session 范围内全部 settled）。

## Current Frontier

（空 —— Round 2 后 Frontier 已清空）

### Round 1 决定（2026-09-24）

- **SD-4（DQ-0001 = c）** **两级账本**：**平台库**记租户级**余额/充值/发放**（跨租户视角归平台）；
  **租户 server**记**成员级消耗明细**（LLM tokens 来自租户侧 activity）。需一条**对账**机制 →
  见 ADR-0003。
- **SD-5（DQ-0002 = b）** 写入方 = **平台自动**（消耗按周期落账）+ **owner 手动**（给自己团队充值/分配，
  走 CLI 与 console）；**成员自助消费**留到"用户使用代币"功能；写账一律在**服务端**（service 车道/CLI），
  客户端不直写账本。
- **SD-6（DQ-0003 = c）** v1 **只做平台赠送/发放额度**（无真实支付、无法币入口）；支付通道/定价/发票
  属后续。（v1 目标是"看见钱怎么花"，不是"收钱"。）
- **SD-7（DQ-0004 = a）** v1 消耗口径 = **只算 LLM tokens**（复用 `activity` + `model_pricing`，
  换算成 TCoin）；**member Pod 资源时长**需新埋点，放 v2（开放项 O-1）。
- **SD-8（DQ-0013 = b）** v1 = **记账 + 软告警**（余额低/耗尽给明确提示，**不阻断任何操作**）；
  同时**修订 design/012 D14/D15 口径**：由"v1 不计量"改为"**v1 计量但不计费、不阻断**"，
  `requirements/002` NFR-7 注明变更。理由：token 口径是**滞后上报**，拿它拦人会误伤。

### Round 2 决定（2026-09-24）

- **SD-9（DQ-0005 = a）** **固定汇率 + 整数记账**：`1 TCoin = 0.001 USD`（1000 TCoin = 1 USD）；
  账本只存**整数 TCoin**，换算 `TCoin = round(cost_usd × 1000)`；v1 无动态定价
  （将来引入"定价版本"字段，开放项 O-2）。
- **SD-10（DQ-0006 = a）** `activity` + `model_pricing` 是**唯一计量源**；TCoin = 其"币视图 + 落账"，
  **绝不出现第二套 token 口径**。
- **SD-11（DQ-0007 = b）** 与配额**双轨、分工明确**：配额管"**能不能**"（结构性上限，如 `tenant_quota`），
  TCoin 管"**花了多少**"；v1 **互不触发**（币不足只告警，不影响配额门禁）。
- **SD-12（DQ-0009 = a）** 账本 **append-only 流水 + 反向分录冲正**（不删不改；余额 = SUM；
  冲正的 `reason` 注明）；两级对账以**批次摘要（流水区间）**为凭。
- **SD-13（DQ-0010 = a）** 展示面：用户名菜单「TCoin」→ **实页**（余额 + 分页流水，可追溯到 activity 行）；
  owner 侧在**团队详情**显示"本团队累计消耗"（**不做成员排行**，开放项 O-3）。
- **SD-14（DQ-0011 = a）** **实时落账 + 幂等**（`ref = activity 行 id` 去重）；不做日结批处理
  （量级增长后再评估，开放项 O-4）。
- **SD-15（DQ-0012 = c）** 可见性：**成员看自己**、**owner 看全租户**；不暴露他人明细。

## Remaining Branches

**owner 明确延后（非未决）**：
- **O-1 member Pod 资源时长计量**（CPU/内存×小时、空闲停机口径）→ v2（SD-7）
- **O-2 动态定价 / 定价版本**（汇率浮动、多档）→ 有真实成本波动再引入（SD-9）
- **O-3 成员消耗排行**（团队内比较）→ v2 视需要（SD-13）
- **O-4 日结/批处理结算** → 量级增长后评估（SD-14）
- **O-5 支付通道/法币充值/发票** → 后续（SD-6）
- **O-6 成员自助消费**（用 TCoin 买什么）→ "用户使用代币"功能，独立议题（SD-5）

**本 session 定稿后需同步修订的口径**（SD-8/AC）：`design/012` D14/D15 由"v1 不计量"改为
"**v1 计量但不计费、不阻断**"；`requirements/002` NFR-7 注明变更。
- 独立 grill：**credit 评分模型**
- 未决基础设施：O-5 租户端点（已由"平台自建通道"缓解同机场景）

## Fact Requests and Reports

（暂无）

## 实现状态（2026-09-25）

**已实现**（代码仓 `main`，3 个提交）：

| 层 | 内容 |
|---|---|
| 平台账本 `tcoin.rs` | append-only `tcoin_entries`（`seq/at/team_id/account/delta/reason/ref/actor`）；余额 = `SUM(delta)`；`grant`（发行）/`allocate`（owner→成员内部转账）/`settle`（按 batch **幂等**）/`reverse`（反向分录，重复冲正不重复回补）/`balance`/`entries`；`usd_to_tcoin`（1 TCoin = 0.001 USD，整数四舍五入） |
| 租户侧 | **物化自 `activity`**（唯一计量源，不另存账）：`pending_usage`（逐成员未结算）/`settle_usage`（原子标记 `activity.tcoin_settled_at` + 返回批次摘要） |
| service 车道 | `tcoin.usage`（只读）/`tcoin.settle`（结算）入平台白名单，走 service 身份 |
| 平台编排 | `settle_team_tcoin`（租户结算 → 平台落总账，两级对账）、`tcoin_overview`（余额+流水+租户侧未结算；通道不可达如实报错） |
| CLI | `teamx tcoin grant|allocate|settle|reverse|show` + RPC `tcoin.*` |
| 控制台 | `GET /api/my/tcoin`（各团队余额/我的分配/累计消耗/软告警+最近动态）、`GET /api/my/tcoin/{team}`（归属校验，owner 附成员分配余额）、`POST /api/my/tcoin/{team}/settle`（owner-only）；UI：用户名菜单 **TCoin 实页**（列表→明细分栏）+ 「我的团队」详情 **本团队累计消耗** 块 |
| 测试 | 单测 3 组（汇率取整 / append-only 全流程 / 租户用量物化与一次结算）；E2E H1–H5（46 断言） |

**真机验证**（k8e 单节点，`teamx-server:dev` 重建导入）：建团队 → 自动通道 bootstrap（等待 Pod 就绪 14–20s）
→ 导入 owner 信认领座位（租户 DB 出现 owner 成员）→ 平台 grant 5000 → 租户 pod 内 `activity push` 注入
2 行消耗（1.0 USD）→ 控制台 `settle`：**consumed=1000**、余额 5000→4000、审户侧 2 行全部标记已结算
→ 再结算 **consumed=0（幂等，余额/条数不变）** → `reverse` seq=1：追加 −5000 分录、余额 −1000、
重复冲正 `already=true` 且不重复回补。全程 append-only（条数只增）。

### 真机发现的缺口（已修，3 项）

1. **bootstrap 不等 Pod 就绪** → port-forward 秒退、报无意义错误。修：`wait_tenant_deploy_ready`
   仅在**确认 Deployment 存在**时有界等待（默认 90s，`TEAMX_TENANT_READY_TIMEOUT_SECS` 可覆盖），
   不可解析/报错时不等待（保持快速失败）。
2. **destroy 残留平台 `teams` 行** → public 团队租户已删却在「全部团队」残留幽灵团队
   （`directory` 只按 `state IN ('forming','active')` 过滤）。修：置 `state='destroyed'`（不删除，保留审计）。
3. **`tcoin grant/reverse/settle/show` 要求 member 会话**，但平台库只有 platform 用户 → 平台侧无法写账。
   修：抽 `tcoin_actor`，先按平台会话令牌 `verify_session`（`user:<id>`），再退回会员会（`member:<id>`）。

### 遗留开放项

- **O-7（新）**：平台 bootstrap 后、**owner 未导入邀请函前**，控制台邀请成员会报
  `member <id> is revoked (seat state: unknown)` —— 语义正确（租户侧该座位尚不存在）但**提示不友好**，
  用户不知道"要先导入 owner 信"。建议：把该错误翻译成「请先导入 owner 邀请函认领座位」，或在
  邀请入口前置检查。**未修**（等 owner 裁决是否需要）。
- O-1 Pod 资源时长计量（v2）· O-2 动态定价 · O-3 成员排行 · O-4 日结批处理 · O-5 支付通道 · O-6 成员自助消费

## Related Artifacts

- `adr/ADR-0003-tcoin-two-level-ledger.md`（本 session 产出）
- `CONTEXT.md`（新增术语：TCoin / 两级账本 / 计价汇率 / 消耗口径）

- `design/012`（D14/D15 v1 不计费不计量 —— 本 grill 将修订该口径）
- `design/015`（SD-22 资源算 owner / 只记账不拦；SD-23 批准即拉 Pod）
- `design/016` + `adr/ADR-0002`（离开即释放 → 停止计量的挂载点）
- `requirements/002`（NFR-7 成本口径 v1=N/A）
