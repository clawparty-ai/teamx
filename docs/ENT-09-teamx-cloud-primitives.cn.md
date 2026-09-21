# 010 — Teamx Cloud 架构原语（第二轮裁决）

> 状态：**已裁决（owner 2026-09-20）** · 所有者为 caishu
> 来源：第二轮讨论 `dsc_18d731ef572734c526ea4e#6628`；输入 = 第一轮 harvest `design/009`
> 关联：`design/006`（现有 provision 链，EC2 + OS 用户 + systemd）、`design/004`（provider/成本模型）
> 用途：**需求分析（`requirements/002`）与后续设计（`design/011+`）的边界输入**

---

## 0. 已定前提（来自第一轮 A 组，不再讨论）

A1 共享集群 · A2 每租户一个 server · A3 先用 SQLite(PVC) · A4 容器化与 EC2+systemd 并存且 cloud 只支持 k8e ·
A5 BYO 主机（平台不代开）· A6 v1 不做计费/配额 · A7 k8e 是基础底座

---

## 1. D1 隔离对应物 = **B（一 team-member 一 Pod）**

- 一个 (team, member) 座位 = 一个 Pod；Pod 只属于一个 namespace。
- **C11 升格为硬约束**：`installation_id ↔ team_id ↔ namespace` **1:1:1**。
  推论：A（同 Pod 多 team 容器）与 C（同容器多 team）在 K8s 语义上不成立，正式出局。
- **已知代价（记录在案）**：design/006 的「按 user 复用主机」（reuse-first）语义在云上**不适用** ——
  一个用户挂 N 支团队 = N 个 Pod + N 份 PVC。对冲：C10（member 先建 + provision 异步）、
  scale-to-zero、预热池；多数用户只有 1 支团队，×N 只落在多团队重度用户。
- **附带收益**：爆炸半径回到「一个 Pod 死 = 一个 team-member」，比共享底座全员受影响收敛（配合 D7 节点池分层）。

## 2. D2 CA 归属 = **每租户独立 CA**

- 与 A2（每租户一 server）配套：CA 并入租户 server 的生命周期，生成/轮换是 bootstrap 的一步，边际成本≈0。
- 吊销租户 = 废该 CA（一次动作）；对比共享 CA + CN 编租户：吊销需前缀匹配 + 全量复查（#62 已修过一次的身份混淆模式）。
- **硬约束：CA 私钥必须进租户 server 的灾备/备份清单** —— 否则灾难恢复后全体成员证书失效，只能全员重签（C15）。
- 边界提醒：平台 console 作为客户端需按租户存放成员证书，**不得**做成"信任所有租户 CA"的角色（与 C8 同向）。
- 举证责任：若 caicai 能给出「平台能力依赖共享信任域」的具体依赖，本条重审；缺席不阻止结案。

## 3. D3 workspace 路径 = **固定 `/workspace` + 上报字段为唯一事实源**

- 容器内固定挂载点 `/workspace`；EC2 存量路径不动。
- **所有消费者一律读 member 上报的 `workspace` 字段**（现网已动态上报），禁止硬编码；
  systemd 工作目录 / 文档 / 脚本三处 baked 全部降级为消费方。
- **验收锚点**：E2E 断言 `members.workspace` == Pod 实际挂载点。
- 双 provider 并存期红利：同一镜像在 EC2（任意路径）与 k8e（`/workspace`）都能跑，只要消费方不硬编码。

## 4. D5 隧道（member 数据面）= **A + C**

- **A sidecar expose**：每 member Pod 带 expose 边车，Pod 活则隧道活（生命周期天然正确）。
- **C 用户态自愈**：退避重连 + 存活探针 + 断线计数（今天已修 provider 侧 90s 空闲超时 + 退避重连，**k8e 路径复用同一代码**）。
- **反对 B（节点级多路 expose）**：跨租户流量在同一进程交汇，与 C6（默认禁出网）/C8（平台默认无 exec）的隔离方向相反。
- **硬约束**：
  1. 探针与在线状态接 **server 侧 presence/online**（不是 K8s readiness —— expose 是出站连接，readiness 管不到）；
     这条同时修掉「隧道僵死 4.5h 而状态仍显示在线」的撒谎问题。
  2. 边车加 **localhost 存活探针**；主容器检测边车失联即上报 degraded。
- 事实澄清：xiaods 实测 ttyd 现象为**慢初始化**（首次访问慢、之后正常），不是"死亡"；
  该失败模式归入「可观测」而非「可用性」需求。

---

## 5. 由原语推导出的新增硬约束（需求分析必须覆盖）

| # | 约束 | 来源 |
|---|---|---|
| P1 | C11 三元组 1:1:1（installation_id ↔ team_id ↔ namespace）为硬约束 | D1 |
| P2 | CA 私钥纳入租户 server 灾备清单 | D2 |
| P3 | `workspace` 上报字段为唯一事实源；E2E 断言其 == Pod 挂载点 | D3 |
| P4 | 隧道探针/在线状态接 server 侧 presence；边车 localhost 存活探针 | D5 |
| P5 | 每租户 CA + 平台 console 不持有跨租户信任 | D2 |
| P6 | member 数据面 1:1 保留（ttyd + fs + forward 自恢复） | D5 |

## 6. 仍待第二轮讨论（不阻塞需求分析启动，结论增量补进需求）

- **第二段（交付与里程碑）**：D13 一步/两步 · D7 爆炸半径与存储 · D11 存量迁移 · D6 BYO 接入
- **第三段（身份与安全）**：D8 GitHub 身份面 · D10 平台可见边界 · D4 forward 锚点 · D17 隔离分档
- **第四段（成本·体验·过程）**：D14 固定开销兜底 · D15 计量口径 · D16 冷启动交互 · D18 收窗节奏

## 7. 评价维度（沿用第一轮）

隔离强度 · 爆炸半径 · 成本（固定/长尾）· 冷启动 · 运维负担 · 可逆性 · 可观测性 · 数据主权 · 体验 · 承诺与实现的落差（诚实声明优先）
