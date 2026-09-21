# 002 — Teamx Cloud 需求分析（草稿 v2 · 已按首轮评审修订）

> 状态：**draft**（待评审） · 所有者：caishu · 日期：2026-09-20
> 输入：`design/009`（第一轮 harvest）、`design/010`（4 条架构原语裁决）、`design/011`（交付与里程碑裁决）
> 关联：`design/006`（现有 EC2 provision 链）、`design/007`（讨论模式）、#8（GitHub App，租户键共用）
> 说明：本文只写**需求**（做什么/边界/验收），不写实现方案（留给 `design/013+`）。
> **v2 修订**：已吸收首轮评审（caicai `task-74` 4 必改 / xiaods-mac `task-76` 6 必改 / weiye-mini `task-78` 4 必改）——
> 见文末「评审修订记录」。

---

## 1. 背景与目标

**Teamx Cloud** = 以 **k8e 集群**为底座的云端团队平台：用户用 **GitHub 账号**注册登录，创建自己的
team/project，邀请成员，并在创建成员时**一并 provision 云端 member**（容器化）。
与现状（design/006：EC2 主机 + 每实例一个 OS 用户 + systemd 常驻）**并存**，云上路径只支持 k8e。

**v1 目标**：跑通「登录 → 建 project → 邀请 → provision 云端 member → 成员可用（终端/文件/任务）→ 回收」全链，
且隔离/持久化/可观测达到设计声明的水准。

## 2. 范围

### 2.1 v1 做（In scope）

| 域 | 内容 |
|---|---|
| 入口 | GitHub 账号登录（身份面与 #8 共用租户键：`installation_id ↔ team_id ↔ namespace` 1:1:1） |
| 租户 | 创建 team/project；每租户一个 teamx server（容器，SQLite + PVC，独立 CA） |
| 成员 | 邀请成员；创建成员时异步 provision 云端 member（k8e Pod） |
| 运行面 | member = 一个 Pod（teamx + opencode + 工具链），workspace = PVC（固定挂载 `/workspace`） |
| 数据面 | ttyd / fs / forward 三类访问 1:1 保留；隧道走 expose 边车 + 自愈与可观测 |
| 存量 | 与 EC2 member 并存（双 provider），`nodes`/job/状态机/回收清单复用 |
| 回收 | 删租户/删成员 = 停服务 + 删 Pod/namespace + 清 PVC/Secret + 吊销（含 CA 级） |

### 2.2 v1 不做（Out of scope）

计费与配额（A6）· 每租户独立集群（A1）· 平台代开主机（A5，BYO）· Postgres（A3，留迁移预案）·
强隔离档（gVisor/独立节点池，D17 待定）· im-gateway 节点侧（无 CLI 命令）· 多集群联邦 · 冷启动预热池（v1 只做埋点与异步化）

## 3. 角色

| 角色 | 说明 |
|---|---|
| 平台管理员 | 平台运维；**默认无 exec 权限**，需 break-glass（C8）；审计独立于租户台账 |
| 租户 owner | 团队负责人（复用现有 owner/co-lead 状态机）；建 project、邀请、provision、回收 |
| 租户成员 | 团队成员；可被 provision 成云端 member；可建任务、参与讨论（含相互唤醒） |
| 云端 member | 容器化 agent（一人可多设备/多座位）；上报 workspace 与在线状态 |

## 4. 关键场景（端到端）

1. **注册登录**：GitHub OAuth → 建立/关联租户 → 落地 `installation_id ↔ team_id ↔ namespace` 三元组。
2. **建 project**：租户内创建 team/project（= 一个 namespace + 一个租户 server 实例 + 一个租户 CA）。
3. **邀请成员**：邀请 → 成员 import 邀请函 → approve（复用 006 语义与状态机）。
4. **provision 云端 member**：创建成员时**异步** provision Pod（member 先秒级可达、可派任务；UI 显示"准备中"）。
5. **日常使用**：成员通过 ttyd/文件/forward 访问自己的 workspace；任务/讨论/台账照常。
6. **回收**：删成员/删租户 → 可验证清单（停服务 → 删 Pod/namespace → 清 PVC/Secret → 吊销证书/CA）。

## 5. 功能需求（FR）

