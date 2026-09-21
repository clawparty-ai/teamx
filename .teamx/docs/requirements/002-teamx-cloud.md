# 002 — Teamx Cloud 需求分析（草稿 v1）

> 状态：**draft**（待评审） · 所有者：caishu · 日期：2026-09-20
> 输入：`design/009`（第一轮 harvest）、`design/010`（4 条架构原语裁决）、`design/011`（交付与里程碑裁决）
> 关联：`design/006`（现有 EC2 provision 链）、`design/007`（讨论模式）、#8（GitHub App，租户键共用）
> 说明：本文只写**需求**（做什么/边界/验收），不写实现方案（留给 `design/011+`）。

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
| FR-2 | 每租户一个 teamx server 实例（容器，SQLite + PVC，独立 CA） | A2、A3、D2 |
| FR-3 | 租户内可创建 project/team；复用现有 team/成员/角色状态机 | A2 |
| FR-4 | 邀请与审批流程复用 006（邀请函 → import → approve → 服务启动） | C4、C5 |
| FR-5 | 创建成员时**异步** provision 云端 member；member 先可用、provision 后台进行；UI 显示"准备中"，三段进度可见（调度+拉镜像 / workspace 恢复 / agent 热身） | C10、D16、C9 |
| FR-6 | member 运行于 k8e Pod（一 team-member 一 Pod），镜像含 teamx + opencode + 工具链 | D1、C5 |
| FR-7 | member workspace 持久化（PVC per member），容器内固定挂载 `/workspace` | C3、D3 |
| FR-8 | 证书/邀请函经 Secret 注入 → 容器内 import；不进镜像、不进 cloud-init | C4 |
| FR-9 | 成员数据面 1:1 保留：ttyd / fs / forward 可访问，且**断线可自愈**（退避重连 + 探针 + 断线计数） | D5、C6 |
| FR-10 | 在线状态与探针接 **server 侧 presence/online**（不依赖 K8s readiness）；边车失联 → 主容器上报 degraded | D5/P4 |
| FR-11 | 支持 scale-to-zero（idle 删 Pod 留 PVC；有任务再拉起） | C2、C10 |
| FR-12 | 与 EC2 provider 并存：同一 `nodes`/job/幂等键/状态机/回收清单抽象复用 | C1、C16 |
| FR-13 | 回收：可验证清单（停服务 → 删 Pod/namespace → 清 PVC/Secret → 吊销证书/CA） | C14、D2 |
| FR-14 | `workspace` 上报字段为**唯一事实源**；File 菜单/forward 目标/快照策略均消费它 | D3/P3 |
| FR-15 | 冷启动**分段埋点**：①调度+拉镜像 ②workspace 恢复 ③agent 热身 | C9 |
| FR-16 | 平台自身 bootstrap 复用 006 的一次性 token（不另造引导协议） | C15 |
| FR-17 | 升级/回滚/版本 pin 能力（含租户 server 的滚动升级） | C14 |
| FR-18 | **里程碑两步走**：第一步只容器化 member（`provider=k8e`，server 不动）；第二步每租户一 server + 独立 CA + namespace | D13 |
| FR-19 | **BYO 主机接入走一次性 join token**（key 留用户侧；server 不持 BYO 主机私钥；命名含成员短 id） | D6 |
| FR-20 | **迁移工具（可选，用户显式触发）**：EC2 workspace → PVC，带一致性验收（文件清单 + 抽查哈希 + 隧道同名重注册），失败即回滚 | D11 |

## 6. 非功能需求（NFR）

