---
status: active
protocol_version: 1
owner: caishu
created_at: 2026-09-25
updated_at: 2026-09-25
---

# 021 — 平台用户 / 团队成员 / 云主机 映射与挂接计划

> 配套：`design/020-yongyan-dev-migration-plan.md`（迁移运行手册）
> 目标平台：https://teamx.beer （生产）· 源端：HK `95.40.221.159`（**不销毁，见 020 硬约束**）
> 本文**只列计划**，不含执行；执行前的确认项见 §G。

## A. 需要在 teamx cloud 创建的平台用户

| 平台用户 | 对应团队内身份 | 角色 | 用途 | 现状 |
|---|---|---|---|---|
| **`caishu`** | `powernano`（owner/lead） | owner | 承载迁移租户的**归属人**（配额、账单、可见性） | ✅ 已建（`4087ae63…`）；**配额仍为默认 3，待调整为 20** |
| **`zhongm`** | `zhongm` / `zhongm-worker` | searcher | 成员，控制台看自己的团队 + 领自己的信 | ✅ 已建（`1084c011…`） |
| **`caicai`** | `caicai` | kb-engineer | 同上 | ✅ 已建（`a5f4ce9d…`） |
| **`zhaoz`** | `zhaoz` | extractor | 同上 | ✅ 已建（`1a2f517c…`） |
| （保留）`demo` | — | — | 冒烟测试账号，**不承载生产团队** | ✅ 已建 |

> 说明：`caishu` 与团队内的 `powernano` 是**同一个人**（owner/lead，本机 Mac）。平台用户 = 人；团队成员 = 该人在某团队里的席位。二者通过 `members.user_id` 关联。
> 是否还需要为**其他人**建平台用户？当前 yongyan-dev / powernano-v2 的活跃人就这 4 位（见 §B/§C）。

## B. 成员现在使用的云主机（cloud member）

来源：`~/Desktop/nano激光/deliver/yongyan-dev/docs/infra-inventory.md` §2/§3（**待逐台复核**，见 §G-3）

| 成员 | 云主机（AWS 实例名） | 区域 | 公网 IP | 机型 | 状态 | 本次迁移涉及？ |
|---|---|---|---|---|---|---|
| **zhongm** | `powernano-zhongm` | ap-northeast-1（东京） | 35.77.5.99 | t4g.small | running | ✅ 是（成员客户端）|
| **caicai** | `powernano-caicai` | us-west-2（俄勒冈） | 184.34.85.90 | t4g.small | running | ✅ 是 |
| **zhaoz** | `powernano-zhaoz` | ap-northeast-2（首尔） | 3.37.16.165 | t4g.small | running | ✅ 是 |
| **powernano / caishu（owner）** | ❌ 无云主机（本机 Mac） | —（中国，39.144.60.135） | — | — | — | ✅ 是（本机作客户端）|
| **团队 server 本身** | HK `ip-172-31-18-83` | ap-east-1（香港） | 95.40.221.159 | — | running | ✅ **是被迁移的源** |
| starnet-member-jp | `starnet-member-jp` | ap-northeast-1 | 18.177.87.118 | — | running | ❌ 否（starnet/企业侧）|
| starnet-member-us | `starnet-member-us` | us-west-2 | 44.234.64.4 | — | running | ❌ 否 |

> **owner 已确认（2026-09-25）**：`16.145.86.98`（us-west-2）与 `clawparty-seoul`（stopped）**与本项目无关** → 从本计划剔除，不再跟踪。

> 另有 **owner 本机 Mac 上的运行时 server**（`127.0.0.1:5781`，团队 `yongyan-backend`）—— 按 020 §9 明确**不在本次范围**。

**复核结果（2026-09-25，`aws ec2 describe-instances` + SSH 实测，已按 owner 要求执行）**：

