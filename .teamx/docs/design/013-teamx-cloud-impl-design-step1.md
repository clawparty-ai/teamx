# 013 — Teamx Cloud 第一步实现设计：`provider=k8e` + member 容器化（v3.1 · M0-M12 + 两轮评审 + 真集群验证）

> 状态：**v3.1（M0-M12 全落实 + M4 已裁决 + 两轮评审意见合并 + 真集群验证）** · 所有者：caishu · 日期：2026-09-22
> 说明：本版 = weiye-mini 的 M0-M12 修正（task-86/90/91）+ 本人 §14 与 letter-Secret 修正的**并集**（此前双方各持一半，已按 TEAM.md 同步规则合并）
> 评审：weiye-mini PASS(2必改) · xiaods-mac CHANGES-REQUESTED(5必改) · weiye CHANGES-REQUESTED(8必改) —— 见文末修订记录
> 验证：`tests/k8e-provision-test.sh`（stub kubectl，8 断言）+ `tests/k8e-real-cluster-test.sh`（kind 真集群，8 断言）**全绿**
> 范围：**第一步里程碑**（只容器化 member，server 不动）—— 见 `design/011` D13
> 输入：`design/010`（原语）· `design/011`（交付）· `design/012`（身份/成本）· `requirements/002`（需求）
> 实现落点：`crates/teamx/src/provision.rs`（新增 `K8sProvider` / `KubectlExec`）、`crates/teamx/src/k8s.rs`（新模块）

---

## 1. 目标与不变量

**目标**：让 `teamx node provision --provider k8e` 走通与 006 完全相同的链路（invite → provision → join → workspace 上报 → destroy），
**复用**现有 `nodes` / `provision_jobs` / 幂等键 `(team_id, member_id)` / 状态机 / destroy 清单（C1/C16）。

**不变量**
1. server 侧**不持任何节点私钥**（k8e 路径无需 SSH 密钥）—— 直接满足 D6「key 留用户侧」的精神。
2. 一个 (team, member) 座位 = **一个 Pod**（D1=B）；`installation_id ↔ team_id ↔ namespace` 1:1:1（C11 硬约束）。
3. workspace **固定挂载 `/workspace`**，且 member 上报的 `workspace` 字段是**唯一事实源**（D3）。
4. 服务在 **approve 之后才启动**（006 裁决 V）。

## 2. 抽象映射（006 → k8e）

| design/006（EC2） | k8e（第一步） | 备注 |
|---|---|---|
| EC2 instance | **Pod**（+ PVC + Secret） | `nodes.instance_id` = Pod 名 |
| per-node SSH keypair | **无** | `delete_keypair` → no-op |
| 安全组（只放行 server IP 连 22） | **NetworkPolicy**（默认 deny egress + 白名单） | 见 §7 |
| SSH + bootstrap 脚本 | **`kubectl exec -n <ns> <pod>` + 同一份 bootstrap 脚本**（或镜像 entrypoint）；`KubectlExec` 须携带 `<ns>/<pod>` 维度（M3：`run_as(ip,key,...)` 无此维度，k8e 需 node_id→DB 查 pod 名） | 见 §5 |
| 每实例 6 个 systemd 单元 | **Pod 内多容器（sidecar）** | 见 §4 |
| OS 用户（隔离单元） | **Pod + namespace** | 见 §3 |
| EBS 卷 + 快照 | **PVC + 快照策略**（D7 硬约束） | 见 §6 |
| `nodes.ip` = 公网 IP | `nodes.ip` = **Pod IP**（集群内；**M4 裁决：集群外不可路由，仅供诊断；隧道/转发经既有转发机制**） | `ip_of` 返回它 |

**`nodes` 行在 k8e 下的字段语义（防 server 按 EC2 语义误读 —— 评审必改）**

| 字段 | k8e 取值 | 说明 |
|---|---|---|
| `instance_id` | Pod 名（`teamx-m-<member8>`） | 与 EC2 的 `i-xxx` 同义 |
| `ip` | `<pod>.<ns>.svc` | 集群内地址；**不保证公网可达** |
| `keypair_ref` | **空** | 无 SSH 密钥；`node ssh` 对 k8e 行应报错而非空指针 |
| `os_user` | **空** | 无 OS 用户；destroy 不做 `userdel` |
| `server_url` | 建节点时落库 | Pod env / 隧道 / git clone 用 |
| `provider` | `k8e` | destroy/provision 按它分支 |

