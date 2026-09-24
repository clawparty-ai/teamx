---
status: completed
protocol_version: 1
owner: caishu
created_at: 2026-09-24
updated_at: 2026-09-24（Shared Understanding 已确认，session 完成）
---

# 016 — 控制台「我的团队」菜单（grill 设计访谈 · 第 2 个一级菜单）

> grill-with-docs 协议的 Design Session Record。范围：云控制台 **「我的团队」** 菜单
> （含"我加入的团队"这一新面），并承接 015 已定稿的发现/加入机制。
> 不含：TCoin 经济模型、credit 评分模型（各自独立 grill）。

## Context

- 「全部团队」（design/015）已定稿并实现：公开名录 → 详情 → 申请加入 → owner 审批 →
  **自动建座位 + 邀请函 + 拉起 member Pod**。
- 现状「我的团队」= **我拥有的租户**（`tenant_owners`），页面上是 list→detail：
  初始化团队 / 发现设置 / 回收租户。
- **缺口（本次要设计）**：用户**加入别人的团队**后，该团队**不出现在「我的团队」**里 ——
  平台只在 `team_join_requests` 记了"已批准"，没有把它当作"我的团队"来展示与管理。
  用户因此看不到"我在哪些团队、我的角色是什么、我的工作区在哪、怎么离开"。
- 已核实事实：
  - **F1** `api_my_teams` 只返回 `tenant_owners` 的团队（含 used/quota），**不含**已加入的团队。
  - **F2** `team_join_requests` 有 `status=approved` + `member_id` + `letter` + `pod_state`，
    是平台侧唯一能识别"我加入了哪个团队"的凭据（席位细节在**租户 server**）。
  - **F3** 租户内成员/角色数据在**租户库**；平台侧只能经 service 车道取（白名单方法 `team.status`
    会带敏感字段，需租户侧裁剪；015 已为"公开详情"加了 `public.team_meta`）。
  - **F4** 现有 owner 侧动作：初始化（bootstrap）/ 发现设置 / 回收租户；邀请成员与
    provision 的路由已存在（`/api/tenants/{team}/invite|provision`，走 service 车道）。
  - **F5** 配额：`platform_quotas.tenant_quota`（默认 3，计"我拥有的团队数"）；
    015 的 SD-22 已定：**成员不设 v1 硬配额、只做 TCoin 记账**。
  - **F6** 离开/转让/成员管理在平台侧**均无实现**（`memberships` 由租户 server 管）。
  - **F7** 015 已定：`join_policy`（open_request/invite_only）与可见性正交；
    批准 = 建座位 + 发函 + 异步拉 pod。

## Settled Decisions

（owner 已明确）
- **SD-1** 「全部团队」与「我的团队」是**两个一级菜单**，逐个设计（015 已完成前者）。
- **SD-2** 控制台**只面向用户**；运维走 CLI（015 SD-6）。
- **SD-3** 云控制台一律 **list→detail 分栏**布局（对齐成员端 discussions/documents）。

## Design Tree

| DQ | 问题 | 依赖 | 状态 |
|---|---|---|---|
| DQ-0001 | 范围（拥有 + 加入，同页两组） | — | ✅ SD-4 |
| DQ-0002 | 两组各展示什么 | DQ-0001 | ✅ SD-5 |
| DQ-0003 | 「我拥有的」可做操作 | DQ-0001 | ✅ SD-6 |
| DQ-0004 | 「我加入的」可做操作 | DQ-0001 | ✅ SD-7 |
| DQ-0005 | 离开团队语义（双侧真释放） | DQ-0004 | ✅ SD-11 |
| DQ-0006 | 成员管理边界（只读 + 邀请） | DQ-0003 | ✅ SD-12 |
| DQ-0007 | 配额与用量展示（三卡 + 占位） | DQ-0001 | ✅ SD-8 |
| DQ-0008 | 转让所有权 | DQ-0003 | ✅ SD-13（v1 不做→O-1） |
| DQ-0009 | 入口关系 | DQ-0001 | ✅ SD-15 |
| DQ-0010 | 回收/删除不可逆提示 | DQ-0003 | ✅ SD-16 |
| DQ-0011 | 排序 / 搜索 / 分页 | DQ-0002 | ✅ SD-17（不搜索→O-2） |
| DQ-0012 | 工作区状态来源（k8s 为准 + 快照回落） | DQ-0002 | ✅ SD-14 |
| DQ-0013 | 二级菜单结构（子 Tab） | DQ-0001 | ✅ SD-10 |