| 实例 | IP | 区域 | 机型 | 状态 | 结论 |
|---|---|---|---|---|---|
| `powernano-zhongm` | 35.77.5.99 | ap-northeast-1 | t4g.small | running | ✅ 与文档一致 |
| `powernano-caicai` | 184.34.85.90 | us-west-2 | t4g.small | running | ✅ 与文档一致 |
| `powernano-zhaoz` | 3.37.16.165 | ap-northeast-2 | t4g.small | running | ✅ 与文档一致 |
| `powernano`（HK 源）| 95.40.221.159 | ap-east-1 | **t4g.xlarge** | running | ✅ 健康：`teamx.service` active、443 在听、DB 99M、**teams 9（active 2）** |

> 文档未记的补充：HK 源机型为 **t4g.xlarge**（ARM/Graviton，4 vCPU · 16 GiB）；源侧二进制为 aarch64（迁移只搬数据，与架构无关）。

## C. 映射总表（平台用户 ↔ 团队 ↔ 席位 ↔ 云主机 ↔ 信）

**yongyan-dev**（`f2adafd3…`，active）

| 团队席位 | 角色 | 状态 | 源库 `user_id`（旧平台） | → 新 cloud 平台用户 | 云主机 |
|---|---|---|---|---|---|
| `powernano` | owner | active | （空） | **`caishu`** | 本机 Mac |
| `zhongm` | searcher | idle | `4085ba53…` | **`zhongm`** | powernano-zhongm |
| `caicai` | kb-engineer | idle | `d6c8e93f…` | **`caicai`** | powernano-caicai |
| `zhaoz` | extractor | waiting | `0b547c51…` | **`zhaoz`** | powernano-zhaoz |

**powernano-v2**（`1da9a7f0…`，active）

| 团队席位 | 角色 | 状态 | 源库 `user_id`（旧平台） | → 新 cloud 平台用户 | 云主机 |
|---|---|---|---|---|---|
| `powernano` | owner | active | （空） | **`caishu`** | 本机 Mac |
| `caishu` | research-lead | active | （空） | **`caishu`** | 本机 Mac |
| `caicai` | kb-engineer | active | `d6c8e93f…` | **`caicai`** | powernano-caicai |
| `zhongm-worker` | searcher | active | `4085ba53…` | **`zhongm`** | powernano-zhongm |
| `zhaoz` | extractor | active | `0b547c51…` | **`zhaoz`** | powernano-zhaoz |

> 同一人在两个 team 里有两个席位（如 `caicai` 在 yongyan-dev 与 powernano-v2 各一个）→ **挂接要按 (user, team, seat) 三元组做**，不能只按人。
> 源库 `user_id` 是**上一代平台的 id**（与新 cloud 的 `a5f4ce9d/1084c011/1a2f517c` 不同）→ **必须重新指向**，否则控制台看不到团队、`roster` 也对不上。

## D. 挂接步骤（把成员挂到自己账号下并加入团队）

前提：020 的 P1–P3 完成（租户已建、**CA 为源 CA**、DB/repos 已灌入、租户端点 `t-<short>.teamx.beer` 可用），且租户归属 = `caishu`。

1. **平台用户**：已建（§A）；`caishu` 配额待调为 20。
2. **租户归属**：把迁移租户写入 `tenant_owners(team_id → caishu)`（承载团队归属 `caishu`）。
3. **席位挂接（逐人逐团队）**：对每个席位写两处——
   - 租户库：`UPDATE members SET user_id = '<新平台 user_id>' WHERE id = '<seat_id>'`
   - 平台库：插入一条 `team_join_requests(team_id, user_id, status='approved', member_id=<seat_id>, letter=…)`
     → 这样该成员登录控制台能在**「我加入的」**看到该团队（`joined_teams` 按 approved + 最新去重）
4. **重发/更新邀请函（同一 CA、新端点）**：为每个席位用**源 CA** 重签一张信（URL 指向 `t-<short>.teamx.beer`）→ 成员 `teamx team import` → 客户端切到云端。
   - 因为 CA 未变，**信任链无需重建**；只是 URL 变了
   - 这些信可以**由迁移后的租户 server 自己签发**（`teamx team invite` / `reissue-members`），不需要平台代签
5. **逐成员切换与验证**（顺序：`zhaoz` → `zhongm` → `caicai` → **`caishu` 最后**）：
   - 成员侧：`teamx status` 指向新端点、能收发事件、任务/讨论正常
   - 平台侧：该成员登录控制台能看到团队（我的团队/我加入的）、`team_roster` 显示其角色