## 3. `K8sProvider`（实现 `CloudProvider`）

```rust
pub struct K8sProvider { kubeconfig: Option<PathBuf>, namespace_for: Box<dyn Fn(&str) -> String + Send> }  // 可注入，C11 派生
```

| trait 方法 | k8e 实现 |
|---|---|
| `create(req)` | apply **namespace + PVC + Secret + ResourceQuota + LimitRange**（**不含 Pod** —— Pod 在 approve 阶段建，见 §5）→ 返回 `Created { instance_id: <pod name>, private_ip: Some(<Pod IP>), public_ip: None }`（**M4 裁决：不建 Service**） |
| `status(id)` | 读 Pod `phase`（`Pending/Running/Succeeded/Failed`）→ 映射到 `provisioning/ready/failed` |
| `destroy(id)` | 删 Pod → 删 PVC → 删 Secret（**可验证清单**逐项 `done`） |
| `list()` | 按 label `teamx.io/managed=true` 列 Pod |
| `stop/start` | Pod 删除/重建（保留 PVC）—— 对应 006 的 stop/start |
| `delete_keypair` | no-op |
| `console_output` | no-op（首登指纹校验在 k8e 无对应物） |
| `ip_of` | 返回 **Pod IP**（M4 裁决：不建 Service；Pod 重建后 IP 变化，以最新 status() 为准，仅供诊断——隧道/转发经 expose 边车既有机制） |

**命名规范（含成员短 id，防同主机多租户碰撞 —— D6 子约束 2）**
```
namespace: tenant-<team_id 全长或 ≥16 字符 hash>
pod:       teamx-m-<member_id 前 8>
pvc:       teamx-ws-<member_id 前 8>
secret:    teamx-letter-<member_id 前 8>
labels:    teamx.io/managed=true, teamx.io/team=<team_id>, teamx.io/member=<member_id>, teamx.io/user=<user_id>
```

**namespace_for 可注入（weiye-mini 必改 2）**：用 `Box<dyn Fn>` 而非 `fn` 指针，允许第二步切换为从 installation 映射表动态获取（当前 C11 下 team_id 派生即可，无需注入外部数据——但接口须预留）。**归属校验（M5 补充）**：create 时若 namespace 已存在，须校验其 `teamx.io/team` label == 预期 team_id——不匹配（碰撞或复用残留）则 **fail 并报冲突**，禁止静默复用他人 namespace。

**认证**：第一步 server 在集群外 → 用 **kubeconfig**（`TEAMX_KUBECONFIG` 或默认 `~/.kube/config`）。
⚠️ **M11 跨租户信任域**：现网 server 为多租户共用 → 一份 kubeconfig 覆盖所有租户 namespace = **可 exec 进他人 Pod、删他人 PVC**，冲突 NFR-1/D17。最小收敛：per-namespace SA + 短 TTL token，或 RBAC 收紧到本租户 pods/pvc/secrets+exec 并留审计；**写入 D17 声明 + 列入第二步验收**。
第二步 server 容器化后切 **in-cluster ServiceAccount**（设计预留，不改接口）。

## 4. Pod 组成（一 team-member 一 Pod）

| 容器 | 内容 | 说明 |
|---|---|---|
| `setup`（initContainer） | 从 Secret 读 letter → `teamx team import --session net:<member>` → `git clone <team repo>` 到 `/workspace` | **幂等**（已 import/已 clone 则跳过） |
| `main` | `opencode serve` + `teamx worker` | 承载 agent 主循环 |
| `ttyd` | `ttyd`（web 终端） | 数据面 1:1 保留（D5） |
| `fs` | `teamx fs`（文件服务） | 同上 |
| `expose` | `teamx tunnel expose ttyd-… / fs-…`（边车） | D5=A：Pod 活则隧道活 |

**卷**：`/workspace` ← PVC；`/etc/teamx/letter` ← Secret（readOnly）；`/tmp` ← emptyDir。
**探针**：`expose` 容器 localhost liveness；`main` 检测 expose 失联 → 上报 `degraded`（D5 硬约束 2）。
**重启语义**：容器 `restartPolicy: Always`（等价 006 的 `Restart=always`）；整个 Pod 重建 = 006 的实例重建。

