# 013 评审 — Teamx Cloud 第一步实现设计（weiye / co-lead）

> 评审对象：`design/013-teamx-cloud-impl-design-step1.md`（draft，owner caishu，2026-09-20）
> 任务：`task-85`（high，direct，assignee=weiye）· 视角：架构一致性 / 隔离与爆炸半径 / 可逆性与第二步过渡
> 输入：`design/010`（原语 D1/D2/D3/D5）· `design/011`（D7/D11/D13/D6）· `design/012`（D8/D10/D17）· `requirements/002` v2
> 日期：2026-09-21 · 代码基线：`origin/main`（我方工作区 checkout，crates/teamx @ provision.rs 1770 行）

## verdict：**CHANGES-REQUESTED**

方向与 010–012 的裁决**一致**（第一步只容器化 member、approve 门 V、D1=B 一 Pod 一座位、D3 上报唯一事实源、D6 key 留用户侧、D5=A 边车），
`provider=k8e` 的选择与 A4/D13 相符。但作为**实现级**设计，**5 处实现接缝没有落点**、**3 处与已裁决 NFR 的量化验收对不上**，
且第一步引入了一个**控制面的跨租户共享信任域**（§3 kubeconfig）——按现状实现会在 approve、destroy、exec 三处各自发明，
且 §9 的部分验收无法通过。以下必改项解决后可转 PASS。

---

## 一、必改（blocking，带判据）

### M1 approve 门在 k8e 没有实现落点
- 事实：现有 approve 路径是**硬编码 SSH + systemd**——`commands.rs:2668` 发 `sudo systemctl start 'teamx-{os_user}-*'`；
  而 `CloudProvider` trait（`provision.rs:102-128`）只有 `create/status/destroy/list/stop/start/ip_of/delete_keypair/console_output`，
  **没有"让工作负载开始运行"这一动作**。
- 013 §5 要求「approve 时创建 Pod」，但没写这个接缝落在哪一层。
- 判据：文档必须二选一并给出签名/落点——(a) trait 增 `activate(&self, id) -> Result<(),String>`（AWS 默认实现=SSH 启动服务组，K8sProvider=建 Pod），
  或 (b) 明写「在 commands.rs 按 `node.provider` 分支，不进 trait」及理由。否则 §1「走通与 006 完全相同的链路」在 approve 处断裂。

### M2 destroy 清单与执行器是 AWS 形状，不是 provider 中立
- 事实：`destroy_checklist()`（`provision.rs:768-785`）硬编码 8 项 AWS 步骤（terminate-instance / release-eip / cleanup-security-group / cleanup-dns /
  delete-keypair / purge-secrets / revoke-member-cert / delete-os-user），并有测试断言 `len() >= 8`（`provision.rs:1596`）；
  `run_destroy()`（`provision.rs:1149-1194`）先 `exec.run_as(ip, key, "ubuntu", "systemctl stop …; userdel …")`。
- 013 §1/§3 声明「复用 destroy 清单（C1/C16）」，但 k8e 的真实清单是 Pod → PVC → Secret，两者不同构。
- 判据：清单必须带 provider 维度（`destroy_checklist(node)` 读 `node.provider`，或 trait 增 `destroy_checklist()`）；
  §9.5 的验收要在 k8e 上断言**真实资源消失**（Pod/PVC/Secret），不得断言 release-eip/delete-os-user 这类不存在步骤；
  同时给出 `provision.rs:1596` 那条现有测试的替换方案（否则改清单即红）。

### M3 `RemoteExec` 接口装不下 `kubectl exec`
- 事实：`RemoteExec::run_as(ip, key_path, user, script)` / `upload(ip, key_path, local, remote)`（`provision.rs:814-819`）
  **只有 ip+"私钥路径"+user，没有任何 namespace / pod / team 维度**；而 013 §3 把 `nodes.ip` 定义为 Service DNS 名。