**Frontier 空**（本 session 范围内全部 settled）。

## Current Frontier

（空 —— Round 2 后本 session Frontier 已清空）

### Round 1 决定（2026-09-24）

- **SD-4（DQ-0001 = b）** 范围 = **我拥有的 + 我加入的**，**同页两组**（"我创建的 / 我加入的"）。
  数据源不同：拥有的 ← `tenant_owners`；加入的 ← `team_join_requests(status=approved)`。
- **SD-5（DQ-0002）** **拥有的**：名称 / 可见性标记 / 成员数 / **工作区状态**（经 service 车道）；
  **加入的**：名称 / **我的角色** / 工作区状态 / 加入时间。角色需租户侧裁剪方法（无端点则如实留空）。
- **SD-6（DQ-0003）** 拥有的团队操作 v1 收敛为：**邀请成员**（service 车道）+ **成员列表（只读元数据）**
  + 初始化 / 发现设置 / 回收；**审批申请不放这里**（在「待我审批」，给出入口提示即可）。
- **SD-7（DQ-0004 = b + d）** 加入的团队操作：**取回邀请函**（平台存 letter，一键复制）+ **离开团队**；
  **重建工作区** 放 v2（须先能判断 pod 实际状态，避免重复建资源）。
- **SD-8（DQ-0007 = a）** 顶部三张汇总卡：**拥有 N/配额 M**、**加入 K**、**运行中工作区 X**；
  TCoin / credit 仅**占位**（未启用就明说，与 015 SD-21 一致）。
- **SD-9（owner 指示 · 本轮新增）** **「创建团队」与「Owner 邀请函」作为二级菜单**出现
  （不再作为列表页内的内联区块）→ 结构见 DQ-0013。

### Round 2 决定（2026-09-24）

- **SD-10（DQ-0013 = a）** 「我的团队」下**二级菜单（子 Tab）** = `我的团队` | `创建团队` | `Owner 邀请函`。
  `创建团队` 为**表单页**（名称 + 可见性 + 加入策略一步建好）；`Owner 邀请函` 为**取件页**
  （列出我拥有的团队 + 邀请函 + 一键复制）。
- **SD-11（DQ-0005 = b）** **离开团队 = 双侧真释放**：平台申请置 `left` **且** 经 service 车道
  把租户侧座位置 `left` + **回收 member Pod**（PVC 数据可选保留 7 天）。离开后可再次申请（无冷却）；
  复入时若旧 PVC 仍在则复用。
- **SD-12（DQ-0006 = b）** 成员管理 = **只读列表（元数据，无凭据/session）+ 邀请成员**；
  **移除成员 / 改角色 v1 不做**（属成员端/CLI 职责）。
- **SD-13（DQ-0008 = c）** **转让所有权 v1 不做**，留 CLI（牵涉配额归属、成员端权限、
  015 SD-22 的"资源算 owner"记账，产品语义待议）→ 开放项 O-1。
- **SD-14（DQ-0012 = c）** 「我的工作区」状态：**以实时 k8s 为准**（`teamx.io/managed` 标签 +
  我的团队 namespace），k8s 不可达时回落 `pod_state` 并**标注为快照**。
- **SD-15（DQ-0009 = c）** 入口关系：空态给**"创建团队 / 去全部团队发现"两个按钮**；
  加入的团队详情给"公开详情"跳转。
- **SD-16（DQ-0010 = b）** 回收/删除：**需输入团队名确认** + 列出**销毁清单**（server 数据 / CA /
  成员工作区）+ 如实声明"**成员会失联**"；不做冷静期。
- **SD-17（DQ-0011 = a + 排序）** 排序/搜索/分页：**不做搜索/分页**；**按时间倒序**
  （拥有的按创建时间、加入的按加入时间）→ 开放项 O-2（数量多了再加）。

## Remaining Branches

**owner 明确延后（非未决）**：
- **O-1 转让所有权**（SD-13）：v1 由运维 CLI 处理；控制台交互（对方接受？配额迁移？）待议。
- **O-2 「我的团队」搜索/分页**（SD-17）：团队数量增长后再加。
- **O-3 重建工作区**（SD-7）：须先能判定 pod 实际状态、避免重复建资源；v1 只做"取回邀请函 + 离开"。
- **O-4 「我的申请」页的 approved 行去重**：加入的团队同时出现在「我的团队/我加入的」与「我的申请」，
  两处语义不同（前者=当前参与，后者=申请历史），暂不合并。

