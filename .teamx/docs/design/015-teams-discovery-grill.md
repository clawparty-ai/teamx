---
status: completed
protocol_version: 1
owner: caishu
created_at: 2026-09-24
updated_at: 2026-09-24（Shared Understanding 已确认，session 完成）
---

# 015 — 控制台团队发现与加入（grill 设计访谈）

> 本文件是 Teamx **grill-with-docs** 协议的 Design Session Record（人工主导；作者方不实现代码）。
> 范围：云控制台的两个菜单 —— ①**全部团队**（本轮）②**我的团队**（下一轮）；
> 并纳入两个待定属性：**TCoin**（租户代币）与 **credit**（租户信用）。

## Context

- 云控制台（platform console，user-facing 部分）当前只有「我的团队」自助区（建团队/配额/初始化）。
  用户要求参考 **GitHub 后台** 的布局与用法：增加「全部团队」菜单，可见所有 **public** 团队，
  可查看详情，可对「可公开加入」的团队发起**加入申请**。
- 现状事实（已核实，非 owner 提问项）：
  - **F1** `teams` 表**没有可见性字段**（列为 `id, name, owner_member_id, goal_id, state, invite_token, created_at, updated_at`）→ public/private 属新增。
  - **F2** **不存在"申请加入"机制**。现有加入路径 = owner 发邀请函（`team invite` → letter）→ 成员 `team import` 认领 → owner `approve/deny`；即**邀请制**，非申请制。
  - **F3** **不存在 TCoin / credit / 余额 / 账目** 任何概念（代码零命中）。
  - **F4** 团队状态机：`forming / active / completed / archived / destroyed`。
  - **F5** 多租户模型（design/014）：**一个租户 = 一个 team + 一个 namespace + 独立 CA**；平台库持有 `teams` 行（空壳）+ `tenants` 行 + `tenant_owners` 归属 + `platform_users` 用户。
  - **F6** 控制台鉴权：密码登录（平台用户）→ 会话；运维面（跨租户管理/平台 token/审计）已决定移出控制台、由 CLI 承担。
  - **F7** 元数据边界（design/012 D10）：平台管理员默认**仅见元数据**、fail-closed 封闭枚举、授权式 break-glass。
- 关联既有设计：`design/012`（身份安全 D8/D10）、`design/014`（每租户 server + 控制台分层 §5）、
  `docs/04-design-team-invite.*`（邀请制）、`docs/goal-v1.*`（join/approve 语义）。

## Settled Decisions

（owner 在 2026-09-24 会话中已明确的）

- **SD-1** 团队在**创建时可选 public / private**（类比 GitHub 项目的可见性）。
- **SD-2** 控制台新增 **「全部团队」** 菜单，列出所有 **public** 团队。
- **SD-3** 点击 public 团队 → **可查看团队详情**。
- **SD-4** 若该团队**可公开加入**，点 **「申请加入」** → 向 **team owner** 发起加入申请。
- **SD-5** 租户新增两个属性：**TCoin**（teamx 代币，租户在 teamx 内的货币；后续会增加"用户使用代币"的功能）与 **credit**（租户信用，如"分包工作如期支付 member TCoin"等行为记录）。
- **SD-6** 控制台**只面向用户**；运维功能走 CLI（运维面将来独立成程序，现在不做）。
- **SD-7** 逐个菜单推进：**先「全部团队」**，后「我的团队」。

### Round 1 决定（2026-09-24）

- **SD-8（DQ-0001）** 可见性 = `teams.visibility`（封闭枚举 `public|private`），**默认 private**，**仅 owner 可改**；**存量团队一律回填 private**；**公开名录只列 `forming/active`**。
- **SD-9（DQ-0002 + 细化）** **列表数据源 = k8s 元数据**（`deploy/pod/namespace`，即租户命名空间与 server Deployment），**不调用任何租户 teamx server**；
  **详情数据源 = 调租户 teamx server 接口**（经现有 service 车道，mTLS）。