**Pod 内 localhost 互访（端口表）**：NetworkPolicy 只作用于 Pod 之间的流量，**loopback 不受影响**；
`ttyd`(7681) / `fs`(4197) / `opencode-serve`(4201) 均监听 `127.0.0.1`，`expose` 边车按 localhost 端口转发。

**degraded 上报通道必须独立于 expose（评审必改）**：`main` 容器直接经 **teamx RPC（mTLS）** 向 server 上报
（不经过 expose 边车），否则"边车死 = 上报同死"，degraded 永远到不了 server（真机 EC2 现网正是"隧道死而 member 仍在线"）。
presence 三态：`online`（心跳 + 连接）/ `degraded`（main 探测到边车失联，如 localhost 探针失败）/ `offline`（超 TTL）。

## 5. 生命周期映射（裁决 V：approve 后才启动服务）

| 阶段 | 006 | k8e（第一步） |
|---|---|---|
| provision job | RunInstances → SSH → bootstrap → 注册单元（**不启动**）→ node `ready` | apply **PVC + Secret**（+ 预拉镜像）→ node `ready`（**不建 Pod**） |
| approve | server SSH 启动服务组 | **创建 Pod**（服务随之启动）。**落点（M1）**：`start_member` 在 `commands.rs` 按 `provider` 分支——EC2=systemctl start 服务组，k8e=调 `K8sProvider::activate(id)` 建 Pod（trait 已含 activate，task-86 M1） |
| 成员可用 | systemd 单元 running | Pod `Running` |

> 这样"pending 期间不启动服务"**天然成立**（Pod 尚不存在），无需额外的暂停机制。
> 若后续要在 provision 阶段就创建 Pod（预热），可加 `suspend` 字段，第一步不做。

## 6. workspace 与上报（D3）

- 容器内固定 **`/workspace`**；PVC 挂载于此；`TEAMX_HOME=/workspace/.teamx`。
- 所有消费者（File 菜单 / forward 目标 / 快照策略 / 文档 / 脚本）**一律读 member 上报的 `workspace` 字段**，禁止硬编码。
- **验收断言**：`members.workspace` == Pod 的 PVC 挂载点。
- 快照策略（D7 硬约束）：PVC 走网络附着存储（云盘 CSI）；快照频率/保留期进 NFR-10（具体值在评审时定）。

## 7. 安全（requirements NFR-1 / NFR-11）

- **PSA**：namespace 标 `pod-security.kubernetes.io/enforce=restricted`。
- **容器**：`seccompProfile: RuntimeDefault`、`readOnlyRootFilesystem: true`（`/workspace`、`/tmp` 例外）、`runAsNonRoot` + **`fsGroup: <pvc-gid>`（评审补：PVC 挂载点的属组；缺它 initContainer 首部署会卡 Permission denied——runAsNonRoot 下进程 UID 无写权限）**、drop ALL capabilities。
- **CNI 前置校验（M0，最高优先）**：k3s/k8e 默认 CNI **flannel 不执行 NetworkPolicy** → §7 的 deny-all egress / 封 IMDS **静默失效**。Provision 前须探测 CNI 是否支持 NetworkPolicy（试部署一条 deny-all NP → Pod 连外验证），不支持则升级 CNI（如 kube-router/canal）或如实声明安全降级。
- **NetworkPolicy**：默认 **deny-all egress**；白名单放行 ① 租户 server ② registry ③ git 源 **④ opencode.ai（NFR-1 四项，原漏项 M6）**；**显式封 IMDS `169.254.169.254`**（NFR-1）。
- **ServiceAccount**：无 RBAC 绑定（容器不需要 K8s API）。
- **Pod QoS 与优先级（M7）**：member Pod 用 `requests==limits`（Guaranteed QoS）防超卖；**平台侧 Pod（未来租户 server）用更高 PriorityClass + PDB**——第一步 AIO 单节点下至少保证 member Pod 有默认 limit（防无限制 Pod OOM 掉全集群）。验收：超 namespace ResourceQuota 的新 Pod **被拒调度**（负例）。
- **namespace 级兜底**：`ResourceQuota`（cpu/mem/PVC 上限）+ `LimitRange`（默认/上限规格）—— v1 不做计费，
  但把"兜底"放在 K8s 层（design/012 D14）。侧车容器须**显式**小规格，否则被 LimitRange 默认值抬起导致调度失败（真集群踩到）。