| # | 需求 | 判据 |
|---|---|---|
| NFR-1 隔离 | 租户间 namespace + NetworkPolicy + PSA(restricted) + seccomp + 只读根 + ResourceQuota；**默认禁出网**并显式封 IMDS `169.254.169.254` | 跨租户负例：租户 A 的 member **解析/访问不到** 租户 B 的 serve/namespace |
| NFR-2 身份边界 | 每租户独立 CA；吊销租户 = 废 CA；平台 console 不持跨租户信任 | 吊销后旧证书 RPC 被拒 |
| NFR-3 可用性 | 爆炸半径 = 一个 Pod；节点池分层 / ≥2 worker + 滚动窗口；**拔节点演练**：节点死亡后工作区可恢复 | 拔节点演练通过 |
| NFR-4 持久化 | member workspace 网络附着存储或显式快照策略；**CA 私钥进灾备清单** | 灾难恢复后证书仍有效 |
| NFR-5 可观测 | 重连日志 / 断线计数 / 存活探针接入 presence；在线状态**不撒谎** | 隧道断连后状态在阈值内变为异常 |
| NFR-6 冷启动 | 分段耗时可见；provision 不阻塞交互路径 | 埋点数据可查 |
| NFR-7 成本 | **N/A（v1）**：不做计费/计量/配额（owner 裁决 D14/D15 —— k8s 宿主机数量由 owner 控制，容量风险在资源层已限制） | — |
| NFR-8 安全声明 | 文档**如实声明**默认档隔离强度与适用前提（同信任域内团队），不夸大 | 设计文档含声明段落 |
| NFR-9 审计 | 平台侧审计独立于租户可控台账；break-glass 全程留痕 | 审计不可被租户删改 |
| NFR-10 存储快照 | member workspace 走网络附着存储或**显式快照策略**；频率/保留期可配 | 节点死亡后工作区可恢复（拔节点演练） |
| NFR-11 BYO 出站 | BYO 接入流程**只走「主机 → server」出站**（不假设平台可直连用户主机，NAT 后仍可用） | 在 NAT 环境完成注册并上报 workspace |
| NFR-12 token 安全 | join token 一次性 + 短 TTL + 可吊销 + 绑定 member_id；**吊销留审计**（谁/何时/为何） | 重放旧 token 被拒；吊销记录可查 |

## 7. 验收

1. **全链复用 006 E2E**：把 xiaods-ec2 已跑通的全链（invite→provision→join→workspace 上报→destroy）**原样跑一遍 `provider=k8e`**。
2. **跨租户负例**：A 租户 member 解析/访问不到 B 租户的 serve/namespace。
3. **拔节点演练**：节点死亡 → 工作区可恢复（断言）。
4. **workspace 断言**：`members.workspace` == Pod 实际挂载点。
5. **回收清单**：删成员/删租户逐项 `done`，含 PVC/Secret/证书清理验证。
6. **冷启动埋点**：三段耗时可见。
7. **两步里程碑**：第一步 = 现有 E2E 原样跑 `provider=k8e`（server 不动）；第二步 = 租户创建/回收 + CA 轮换吊销 + 灾备恢复（含 CA）。
8. **迁移一致性**（仅在用户触发迁移时）：文件清单 + 抽查哈希 + 隧道同名重注册，缺一条即回滚。
9. **BYO 出站注册**：在 NAT 后的主机上以 join token 完成注册并上报 workspace；旧 token 重放被拒。

## 8. 与现有系统的关系

- **design/006（EC2 + OS 用户 + systemd）**：并存，作为 BYO/自建档；云上默认走 k8e 容器。
- **#8（GitHub App）**：共用租户键（installation_id）与身份面（D8 待定：合一/分开）。
- **现有 console**：平台面 vs 租户面的分层待定（D-2 段）。

## 9. 开放问题（不阻塞本稿评审，结论增量补进）

**第三段（身份与安全，进行中）**：D8 GitHub 身份面 · D10 平台可见边界细节 · D4 forward 锚点 · D17 隔离分档
**第四段（成本·体验·过程）**：D14 固定开销兜底 · D15 计量口径 · D16 冷启动交互 · D18 收窗节奏

> 已裁决并并入本稿：D1/D2/D3/D5（`design/010`）· D13/D7/D11/D6（`design/011`）· D14/D15/D16/D18（`design/012`）
> 团队已收敛、待 owner 确认：D8/D10/D4/D17（`design/012` §A）

## 10. 术语

**租户** = GitHub installation / teamx team / K8s namespace 的三位一体 · **member** = 一个座位（设备），
一人可多座位 · **provider** = 主机来源实现（`aws` / `k8e` / `fake`）· **expose 边车** = member Pod 内负责隧道出口的容器 ·
**presence** = server 侧成员在线状态（心跳 + 连接）· **break-glass** = 平台管理员的临时提权流程