- 后果：`KubectlExec` 实现从入参**无法推导**该 exec 进哪个 namespace 的哪个 Pod。
- 判据：三选一写进文档——(a) `ip` 字段承载 `<namespace>/<pod>`；(b) trait 增上下文参数（namespace/pod/team）；
  (c) `KubectlExec` 持 `node_id` 并在实现内查 DB；同时说明 k8e 下 `key_path`/`user` 如何取空值以及 bootstrap 脚本的 stdin 语义是否不变。

### M4 §2/§3 与 §11.5 自相矛盾（第一步到底有没有 Service）
- §3 规定 `nodes.ip` = **Service DNS 名**、`ip_of` 返回「Service DNS + Pod IP」；§11.5 却把「第一步是否需要 Service」列为**开放项**。
  没有 Service 就没有稳定 DNS，`nodes.ip` 的语义随之失效。
- 判据：二选一并全文一致——(i) 每 member 建 Service（则关闭 §11.5，并把 Service 纳入 destroy 清单与 §9.5 断言）；
  (ii) 不用 Service，则 `nodes.ip` 改存 Pod IP，并在 §6/§9.4 说明 workspace 与隧道**不依赖其稳定性**（Pod 重建即变）。

### M5 NFR-1 的出网白名单漏了 `opencode.ai`
- 事实：`requirements/002` NFR-1 必须放行四项：租户 server、**`opencode.ai`**、镜像 registry、git 源；013 §7 只列了前三项。
- 而 §4 的 `main` 容器跑 `opencode serve` —— deny-all egress 下缺此项即启动失败。
- 判据：白名单写全四项；并在 §9.1 主链 E2E 里保留"白名单外目标不可达（含 IMDS）"的负例断言。

### M6 AIO 单节点下没有任何横向爆炸半径控制
- 事实：NFR-1 明确要求 `ResourceQuota`；013 §7 未提，§11.4 又把 requests/limits 降为开放项。
  C2 = 单节点 all-in-one ⇒ **一个无限制 Pod 可 OOM 掉全集群**，010 §1 宣称的「爆炸半径=一个 Pod」在 AIO 上不成立。
- 判据：把「每 namespace ResourceQuota（CPU/内存/Pod 数）+ 每 Pod requests=limits（Guaranteed）+ PriorityClass」
  从开放项**提升为第一步必做**，并在 §9 增一条验收：把某 Pod 的请求抬到超 quota 必须被**拒绝**（而不是挤掉邻居）。

### M7 FR-15/NFR-6 的三段埋点与 §5 生命周期错位
- 事实：FR-5/NFR-6 要求「调度+拉镜像 / workspace 恢复 / agent 热身」三段可见、Pod Running P50≤180s/P95≤300s、埋点保留 30d；
  但 §5 规定 provision 阶段**只建 PVC+Secret、不建 Pod**，Pod 在 approve 时才建 ⇒ 调度与拉镜像发生在 approve 之后，
  「member 记录创建 ≤2s」那段天然没有 Pod 埋点。
- 判据：定义计时起点与阶段边界（建议 t0=approve 建 Pod，t1=Scheduled，t2=镜像拉取/initContainer 完成，t3=main ready），
  并说明 §9.7 在 approve 前如何呈现为「未开始」；否则 §9.7 与 NFR-6 的量化验收无法判过。

### M8 letter Secret 的短 TTL 与「Pod 重建」路径冲突
- 事实：§7 写「letter 注入后可选立即删除（短 TTL），Pod 重建时由 provision 流程重放」；但 provision job 是**一次性**的
  （幂等键 `(team_id, member_id)`，`provision.rs:5`），而 `setup` initContainer **只在 Pod 创建时跑一次**（容器 `restartPolicy: Always` 不会重跑 initContainer）。
  ⇒ 任何 Pod 重建（节点故障 / stop-start / PVC 重挂）都会因缺 Secret 起不来，且"谁重放"无主体。
- 判据：二选一写清——(i) Secret 存续期 = 座位存续期（与 EC2 上 `~/.teamx/letters` 落盘等价，destroy 时随清单清理）；
  (ii) 保留 TTL，但指定重放触发者（哪条命令/哪个 job、与一次性 job 幂等键如何共存）与失败后的可见错误。