| # | 需求 | 来源 |
|---|---|---|
| FR-1 | 支持 GitHub 账号登录并建立租户；三元组 `installation_id ↔ team_id ↔ namespace` 严格 1:1:1 | D1/P1、C11 |
| FR-2 | 每租户一个 teamx server 实例（容器，SQLite + PVC，独立 CA）—— **归属第二步里程碑**；第一步 server 沿用现有形态（EC2/systemd），member Pod 直连它 | A2、A3、D2、D13 |
| FR-3 | 租户内可创建 project/team；复用现有 team/成员/角色状态机 | A2 |
| FR-4 | 邀请与审批流程复用 006（邀请函 → import → approve → 服务启动） | C4、C5 |
| FR-5 | 创建成员时**异步** provision 云端 member；member 先可用、provision 后台进行；UI 显示"准备中"，三段进度可见（调度+拉镜像 / workspace 恢复 / agent 热身）。**量化**：member 记录创建 ≤ **2s**（座位可派任务）；Pod `Running` ≤ **180s**（AI0 单节点、镜像预热后）；三段各自埋点保留 **30d** | C10、D16、C9 |
| FR-6 | member 运行于 k8e Pod（一 team-member 一 Pod），镜像含 teamx + opencode + 工具链 | D1、C5 |
| FR-7 | member workspace 持久化（PVC per member），容器内固定挂载 `/workspace` | C3、D3 |
| FR-8 | 证书/邀请函经 Secret 注入 → 容器内 import；不进镜像、不进 cloud-init | C4 |
| FR-9 | 成员数据面 1:1 保留：ttyd / fs / forward 可访问，且**断线可自愈**（退避重连 + 探针 + 断线计数） | D5、C6 |
| FR-10 | 在线状态与探针接 **server 侧 presence/online**（不依赖 K8s readiness）；边车失联 → 主容器上报 degraded | D5/P4 |
| FR-11 | 支持 scale-to-zero（idle 删 Pod 留 PVC；有任务再拉起）—— **后置（第二步）**；与 FR-5 的交互定义为：Pod 被回收后再次派任务 → 走同一条异步 provision 路径（复用 FR-5 的进度与埋点），不阻塞派任务方 | C2、C10 |
| FR-12 | 与 EC2 provider 并存：同一 `nodes`/job/幂等键/状态机/回收清单抽象复用 | C1、C16 |
| FR-13 | 回收：可验证清单（停服务 → 删 Pod/namespace → 清 PVC/Secret → 吊销证书/CA） | C14、D2 |
| FR-14 | `workspace` 上报字段为**唯一事实源**；File 菜单/forward 目标/快照策略均消费它 | D3/P3 |
| FR-15 | 冷启动**分段埋点**：①调度+拉镜像 ②workspace 恢复 ③agent 热身 | C9 |
| FR-16 | 平台自身 bootstrap 复用 006 的一次性 token（不另造引导协议） | C15 |
| FR-17 | 升级/回滚/版本 pin 能力（含租户 server 的滚动升级） | C14 |
| FR-18 | **里程碑两步走**：第一步只容器化 member（`provider=k8e`，server 不动）；第二步每租户一 server + 独立 CA + namespace | D13 |
| FR-19 | **BYO 主机接入走一次性 join token**（key 留用户侧；server 不持 BYO 主机私钥；命名含成员短 id）；token **绑定 `user_id` + 预期 `member_id`**（一 token 一设备） | D6、D8 |
| FR-20 | **迁移工具（可选，用户显式触发）**：EC2 workspace → PVC，带一致性验收（文件清单 + 抽查哈希 + 隧道同名重注册），失败即回滚；**迁移前保留源（不删 EC2 workspace），回滚有源可退** | D11 |

## 6. 非功能需求（NFR）