- **Secret**：letter 存续期 = 座位存续期（**不删**，Pod 重建/重启不需要重放——M9 评审必改：短 TTL 导致 Pod 重建缺 Secret 起不来，且重放主体无归属）。
- **如实声明**（D17，两段）：**数据面** = 默认档 = 共享内核 + PSA + seccomp + 只读根 + 禁出网 + 封 IMDS；**非目标** = 不防内核侧信道；适用前提 = 同组织成员、共享同一套合规要求。**控制面（M11 同步）** = 第一步用**一份集群级 kubeconfig**，server 持有它即具备**跨租户 exec/PVC 删除能力**——这是控制面信任域，与数据面隔离档位不同；收敛措施（per-namespace SA/短 TTL token/RBAC 收紧+审计）与最终声明见 §3 认证段，第二步 server 入集群后消除。

## 7.5 可逆性（M12：RTO 声明 + 逃生通道）

k8e 路径无 EC2 回退——Pod 形态崩了无逃生通道。最小措施：写明 **RTO 声明**（v1 可为"最佳努力"）+ **手工 runbook**（导出 workspace → EC2 重 provision），列入第二步升级验收。v1 不建自动回退，但必须有文档化的手工逃生路径。

## 8. 第一步不做（明确后置）

scale-to-zero（Pod 常驻）· 每租户 server / 独立 CA（第二步）· gVisor / 独立节点池 · 计费与计量（v1 不做，D14/D15）·
operator/CRD（第一步直接用 kubeconfig 调 API，第二步再评估）· 预热池（只做三段埋点，C9/D16）

## 9. 验收（第一步）

1. **主链**：现有 E2E（`invite → provision → join → workspace 上报 → destroy`）原样跑 `provider=k8e`（复用 `tests/node-provision-test.sh` 结构 + 真实集群变体）。
2. **跨租户负例（M10，改写为 namespace 边界）**：第一步只有一台 server——负例断言三件：① B 租户 member Pod **不可达 K8s API**（无 SA/RBAC 权限，任何 namespace 的资源均不可读写）② A 租户 Pod **访问不到 B 租户的 Pod/PVC/Secret**（NetworkPolicy + namespace 隔离）③ 各租户 member 经 mTLS 只能触达**自己的租户 server**（CA 隔离，无跨租户证书信任）。
3. **拔节点演练**：节点故障后 Pod 重建 + `/workspace` 从 PVC 恢复（断言文件清单一致）。
4. **workspace 断言**：`members.workspace` == PVC 挂载点。
5. **回收清单**：destroy 逐项 `done`（Pod → PVC → Secret），并断言资源在集群中消失。**实现点名（M2）**：`destroy_checklist()`（provision.rs:768-785，现硬编码 8 项 AWS 项）须带 provider 维度——k8e 返回 Pod/PVC/Secret 三项；`provision.rs:1596` 的 `len()>=8` 断言同步改为按 provider 断言。
6. **隧道自愈**：断连后按退避重连；探针接入 server 侧 presence（在线状态不撒谎）。
7. **三段冷启动埋点**：① approve → Pod Running（调度+拉镜像，t0=approve）② workspace 恢复（PVC 挂载 + initContainer 完成）③ agent 热身（opencode serve 起来）。**计时起点 = approve**（provision 阶段不建 Pod）。
7b. **CNI/出网负例（M0 验收断言）**：① Pod 连任意外网（非 allowlist 目标）**必须失败**（deny-all egress 生效的证据）；② CNI 探测不支持 NetworkPolicy 时 **provision 阻断或显式降级声明**（不允许静默继续）。
8. **隧道验收三件套（量化）**：liveness 断言（能建连）+ idle 后首字节 ≤ 3s + 10MB 大包传输成功率 100%。
9. **命名一致性**：provision 建的 PVC 名 == Pod 的 `claimName`（真集群曾因不一致导致调度失败）。
10. **真实集群验证**（kind v1.35，`tests/k8e-real-cluster-test.sh`）：namespace+PSA / PVC+Secret+Quota+LimitRange /
    WaitForFirstConsumer / approve→Pod / PSA 准入+5 容器+seccomp / PVC Bound / NetworkPolicy 未放行 IMDS / destroy 清空 —— **已全绿**。