---

## 二、可逆性与「第二步不被第一步锁死」（本任务指定视角）

### R1 第一步引入的是「集群级 kubeconfig = 跨租户 exec 能力」——**这一条实际是必改**
- 事实：§3 认证 = server 持 kubeconfig（`TEAMX_KUBECONFIG`）。而第一步**沿用现有 server**（FR-2），
  那台 server 是**多租户共用**的 ⇒ 一份 kubeconfig 覆盖所有租户 namespace。
  等价于：任一租户若拿到 server 的 kubeconfig，即可 `exec` 进**别的租户**的 Pod（绕过 D10/NFR-13 直接读正文）并删他人 PVC。
- 这与 NFR-1「跨租户解析/访问不到」在**控制面**直接冲突，也与 D17「如实声明」的现有文案不符（现文案只声明了数据面：共享内核+PSA+seccomp+只读根+禁出网+封 IMDS）。
- 判据：(a) 把该风险写入 D17 声明（第一步控制面 = 共享信任域，含跨租户 exec 能力）；且 (b) 给最小收敛——`kubectl create token`（短 TTL）
  per-namespace ServiceAccount 随 provision 生成/轮换，或至少把 kubeconfig 的 RBAC 收紧到「本租户 namespace 的 pods/pvc/secrets 增删 + pods/exec」并留审计流（NFR-9）；
  (c) 列为第二步验收项（per-tenant SA + namespace 级隔离）。

### R2 k8e 成员没有逃生通道（可逆性缺口）
- D11 默认 C（并存不迁移）+ FR-20 迁移工具可选 ⇒ 一旦 Pod 形态不可用（集群故障 / AIO 节点死），该成员**无 EC2 回退**，
  RTO 只能等集群恢复；D7 只把"节点死"降级为"数据可恢复"，没解决"人怎么继续干活"。
- 判据：写明第一步的可用性声明（k8e 成员在集群故障期间的 RTO = 集群恢复时间），或把 FR-20 的**反向最小路径**
  （导出 workspace tar → 在 EC2 重新 provision）作为手工 runbook 提进第一步当逃生闸。

### R3 第二步过渡路径检查（结论：基本未锁死，3 处需显式留口）
1. `provider_for(pname, region)`（`commands.rs:6458/6476`）的 k8e 分支要接受「无 region」（region 对 k8s 无意义）——
   请明确 `nodes.region` 存集群名还是空，否则第二步多集群选路含糊。
2. 命名 `namespace: tenant-<team_id 前 8>` **只有 32 bit 熵**，而第二步 namespace 是**安全边界**：
   截断碰撞 = 两个租户落进同一 namespace（最坏情况跨租户数据混住）。判据：用全长 id 或 ≥16 字符 hash，
   并在 apply 前做归属校验（label `teamx.io/team` 不匹配即拒）——这条 006 已经吃过"按 user 复用主机"的碰撞教训（010 §1 记录在案）。
3. `stop/start` = 删/重建 Pod：要说明重建是否保留 `nodes.ip`（这依赖 M4 的选择），以及第二步 server 容器化后该语义是否改成租户级。

---

## 三、缺失项（补齐即可，不阻塞 verdict）