| # | 需求 | 判据 |
|---|---|---|
| NFR-1 隔离 | 租户间 namespace + NetworkPolicy + PSA(restricted) + seccomp + 只读根 + ResourceQuota；**默认拒绝出网 + 显式 allowlist**（必须放行：租户 server、`opencode.ai`、镜像 registry、git 源）；**显式封 IMDS `169.254.169.254`** | 跨租户负例：A 租户 member **解析/访问不到** B 租户的 serve/namespace；白名单外目标不可达（含 IMDS） |
| NFR-2 身份边界 | 每租户独立 CA；吊销租户 = 废 CA；平台 console 不持跨租户信任 | 吊销后旧证书 RPC 被拒 |
| NFR-3 可用性 | 爆炸半径**按两类 Pod 分别声明**：① **member Pod**（业务面）死 = 一个 team-member 受影响；② **平台 Pod**（租户 server / console 等）死 = 该租户整体受影响（第二步引入，需单独评估）；节点池分层 / ≥2 worker + 滚动窗口；**拔节点演练**：节点死亡后工作区可恢复 | 拔节点演练通过；两类 Pod 的 RTO 分别有数 |
| NFR-4 持久化 | member workspace 网络附着存储或显式快照策略；**CA 私钥进灾备清单** | 灾难恢复后证书仍有效 |
| NFR-5 可观测 | 重连日志 / 断线计数 / 存活探针接入 presence；在线状态**不撒谎** | 隧道断连后 ≤ **90s**（= 空闲超时）状态变为异常；断线计数与重连日志可按成员查询 |
| NFR-6 冷启动 | 分段耗时可见；provision 不阻塞交互路径 | 三段埋点可查询、**保留 30d**；Pod Running P50 ≤ 180s / P95 ≤ 300s（AIO、镜像预热后） |
| NFR-7 成本 | **N/A（v1）**：不做计费/计量/配额（owner 裁决 D14/D15 —— k8s 宿主机数量由 owner 控制，容量风险在资源层已限制）。**触发条件备注**：出现多租户共享同一集群且非 owner 控制容量时，本条重开 | — |
| NFR-8 安全声明 | 文档**如实声明**默认档隔离强度与适用前提（同信任域内团队），不夸大 | 设计文档含声明段落 |
| NFR-9 审计 | 平台侧审计独立于租户可控台账；break-glass 全程留痕 | 审计不可被租户删改 |
| NFR-10 存储快照 | member workspace 走网络附着存储或**显式快照策略**；频率/保留期可配 | 节点死亡后工作区可恢复（拔节点演练） |
| NFR-11 BYO 出站 | BYO 接入流程**只走「主机 → server」出站**（不假设平台可直连用户主机，NAT 后仍可用） | 在 NAT 环境完成注册并上报 workspace |
| NFR-12 token 安全 | join token 一次性 + 短 TTL + 可吊销 + 绑定 member_id；**吊销留审计**（谁/何时/为何） | 重放旧 token 被拒；吊销记录可查 |
| NFR-13 平台可见边界 | 平台管理员**默认仅可见元数据**（成员/状态/成本/资源用量）；**字段清单为封闭枚举（fail-closed，不在清单即不可见）**；访问正文需**租户显式授权**（短 TTL + 单次 + 落审计 + 告警通知租户全部 lead） | 负例：平台无授权读任务/讨论正文 → 拒 + 审计；授权过期后不可再读 |
| NFR-14 凭据分离 | GitHub 身份面合一但**凭据用途分离**：user token 只读身份/仓库列表，**绝不执行 agent 动作**（agent 动作一律 installation token 且归因 device member）；**App private key 不落 node** | 负例：user token 调 agent 动作 → 拒 + 审计；伪造他 installation 事件 → 拒 |

## 7. 验收

1. **全链复用 006 E2E**：把 xiaods-ec2 已跑通的全链（invite→provision→join→workspace 上报→destroy）**原样跑一遍 `provider=k8e`**。
2. **跨租户负例**：A 租户 member 解析/访问不到 B 租户的 serve/namespace。
3. **拔节点演练**：节点死亡 → 工作区可恢复（断言）。
4. **workspace 断言**：`members.workspace` == Pod 实际挂载点。
5. **回收清单**：删成员/删租户逐项 `done`，含 PVC/Secret/证书清理验证。
6. **冷启动埋点**：三段耗时可见。
7. **两步里程碑**：第一步 = 现有 E2E 原样跑 `provider=k8e`（server 不动）；第二步 = 租户创建/回收 + CA 轮换吊销 + 灾备恢复（含 CA）。
8. **迁移一致性**（仅在用户触发迁移时）：文件清单 + 抽查哈希 + 隧道同名重注册，缺一条即回滚。
9. **BYO 出站注册**：在 NAT 后的主机上以 join token 完成注册并上报 workspace；旧 token 重放被拒；token 绑定的 `user_id`/`member_id` 不符被拒。
10. **隧道验收三件套**（覆盖整条链路，非只测提供端）：liveness 断言（能建连）+ idle 后首字节预算（如 ≤ 3s）+ 大包完成率（如 10MB 传输成功率 100%）。
11. **tx-expose 故障注入**：杀掉 expose 边车 → 主容器在 ≤ 90s 内上报 degraded，恢复后隧道自动重注册。
12. **命名唯一性**：同一主机/集群内多成员、多团队的 Pod/PVC/Secret/隧道名**无碰撞**（断言）。
13. **成员唤醒路径**：成员被派任务 → 从"准备中"到可执行的触发者、时限与 UX 有定义并可观测（M1）。