- **SD-10（DQ-0003）** **列表只显示团队名**（保持最简）；更丰富信息放详情。
- **SD-11（DQ-0004 = i）** 申请加入：平台库落申请 → **归属平台用户（owner）在控制台审批** → 批准时经 service 车道在租户 server 建 seat + 邀请函 → **控制台展示邀请函，申请人自行 `teamx team import`**（**不**自动拉 pod）。同一 (user, team) 仅 1 个 pending；可撤回；拒绝冷却 7 天。
- **SD-12（DQ-0005）** 名录**必须登录**（未登录 → 登录页）；v1 不做匿名只读。
- **SD-13（DQ-0006）** **本轮定下展示面**；入口放在**右上角用户名处的新增菜单**（类比 GitHub 头像菜单）。

### Round 2 决定（2026-09-24）

- **SD-14（DQ-0012 = c）** 可见性**两处都存**：平台库 `teams.visibility` **且** k8s 侧打标（namespace label `teamx.io/visibility`）。→ 需要同步纪律（见 DQ-0018）。
- **SD-15（DQ-0013 = a）** 详情走**精简白名单**且**服务端裁剪**：团队名 / 公开简介 / goal 标题+状态 / 成员数 / 创建时间；**不含**成员名单、`invite_token`、证书与 session 信息。
- **SD-16（DQ-0015）** 可见性与"可加入"**正交**：`visibility=public` 只代表"可被发现"；新增 `join_policy ∈ {open_request, invite_only}`（owner 选，**默认 `invite_only`**）；**审批始终人工**（owner），v1 无自动批准。
- **SD-17（DQ-0005）** 批准后：默认 role = **`contributor`**（审批时可改）；**自动拉起 cloud member(pod)**；**不占申请人配额**，**占邀请人（团队 owner）的配额/资源**——因为消耗的是 owner 的资源；**该计量以后由 TCoin 承担**。→ 需要 owner 侧成员配额模型（DQ-0016）。
- **SD-18（DQ-0010 = a）** 申请制与邀请制**双轨并存**（owner 主动邀请照旧，申请是被动收）。
- **SD-19（DQ-0007 = c）** 公开团队页按 **GitHub 公开项目页**规格：搜索 + 排序 + 筛选 + 分页（细化见 DQ-0019）。
- **SD-20（DQ-0009 = a）** **private 团队完全不可见**（非成员直链 → 403）。
- **SD-21（DQ-0014）** 右上角用户名菜单 = `我的团队` / `我的申请` / `TCoin（占位）` / `credit（占位）` / `设置` / `退出`；「全部团队」「我的团队」为**主导航 Tab**，不塞进用户名菜单。

### Round 3 决定（2026-09-24）

- **SD-22（DQ-0016 = c）** **v1 不设成员硬配额**：批准加入**只做 TCoin 记账（记但不拦）**。理由：TCoin 计量即将上线，此时加一套会被替代的硬配额是浪费；**如实记录**"v1 对 owner 资源占用不设平台级上限"（集群侧仍有 namespace `ResourceQuota` 兜底）。
- **SD-23（DQ-0017 = a）** 自动拉 member pod 走**异步 job**（复用 `node provision` + `run-jobs`）：批准 → **立即建 seat + 发函**（用户马上是成员）→ 后台 provision（状态 `pending/running/failed` 对用户可见）；**pod 失败不回滚 seat**，可重试。
- **SD-24（DQ-0018 = a）** 可见性**平台库为唯一事实源**，k8s label 为**投影**（写库成功 → 异步打标 → 失败进补偿 → 启动低频对账修复）；不一致**以平台库为准并自动修回**；**如实声明为最终一致（秒级）**。
- **SD-25（DQ-0019 = a）** 公开团队页 v1 = 搜索（名称+简介）/ 排序（**创建时间↓、成员数↓**）/ 筛选（可加入）/ 分页 20。**排序键仅用平台库可得字段**（因 SD-9 列表不调租户 server）。
- **SD-26（DQ-0020）** 「我的申请」页：申请列表（团队名 / 时间 / 状态 `pending|approved|rejected|withdrawn`）+ pending 可撤回 + **approved 行展示邀请函 + 一键复制**（SD-11 的交付落点）+ 被拒显示冷却到期时间。

## Design Tree

