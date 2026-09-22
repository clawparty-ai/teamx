# Review: requirements/002 Teamx Cloud 需求分析

- reviewer: weiye-mini（非作者；作者 caishu/owner）
- target: `.teamx/docs/requirements/002-teamx-cloud.md` @ a5d281f（128 行，state=draft）
- date: 2026-09-21
- verdict: **CHANGES-REQUESTED（与 caicai 的 4 必改互补，本评审 4 条独立必改 + 2 建议）**

## 总评

结构完整（范围/角色/场景/FR/NFR/验收/开放问题），18 条 FR + 12 条 NFR 覆盖了四轮讨论的全部裁决。caicai 的 4 必改（时效性同步 / NFR-1 改 allowlist / 量化验收词 / k8e 复用语义阐明）我全部确认成立——本评审不重复，只补 caicai 未覆盖的 4 条。

## 必改（本评审独有）

### 必改 1：FR-11（scale-to-zero）与 FR-5（异步 provision）的交互未定义

FR-11 说「idle 删 Pod 留 PVC；有任务再拉起」——idle 删 Pod 后该 member 的状态是什么（online=false？degraded？）、「有任务再拉起」走的是哪条路径？如果是复用 FR-5 的异步 provision 路径（PVC 还在 → 跳过 workspace 恢复段 → 只做 agent 热身），需要写明：**scale-to-zero 的恢复 = FR-5 路径的子集（跳过 workspace 恢复段）**，且恢复期间 member 在 nodes 表的状态语义（ready→provisioning？还是新增 waking 态？）。否则实现时会出现「FR-11 的拉起」与「FR-5 的 provision」两套并行逻辑。

### 必改 2：FR-19/NFR-12 的 join token 缺 user 绑定维度

NFR-12 只写「绑定 member_id」。但 §3 角色表明确「云端 member：一人可多设备/多座位」，且 user 绑定特性已上线（users.user_id，member.user_bound 实测 @ 5954/5955）——同一 user 多座位时，BYO 主机（属于 user）复用同一主机接多 team，join token 若逐 member 发放 = N 个 token 管理负担，且「主机属于 user」的语义没有体现。**建议**：join token 验证挂 user 身份（token → user → 允许该 user 名下 member 注册），member_id 作为注册时的归属声明（绑定期望 member_id 保留，但验证链是 user）。与 D8 的 users.id「人」锚点统一（我在 #6671/#6652 已提，未进 requirements）。

### 必改 3：NFR-3 爆炸半径未区分两类 Pod

NFR-3 说「爆炸半径 = 一个 Pod」——但 A2（每租户一 server）之后集群里有**两类 Pod**：member Pod（可重建，workspace 在 PVC）与**租户 server Pod**（挂着该租户全部状态：SQLite + CA + 节点注册表）。server Pod 死亡 = 该租户控制面下线（weiye #6526 已论证：调度要分工作负载类别——server 高 PriorityClass + PDB，member 才可驱逐）。**建议**：NFR-3 拆成 NFR-3a（member Pod：爆炸半径=单个 team-member，可驱逐）+ NFR-3b（server Pod：爆炸半径=单租户控制面，高优先+PDB+驱逐保护），验收各自独立（server Pod 驱逐后租户自愈 + SQLite WAL 恢复测试）。

### 必改 4：§7 验收清单缺 tx-expose 故障注入

NFR-9 写「断线可自愈（退避重连+探针+断线计数）」，但 §7 验收 1-9 **没有一条覆盖 tx-expose 故障注入**——而 xiaods-ec2 三天真机排障（白板僵死 4.5h、慢初始化 30s）恰恰证明这是最容易坏、且现有验收抓不住的面（xiaods-mac #6631：两种死法必须拆验收——liveness 断言 / 首字节 latency 预算 / 大包完成率，三项缺一不可；同一 curl 状态码断言会把 hung 与慢混为一谈）。**建议**：§7 补验收 10 = tx-expose 断线自愈（kill 边车容器→自动重连→隧道恢复→stale 转发复活，#6631 判据 2 已真机验证）+ 两种死法区分断言（按 #6631 三件套）。

## 建议（非阻塞）

1. **验收 1 的断言集明确化**：「全链复用 006 E2E 原样跑 provider=k8e」——原 006 E2E 无 workspace 上报断言（那是 ff5548b 后的功能），建议写明验收 1 的断言集 = 原 006 全链 + 验收 4 的 workspace 断言，避免「原样」二字被解读为不含新断言。
2. **FR-16 语义澄清**：「平台自身 bootstrap 复用 006 的一次性 token」——「平台自身」指什么对象（租户 server 的 bootstrap？k8e 集群本身的引导？还是 cloud console 的部署？）语义不清，建议明确定义引导对象。
3. **NFR-1 补 allowlist 初始集**：caicai 必改 2 的「默认拒绝+显式 allowlist」——allowlist 初始集建议在 requirements 就列（serve mTLS / opencode.ai / 镜像仓 / GitHub API——D8 合一后 GitHub API 也需出网），实现阶段直接照抄。
4. **typos**: FR-5 的「member 先可用、provision 后台进行」与 §4 场景 4「member 先秒级可达、可派任务；UI 显示准备中」语义重复但措辞不一——统一为后者（UI 语义更完整）。

## 缺失需求

- **FR-11 的唤醒触发源**：「有任务再拉起」——什么算「有任务」？task.create 指派给该 member？discussion @？定义拉起触发源（建议：仅 task.create 指派触发，讨论 @ 不触发——与唤醒纪律一致）。
- **NFR-5 的「不撒谎」量化**：xiaods 僵死 4.5h 状态显示在线——「状态在阈值内变为异常」的阈值数字未给（caicai 必改 3 已提量化，此处补：建议探测失联 ≥2 个周期即翻转，周期 = 探针间隔，数字进 NFR-5）。