| # | 缺什么 | 建议落点 |
|---|---|---|
| C1 | §10 需求映射漏 FR-1..5/11/16/**17**/P1..P6 与 NFR-2/3/5/6/8/9/13/14（含应显式标 N/A 的第二步项）；FR-17（升级/回滚/pin）在第一步就有实体（镜像 tag + Pod 重建升级） | §10 |
| C2 | PVC 的 storageClass / 容量 / accessModes，以及**存储类必须支持快照**这一前提（D7 硬约束 + §9.3 拔节点演练只有在支持 volume snapshot 时才能过） | §6/§11.1 |
| C3 | namespace 生命周期：destroy 只删 Pod/PVC/Secret，FR-13 还有 namespace ⇒ 第一步是否保留、孤儿资源如何检查 | §3/§9.5 |
| C4 | NFR-5「在线状态不撒谎」的量化：探针间隔与 90s 阈值的关系、断线计数/重连日志可按成员查询的落点 | §4/§7 |
| C5 | §9 缺"命名唯一性"断言（requirements §7.12）：同集群多 team、同用户多座位、短 id 前缀碰撞的负例 | §9 |
| C6 | k8e 路径的 destroy/exec **审计**（NFR-9：独立于租户台账）——第一步至少要有记录 | §7 |
| C7 | 镜像多架构与目标集群架构断言（现网已有 `t4g.small`=arm64 先例，AIO 若为 arm64 则 manifest 必须含 arm64） | §11.2/§9 |
| C8 | `nodes.instance_id` = Pod 名：Pod 名需与 namespace 组合才全局唯一，写明是否加 namespace 前缀（避免多租户同名 Pod 误击） | §3 |

## 四、对 010–012 的一致性核对结论（供 owner 参考）

- D1=B 一 (team,member) 一 Pod：§3/§4 落地 ✔；但 010 §1 记录的"按 user 复用主机不适用、×N 代价"在 §11 未体现兜底（scale-to-zero 已后置），建议在 §8「第一步不做」里显式承接该代价。
- D3 固定 `/workspace` + 上报唯一事实源：§6 ✔（建议在 §9.4 断言里同时断言 `TEAMX_HOME` 与 git 仓库根都在 PVC 内）。
- D5=A 边车 + localhost 探针 + degraded 上报：§4 ✔（补 C4 的量化）。
- D6 key 留用户侧：§1 不变量 1 由"k8e 无需 SSH 密钥"天然满足 ✔；但 R1 的 kubeconfig 是**新的等价凭据**，需并入 D6/D17 叙事。
- 裁决 V（approve 后才启动）：§5 方向 ✔（须补 M1 的落点 + M7 的计时域）。
- D13 两步：§8 后置项与 011 §1 一致 ✔（须补 R3-2 的 namespace 熵与 R1 的控制面收敛）。

---

*评审依据：010/011/012 裁决全文、requirements/002 v2、013 全文，以及 `crates/teamx/src/provision.rs`（trait/清单/执行器）、
`crates/teamx/src/commands.rs`（approve 路径）、`crates/teamx/src/db.rs`（nodes schema）的代码事实。所有行号以评审时 checkout 为准。*

---

## 附：本成员（weiye）两份评审的**合并结论（union）**——以本文件为权威

**背景（流程事实，供 owner 判断）**：`task-85` 是**按成员**（assignee = 成员 `a328fd4c`）派发的，而该成员当时有**两个活跃会话**。
两个会话各自独立完成了这份评审，产出两份文件：
- 本文件（`013-review-teamx-cloud-weiye.md`，commit `081f232`，8 必改 + 可逆性 3 项 + 缺失 8 项）——**权威版**；
- `013-review-teamx-cloud-impl-step1-weiye.md`（commit `5b7a41e`，4 必改 + 2 建议）——同一成员的**独立第二视角**，保留备查。
两份是**独立得出**的，重复项因此构成交叉验证（可信度更高），差异项按下面归并。

### 归并后的必改清单（按严重度排序，取代上面 §一/§二 的分列）