## 8. 与现有系统的关系

- **design/006（EC2 + OS 用户 + systemd）**：并存，作为 BYO/自建档；云上默认走 k8e 容器。
- **#8（GitHub App）**：**身份面合一**（D8 已裁决）：共用 installation 租户键与同一 GitHub App；`users.id` 为「人」的锚点。
- **现有 console**：平台面（登录/租户/集群）与租户面（团队/成员/任务）分层，**第二步**落地；术语统一采用 **admin-console / user-console**（见 §10）。
- **006 复用语义**：006 §3.1 的「按 user 复用主机」在 k8e 路径**不适用**（D1=B：一 team-member 一 Pod）；`nodes`/job/幂等键/状态机/destroy 清单仍复用；**approve 门（裁决 V）在 k8e 路径同样适用**（provision 只建 PVC/Secret，approve 后才建 Pod）。

## 9. 开放问题（不阻塞本稿评审，结论增量补进）

**四段全部裁决完毕**（`design/010`/`011`/`012`），本稿已同步；剩余开放项：
1. PVC 快照频率与保留期（NFR-10 具体值）
2. member 镜像 registry 位置、tag 规范与 semver 门
3. Pod 默认 requests/limits（默认规格）
4. 第一步是否需要 Service（同集群成员互访）还是只经隧道
5. 双 provider **共用 conformance 矩阵**的归属（防双倍测试成本）
6. break-glass 演练（NFR-13）与 token 负例（NFR-14/FR-19）的用例归属

## 10. 术语

**租户** = GitHub installation / teamx team / K8s namespace 的三位一体 · **member** = 一个座位（设备），
一人可多座位 · **provider** = 主机来源实现（`aws` / `k8e` / `fake`）· **expose 边车** = member Pod 内负责隧道出口的容器 ·
**presence** = server 侧成员在线状态（心跳 + 连接）· **break-glass** = 平台管理员的临时提权流程 ·
**admin-console** = 企业级管理看板（原 `teamx ui`，owner/lead 用）· **user-console** = 成员 web 控制台（`teamx console`）

---

## 11. 评审修订记录（v1 → v2）

首轮评审 3 份，verdict 均为 **CHANGES-REQUESTED**（共 14 必改 + 建议），v2 已逐条落实：

| 来源 | 必改 | 落点 |
|---|---|---|
| caicai `task-74` | ① 同步已裁决内容、清理过时表述与不明引用 ② NFR-1 改"默认拒绝 + 显式 allowlist" ③ 量化 FR-5/NFR-5/NFR-6 ④ 阐明 k8e 复用语义与 approve 门 | §8/§9、NFR-1、FR-5/NFR-5/NFR-6、§8 末条 |
| xiaods-mac `task-76` | ① FR-2 里程碑归属 ② D10/D8 补需求落点 ③ NFR-5 阈值 + 隧道验收三件套 ④ FR-20 源保留 ⑤ §8/§9 对齐 ⑥ NFR-7 触发条件 | FR-2、NFR-13/NFR-14、NFR-5/§7.10、FR-20、§8/§9、NFR-7 |
| weiye-mini `task-78` | ① FR-11 与 FR-5 交互 ② join token 的 user 绑定 ③ NFR-3 区分两类 Pod ④ §7 加 tx-expose 故障注入 | FR-11、FR-19/NFR-12、NFR-3、§7.11 |
| 共同建议 | 共用 conformance 矩阵、镜像 pin+semver 门、break-glass 演练、token 负例 | §9 开放项 5/6、§7.9 |
| 缺失需求 | M1 唤醒路径 / M2 秒级可达定义 / M3 命名唯一性 / M4 术语 | §7.13、FR-5、§7.12、§10 |