## 10. 需求映射（`requirements/002`）

| 需求 | 本设计落点 |
|---|---|
| FR-6 member 运行于 Pod | §4 |
| FR-7 workspace 持久化 + 固定挂载 | §6 |
| FR-8 证书经 Secret 注入 | §4 `setup` |
| FR-9 数据面 1:1 + 自愈 | §4 `ttyd`/`fs`/`expose` |
| FR-10 在线状态接 presence | §4 探针 |
| FR-12 与 EC2 并存（同抽象） | §2/§3 |
| FR-13 回收清单 | §3 `destroy` |
| FR-14 上报唯一事实源 | §6 |
| FR-15 冷启动分段埋点 | §9.7 |
| FR-18 两步里程碑（第一步） | 全文 |
| NFR-1/10/11/12 | §7/§6 |

## 11. 开放项（评审时定）

1. ~~PVC 快照的**频率与保留期**具体值（NFR-10）~~ **已裁决（owner @ 2026-09-22）**：频率 = **每日 1 次自动 + 升级/迁移前即时一次**；保留期 = **7 天**（与 design/004 节点快照「保留 7d 后删」口径一致）；PVC 走网络附着存储（云盘 CSI，D7）。开放项关闭。
2. member 镜像的**registry 与多架构发布流程**——**方向（owner @ 2026-09-22）**：私有 registry、**digest pin + semver tag**、**多架构 arm64+amd64**；具体 registry 位置由 WS-G 定（未关闭）。
3. ~~`setup` initContainer 与 `kubectl exec` 两种 bootstrap 载体的取舍~~ **已裁决（owner @ 2026-09-22，采纳 A）**：以 **`setup` initContainer 为唯一正式载体**（声明式 + 幂等，真集群已验证）；`kubectl exec` 仅保留为人工调试/逃生手段，不进 provision 主链。开放项关闭。
4. ~~Pod 资源 requests/limits 的默认值（默认规格）~~ **已裁决（owner @ 2026-09-22，原则）**：member Pod 全部容器 **Guaranteed QoS（requests==limits）**；初值 main `500m/1Gi`、侧车（ttyd/fs/expose）各 `50m/64Mi`，合计不超节点下限（1 vCPU / 2 GiB，006 D/Q），`LimitRange` 兜底；具体数值由 WS-F 真集群 profiling 校准（校准后仍须 Guaranteed）。开放项关闭。
5. ~~M4：第一步是否需要 Service~~ **已裁决（owner @ seq 7397，采纳 (ii)）**：第一步**不建 Service**，`nodes.ip` 存 **Pod IP**（集群外不可路由，仅供诊断；隧道/转发经既有转发机制）；§11.5 关闭，稳定寻址留第二步若需再议。

---

## 12. 评审修订记录（v1 → v2）

首轮 3 份评审（weiye-mini PASS+2 / xiaods-mac CHANGES-REQUESTED+5 / weiye CHANGES-REQUESTED+8），v2 落实：

| 来源 | 必改 | 落点 |
|---|---|---|
| weiye-mini | ① §3 与 §5 矛盾（create 含 Pod vs approve 才建） ② namespace 应可注入 | §3（改为不含 Pod）；实现用普通函数（C11 下 namespace 由 team_id 派生，无需注入） |
| xiaods-mac | ① degraded 上报通道须独立于 expose ② Pod 内 localhost 互访声明 ③ nodes 行 k8e 字段语义 ④ §9.6 三件套量化 ⑤ 预拉镜像写实 | §4（上报走 RPC + presence 三态）/ §4 端口表 / §2 字段语义表 / §9.8 / §8（预拉镜像改为埋点口径） |
| weiye | ① status() Succeeded→failed ② ResourceQuota+LimitRange 缺失 ③ initContainer 幂等加固 ④ ip_of 与 Service 开放项矛盾 ⑤ 映射表补 006 复用语义 | 代码已改 / §7 + 代码 / 代码（setup.lock 原子 mkdir + trap）/ §11.5 保留为开放项 / §2 末条 |
| 共同 | 共用 conformance 矩阵 · 镜像 pin+semver · break-glass 演练 · token 负例 | `requirements/002` §9 开放项 |