**独立 grill**：TCoin 经济模型、credit 评分模型。
**未决基础设施**：O-5 租户端点（当前 operator 配 `tenants.endpoint` + loopback port-forward）。
- 独立 grill：**TCoin 经济模型**、**credit 评分模型**
- 未决基础设施：**O-5 租户端点**（当前靠 operator 配 `tenants.endpoint` + loopback port-forward）

## Fact Requests and Reports

（暂无）

## Related Artifacts

- `adr/ADR-0002-leaving-a-team-releases-both-sides.md`（本 session 产出）

- `design/015-teams-discovery-grill.md`（全部团队 + 加入机制，已定稿）
- `adr/ADR-0001-public-team-directory-data-source.md`
- `CONTEXT.md`（术语表）
- `design/014`（§5 控制台分层 / §6.5 service 车道）

## 实现状态（2026-09-24，真机验证）

「我的团队」按定稿实现并**真集群验证**。

| 能力 | 实现 | 验证 |
|---|---|---|
| 二级菜单 | 子 Tab：`我的团队` / `创建团队`（表单页）/ `Owner 邀请函`（取件页） | ✅ 活体 |
| 两组列表 | `provision::my_teams`：`owned`（tenant_owners）+ `joined`（**按团队去重**，取最新一条；离开/回收后不再出现） | ✅ 真机（去重修复 + 孤儿失效） |
| 顶部三卡 | 我创建的 N/配额 · 我加入的 K · 运行中工作区 X | ✅ 活体 |
| 工作区状态 | `kubectl get pods -A` 按 `teamx.io/managed` 标签**实时**取；不可达回落快照并标注 `workspace_live=false`；owner 用 `tenants.owner_member_id` | ✅ 真机（applicant 显示 `Pending/live=true`；owner 无 pod → 如实 `none`） |
| 成员只读名册 | 租户侧 `team_roster`（仅 名/角色/状态/is_lead）+ service 车道白名单 `team.roster` | ✅ 单测/接口；无端点时**明确失败** |
| 邀请成员 | `POST /api/my/teams/{t}/invite`（owner 鉴权 → `team.invite`） | ✅ 无端点时明确失败（E2E F5） |
| 创建表单 | 一步带 名称/可见性/加入策略/简介 | ✅ 真机（public+open_request 生效） |
| Owner 邀请函 | `tenants.owner_letter` 持久化（bootstrap 写入）+ 取件页 | ✅ 真机（新建团队 `available=true`，老团队无信如实 `false`） |
| 离开团队 | `leave_team`：租户侧 `team.release_seat`（窄接口）→ 删 member Pod → 平台置 `left`；租户不可达则整体失败（ADR-0002） | ✅ E2E F6（非成员被拒） |
| 回收确认 | 需输入团队名 + 销毁清单 + "成员会失联" | ✅ 活体 |
| 回收失效 | destroy 时把该团队 pending/approved 申请置 `left`（成员失联） | ✅ 真机（清理孤儿后 joined 回到 1） |

### 真机踩到并修掉的 3 个问题

1. **「我加入的」未去重**（真 bug）：反复申请/离开会在同一团队留多条 approved，
   实测同一团队出现 7 次 → 改为"按团队取最新一条（含 `left`）"，仅最新为 approved 才计入（`a34d149`）。
2. **owner 工作区状态取不到**：owner 无 join 请求（座位来自 bootstrap）→ 新增
   `tenants.owner_member_id`（bootstrap 写入）（`a34d149`）。
3. **回收后成员的「我加入的」仍显示该团队** → destroy 时失效其 pending/approved 申请
   （`0525de9`，destroy 清单加 `invalidate-join-requests`）。

另：编辑 UI 时一次**非贪婪正则误删**了 `INDEX_HTML` 结束标记与测试模块（`git checkout` 恢复后改用
精确切片 + 锚点替换，并加结构自检：div 配平 / 真实 id 去重 / `"#;` 与 tests 存在）。

**未做（延后项，与 record 一致）**：转让所有权（O-1）、搜索/分页（O-2）、重建工作区（O-3）、
申请历史去重（O-4）；租户端点仍靠 operator 配置（O-5）。