| DQ | 问题 | 依赖 | 状态 |
|---|---|---|---|
| DQ-0001 | 可见性模型（public/private，默认 private，owner 可改） | — | ✅ SD-8 |
| DQ-0002 | 名录数据源（列表=平台库/k8s；详情=租户 server） | — | ✅ SD-9 |
| DQ-0003 | 名录/详情字段集（列表仅名称；详情白名单） | DQ-0001 | ✅ SD-10 / SD-15 |
| DQ-0004 | 「申请加入」流程（平台库申请 + owner 审批 + 发函） | DQ-0001 | ✅ SD-11 |
| DQ-0005 | 加入后身份与资源（contributor + 自动拉 pod + 记账） | DQ-0004 | ✅ SD-17 / SD-22 / SD-23 |
| DQ-0006 | 可见范围与未登录态（须登录） | DQ-0001 | ✅ SD-12 / SD-13 |
| DQ-0007 | 搜索 / 排序 / 分页（GitHub 公开页规格） | DQ-0003 | ✅ SD-19 / SD-25 |
| DQ-0008 | TCoin/credit 展示面（用户名菜单占位） | — | ✅ SD-13 / SD-21（模型另开 grill） |
| DQ-0009 | private 可达性（完全不可见，403） | DQ-0001 | ✅ SD-20 |
| DQ-0010 | 与邀请制并存（双轨） | DQ-0004 | ✅ SD-18 |
| DQ-0012 | 可见性存哪（双写，平台库为准投影） | DQ-0002 | ✅ SD-14 / SD-24 |
| DQ-0013 | 详情字段裁剪（服务端白名单） | DQ-0002/0003 | ✅ SD-15 |
| DQ-0014 | 用户名菜单内容（6 项） | DQ-0006 | ✅ SD-21 |
| DQ-0015 | 可加入开关与审批（正交 + 人工审批） | DQ-0004 | ✅ SD-16 |
| DQ-0016 | owner 侧成员配额 | DQ-0005 | ✅ SD-22（v1 不设硬配额） |
| DQ-0017 | 自动拉 pod 时机与失败处理 | DQ-0016 | ✅ SD-23 |
| DQ-0018 | 可见性双写同步纪律 | DQ-0012 | ✅ SD-24 |
| DQ-0019 | 公开页维度细化 | DQ-0007 | ✅ SD-25 |
| DQ-0020 | 「我的申请」页内容 | DQ-0004 | ✅ SD-26 |

**本 session 范围 = 「全部团队」菜单**（SD-7 决定逐菜单推进）。以下为**owner 明确处置的延后项**（非本 session 未决）：
`我的团队` 菜单（DQ-0011）、TCoin 经济模型、credit 评分模型、topics 标签体系、公开页"最近活跃"排序 —— 见 Remaining Branches。

## Current Frontier

（空 —— 本 session 范围内的 Frontier 已全部 settled）

## Remaining Branches

以下均为 **owner 明确延后**（非未决），各自独立开 session：

| 项 | 处置 | 说明 |
|---|---|---|
| **「我的团队」菜单**（DQ-0011） | 延后 → **下一个 grill**（owner：一个菜单一个菜单来） | 现有自助区（建团队/配额/初始化）会被重新设计 |
| **TCoin 经济模型** | 延后 → **独立 grill** | 核心待决：账本落平台库还是租户 server？谁有权写账？铸币/分发/消耗规则；SD-22 已把"TCoin 记账"作为 v1 方向 |
| **credit 评分模型** | 延后 → **独立 grill** | 记什么行为（如如期支付 TCoin）、谁能写、可否申诉 |
| **topics 标签体系** | 延后（SD-25） | 需 owner 维护界面，属独立小特性 |
| **公开页"最近活跃"排序** | 延后（SD-25） | 需平台侧活跃快照；当前列表不调租户 server（SD-9） |

## Fact Requests and Reports

（暂无）

## Related Artifacts

- `docs/adr/ADR-0001-public-team-directory-data-source.md`（本 session 产出）
- `CONTEXT.md`（本 session 新增术语：可见性 / join_policy / 全部团队 / TCoin / credit）

- `design/012`（D8/D10 身份与元数据边界）
- `design/014`（§5 控制台分层；§9.6 实现状态）
- `docs/04-design-team-invite.cn.md`（邀请制现状）
- `docs/goal-v1.cn.md`（join/approve 状态机）