### v3.1 合并记录（2026-09-22）

此前台账仓与本地镜像各自演化出半份内容（本地缺 M0-M12、台账仓缺 §14），
按 TEAM.md「台账仓为准 + 保留双方改动」合并：以 weiye-mini 的 v3（M0-M12 + M4）为基，
补入本人的 §14（BYO 台式机开发环境 + 云主机成本决策记录）与 letter-Secret 生命周期修正。

## 13. 真集群验证结果（kind v1.35）

`tests/k8e-real-cluster-test.sh` 8 断言 **ALL PASSED**；过程中抓到 3 个只有真集群才暴露的 bug（已修，见 commit `f0d4109`）：

1. **命名不一致**：provision 用 `req.name`（`teamx-<os_user>`）命名 PVC/Secret，approve 用 `member_id` 命名 Pod
   → scheduler 报 `persistentvolumeclaim "teamx-ws-<member>" not found`。修：`CreateReq::member_id_hint()` + E2E 守卫。
2. **空白名单 YAML 非法**：`egress:` 空值被 API 当 null 拒 → NetworkPolicy **静默缺失**。修：空白名单输出 `egress: []`。
3. **approve 的异步线程随 CLI 进程退出被杀**：apply 只完成第一个文档（Pod），NetworkPolicy 未写完。
   修：k8e 的 `start_member` 改**同步**（kubectl apply 很快；拉镜像是 kubelet 的事）。

另：侧车容器补显式小规格（否则被 LimitRange 默认 500Mi 抬起 → 调度失败 `Insufficient memory`）。

## 14. 开发/测试环境：BYO 台式机（owner 裁决 2026-09-20）

**裁决**：不新开云主机做 k8e 开发环境（成本考虑）；改用**加入团队的台式机**作为 BYO 主机。
本地 kind 仅用于一次性验证（已完成，见 §13），不作为日常开发环境。

**接入方式（脚本化）**：`scripts/dev-cluster-setup.sh` —— 在台式机上运行：
1. 装容器运行时（docker）
2. 装**单节点 k8s**（`DISTRO=k8e` 优先，k3s 兜底 —— 两者都是 k3s 派生，行为等价）
3. 拉团队源码（经 teamx server mTLS 克隆；GitHub 在受限网络下不可达）
4. 构建 member 镜像（`BASE_IMAGE`/`APT_MIRROR` 支持镜像源）
5. 打印 provider 需要的环境变量（`TEAMX_KUBECONFIG` / `TEAMX_K8S_CONTEXT` / `TEAMX_MEMBER_IMAGE` / `TEAMX_K8S_EGRESS_ALLOW`）

**与 BYO 产品路径（D6）的关系**：本脚本是**开发期**的接入方式（人肉执行、信任本机）。
产品化的 BYO 接入（一次性 join token + 出站假设 + 命名含成员短 id）见 `design/011` D6，尚未实现，
但两者共用同一套 provider（kubeconfig 指向集群）+ 同一份 Pod/PVC/Secret 清单。

**为什么不新开云主机（决策记录）**：曾评估 `t4g.xlarge`（4C/16G）+ 100GiB gp3 + us-west-2
（≈ $98/月 + $8/月磁盘；`t4g.large` ≈ $49/月；spot ≈ $30/月）—— 判定**开发期过贵**，
改用手边台式机。若将来需要云端测试机，规格按下式估算：控制面 ~1C/2G + 每 member ~0.5G
+ 构建峰值 ~4G + 3×PVC(10G) ⇒ **4C/16G + 100G gp3** 起步。

**台式机的网络要求**：能出网访问 ① teamx server（成员 RPC/隧道）② 镜像 registry ③ git 源；
**不需要**被平台入站访问（出站假设）。若台式机在 NAT 后，provider 侧（server 或开发机）需能连到
其 API server（`6443`）——开发期通常由开发机直连，故只需开发机 ↔ 台式机互通。