6. **owner（caishu=powernano）**：最后切换；同时确认平台侧「我创建的」显示该团队（`tenant_owners`）+ 团队内 owner 身份不变。

## E. 遗留 `user_id` 的处理决策（**owner 已确认：E1 重新指向**）

| 选项 | 说明 |
|---|---|
| **E1（✅ owner 已选）重新指向** | 把 `members.user_id` 改为新 cloud 平台用户 id；旧 id 只留历史。语义清晰：新平台是新身份体系 |
| E2 保留旧 id | 不动源 `user_id`，仅靠平台侧 join_requests 表达成员关系 → **控制台/roster 会不一致**（不推荐）|
| E3 兼容双写 | 额外维护旧 id 映射表 → 复杂，收益低 |

> 若选 E1：迁移是**改租户库的一次性 UPDATE**，写在迁移快照的副本上（源端不动）。

## F. 明确不做（边界）

- 不迁移/不建 **starnet** 相关成员与实例（`starnet-member-jp/us`、`starnet` 团队已完成）
- 不处理未标注实例 `16.145.86.98`、`clawparty-seoul`（除非 owner 指明归属）
- 不动 3 台成员云主机本身（它们只是客户端；切换 = 改端点 + 换信）
- 不动 owner 本机 Mac 的运行时 server（`yongyan-backend`）
- 不动企业版 server（`120.27.222.33:5783`）

## G. owner 已确认（2026-09-25）

1. ✅ **成员就这 4 位云端成员 + owner 本机 1 位**（无遗漏）
2. ✅ **密码：生成 12 位随机密码**（由 owner 分发；建议成员首登后自行修改）
3. ✅ **三位成员同样下发临时密码**
4. ✅ **遗留 `user_id` 按 E1 重新指向**
5. ✅ `16.145.86.98` / `clawparty-seoul` **与本项目无关**（已剔除）
6. ✅ **迁移前逐台复核**云主机与源端（§B 已完成）

### 仍待 owner 决策（阻塞正式迁移，来自 design/020）

- **D1** 租户端点：`t-<short>.teamx.beer` + nginx SNI 直通（建议）／NodePort 直连
- **D2** 迁移范围：整库（建议）／仅两个 active 团队
- **D3** 切换节奏：逐成员（建议，caishu 最后）／一次性
- **D4** 看板与隧道：后置（建议）／随迁
- **D5** 源端保留：≥30 天（建议）
- **A/B**：先整库迁（A，建议）／先做"按团队拆分租户"工具再迁（B）

## H. 已执行动作（诚实记录，便于审计）

| 时间 | 动作 | 结果 |
|---|---|---|
| 2026-09-25 | 在生产平台库创建 `caishu` / `zhongm` / `zhaoz` / `caicai` | ✅ 4 个用户已建（`demo` 为早先冒烟账号）|
| 2026-09-25 | 尝试把 `caishu` 配额设为 20（第一次）| ❌ 失败（shell 保留变量 `UID`）→ 已用 Python 重做 |
| 2026-09-25 | **重设 4 个用户密码为 12 位随机**（`caishu`/`zhongm`/`zhaoz`/`caicai`）| ✅ 4/4 成功；**已逐个用生产入口验证可登录** |
| 2026-09-25 | **`caishu` 配额 → 20** | ✅ 成功（`tenant_quota: 20`）|
| 2026-09-25 | **复核 3 台成员云主机 + HK 源端** | ✅ 与 infra-inventory 一致（HK 补充机型 t4g.xlarge，源端健康）|

> **凭据交付**：12 位随机密码**仅通过会话交付 owner**，**不写入本仓/台账**（本文件只记"已生成并交付"）。
> 建议：owner 收到后立即分发，并要求成员首登后改密；owner 自己的账号也建议改密。
| — | 源端（HK 95.40.221.159）| **未做任何改动**（仅只读查询）|
| — | 租户/迁移/成员挂接 | **未开始**（等 §G 确认）|