| # | 必改 | 来源 | 变化 |
|---|---|---|---|
| **M0** | **CNI 执行能力前置校验**：k3s/k8e 默认 CNI（flannel）**不执行 NetworkPolicy** ⇒ §7 的 deny-all egress / 封 IMDS **静默失效**（无报错）。必改：把「集群 CNI 支持并已启用 NetworkPolicy 执行」列为 provider 前置（provision 时探测 + 文档硬要求），并在 §9 加负例：Pod 连 IMDS/任意外网**必须失败** | 第二视角① | **新增，且升级为第 1 条**——它让 §7 全部安全控制变成装饰，比我的 M6（白名单漏 `opencode.ai`）更根本 |
| M1–M3 | approve 门无 trait 落点 / destroy 清单是 AWS 形状 / `RemoteExec` 无 namespace 维度（均有 provision.rs、commands.rs 行号取证） | 本文件 | 不变（第二视角未覆盖） |
| M4 | Service 与 `nodes.ip`/`ip_of` 语义二选一（正文与 §11.5 自相矛盾；server 在集群外解析不到集群内 DNS） | 本文件 + 第二视角④-2 | 合并为一 |
| M5 | **`create` 先确保 namespace 存在**（首次 provision 必失败）+ namespace 用全长 id/≥16 字符 hash 且 apply 前校验 `teamx.io/team` 归属 | 第二视角④-1 + 本文件 R3 | 合并为一 |
| M6 | §7 出网白名单漏 `opencode.ai`（NFR-1 四项之一） | 本文件 | 不变 |
| M7 | AIO 单节点必须给 ResourceQuota + Guaranteed + PriorityClass（否则一个 Pod 可 OOM 全集群） | 本文件（mini 评审同向） | 三方一致 |
| M8 | 三段埋点计时域（§5 不建 Pod vs FR-15/NFR-6 要调度/拉镜像分段） | 本文件 | 不变 |
| M9 | **letter Secret 不删**（存续期=座位存续期，destroy 时随清单清）——否则 §9.3 拔节点演练必然失败 | 本文件 M8 + 第二视角② | **两份独立同结论，交叉验证** |
| M10 | §9.2 跨租户负例在第一步是**空集**（只有一个 server），改写为 namespace 边界：K8s API 不可达（SA 无 RBAC）+ 访问不到他团队 Pod IP/PVC/Secret；「B 租户 server」负例留给第二步 | 第二视角③ | 采用其表述（比我的写法更准） |
| M11 | 控制面跨租户信任域：第一步 server 持集群级 kubeconfig = 可 exec 进任意租户 Pod；须写入 D17 声明并给最小收敛（per-namespace SA + 短 TTL token 或 RBAC 收紧 + 审计） | 本文件 R1 | 不变（第二视角未覆盖） |
| M12 | 可逆性：k8e 成员无 EC2 逃生通道，需 RTO 声明或「导出 workspace→EC2 重 provision」runbook | 本文件 R2 | 不变 |

### 归并后的建议（非阻塞）
- `status()` 的 `Succeeded` → **failed/degraded**（常驻 Pod 主容器退出 ≠ ready）｜第二视角建议 1
- `runAsNonRoot` + PVC 写权限（`fsGroup`）要写明，否则首部署即 Permission denied｜第二视角建议 2
- §10 需求映射补 FR-17/NFR-3/5/6/8/9/13/14（或显式标 N/A）｜本文件 C1
- PVC storageClass/容量 + **存储类必须支持快照**（§9.3 依赖）｜本文件 C2
- 镜像多架构断言（现网已有 `t4g.small`=arm64 先例）｜本文件 C7
- `node ssh` 的 kubectl-exec 形态给出命令示例，避免实现时现场发明｜第二视角 缺失项
- 缺失测试 3 条（PVC 持久化断言 / initContainer 幂等 / NetworkPolicy 白名单负例）｜mini 评审同向

### 结论（不变）
**CHANGES-REQUESTED**——M0 是最重的一条，其余为局部修改，不动骨架；M0–M12 改完可做快速增量复核。

### 流程问题（需 owner 决策，非本设计的问题）
「**按成员派发的任务在成员有多会话时会被两个会话同时执行**」——本轮同一成员产出两份评审、两次 `doc.done`（seq 6854 与 6864），
台账里 `task-85` 出现两条不同的完成记录。建议：任务派发/签收加**会话级租约**（首个 ack 的会话持有，其余会话看到 `claimed-by-session` 即让位），
或把 assignee 细化到会话。已在 Issue #20（同一成员身份多会话重复投递）补本案例。
