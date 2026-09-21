# 013 — Teamx Cloud 第一步实现设计：`provider=k8e` + member 容器化（v2 · 已按评审修订 + 真集群验证）

> 状态：**v2（首轮评审必改已落实；真集群验证已通过）** · 所有者：caishu · 日期：2026-09-20
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
| SSH + bootstrap 脚本 | **`kubectl exec` + 同一份 bootstrap 脚本**（或镜像 entrypoint） | 见 §5 |
| 每实例 6 个 systemd 单元 | **Pod 内多容器（sidecar）** | 见 §4 |
| OS 用户（隔离单元） | **Pod + namespace** | 见 §3 |
| EBS 卷 + 快照 | **PVC + 快照策略**（D7 硬约束） | 见 §6 |
| `nodes.ip` = 公网 IP | `nodes.ip` = **Service DNS 名**（集群内） | `ip_of` 返回它 |

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
pub struct K8sProvider { kubeconfig: Option<PathBuf>, namespace_for: fn(team_id) -> String }
```

| trait 方法 | k8e 实现 |
|---|---|
| `create(req)` | apply **namespace + PVC + Secret + ResourceQuota + LimitRange**（**不含 Pod** —— Pod 在 approve 阶段建，见 §5）→ 返回 `Created { instance_id: <pod name>, private_ip: Some(<svc dns>), public_ip: None }` |
| `status(id)` | 读 Pod `phase`（`Pending/Running/Succeeded/Failed`）→ 映射到 `provisioning/ready/failed` |
| `destroy(id)` | 删 Pod → 删 PVC → 删 Secret（**可验证清单**逐项 `done`） |
| `list()` | 按 label `teamx.io/managed=true` 列 Pod |
| `stop/start` | Pod 删除/重建（保留 PVC）—— 对应 006 的 stop/start |
| `delete_keypair` | no-op |
| `console_output` | no-op（首登指纹校验在 k8e 无对应物） |
| `ip_of` | 返回 Service DNS + Pod IP |

**命名规范（含成员短 id，防同主机多租户碰撞 —— D6 子约束 2）**
```
namespace: tenant-<team_id 前 8>
pod:       teamx-m-<member_id 前 8>
pvc:       teamx-ws-<member_id 前 8>
secret:    teamx-letter-<member_id 前 8>
labels:    teamx.io/managed=true, teamx.io/team=<team_id>, teamx.io/member=<member_id>, teamx.io/user=<user_id>
```

**认证**：第一步 server 在集群外 → 用 **kubeconfig**（`TEAMX_KUBECONFIG` 或默认 `~/.kube/config`）；
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
| approve | server SSH 启动服务组 | **创建 Pod**（服务随之启动） |
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
- **容器**：`seccompProfile: RuntimeDefault`、`readOnlyRootFilesystem: true`（`/workspace`、`/tmp` 例外）、`runAsNonRoot`、drop ALL capabilities。
- **NetworkPolicy**：默认 **deny-all egress**；白名单放行 ① 租户 server ② registry ③ git 源；**显式封 IMDS `169.254.169.254`**（NFR-1）。
- **ServiceAccount**：无 RBAC 绑定（容器不需要 K8s API）。
- **namespace 级兜底**：`ResourceQuota`（cpu/mem/PVC 上限）+ `LimitRange`（默认/上限规格）—— v1 不做计费，
  但把"兜底"放在 K8s 层（design/012 D14）。侧车容器须**显式**小规格，否则被 LimitRange 默认值抬起导致调度失败（真集群踩到）。
- **Secret**：letter 注入后可选立即删除（短 TTL），Pod 重建时由 provision 流程重放。
- **如实声明**（D17）：默认档 = 共享内核 + PSA + seccomp + 只读根 + 禁出网 + 封 IMDS；**非目标** = 不防内核侧信道；适用前提 = 同组织成员、共享同一套合规要求。

## 8. 第一步不做（明确后置）

scale-to-zero（Pod 常驻）· 每租户 server / 独立 CA（第二步）· gVisor / 独立节点池 · 计费与计量（v1 不做，D14/D15）·
operator/CRD（第一步直接用 kubeconfig 调 API，第二步再评估）· 预热池（只做三段埋点，C9/D16）

## 9. 验收（第一步）

1. **主链**：现有 E2E（`invite → provision → join → workspace 上报 → destroy`）原样跑 `provider=k8e`（复用 `tests/node-provision-test.sh` 结构 + 真实集群变体）。
2. **跨租户负例**：A 租户的 Pod **解析/访问不到** B 租户的 server/Service/namespace。
3. **拔节点演练**：节点故障后 Pod 重建 + `/workspace` 从 PVC 恢复（断言文件清单一致）。
4. **workspace 断言**：`members.workspace` == PVC 挂载点。
5. **回收清单**：destroy 逐项 `done`（Pod → PVC → Secret），并断言资源在集群中消失。
6. **隧道自愈**：断连后按退避重连；探针接入 server 侧 presence（在线状态不撒谎）。
7. **三段冷启动埋点**：调度+拉镜像 / workspace 恢复 / agent 热身 的耗时可见。
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

1. PVC 快照的**频率与保留期**具体值（NFR-10）。
2. member 镜像的**registry 与多架构发布流程**（私有 registry 位置、tag 规范）。
3. `setup` initContainer 与 `kubectl exec` 两种 bootstrap 载体的取舍（前者更声明式，后者复用 006 脚本更彻底）。
4. Pod 资源 requests/limits 的默认值（默认规格）。
5. 第一步是否需要 **Service**（同集群内成员互访）还是只经隧道。

---

## 12. 评审修订记录（v1 → v2）

首轮 3 份评审（weiye-mini PASS+2 / xiaods-mac CHANGES-REQUESTED+5 / weiye CHANGES-REQUESTED+8），v2 落实：

| 来源 | 必改 | 落点 |
|---|---|---|
| weiye-mini | ① §3 与 §5 矛盾（create 含 Pod vs approve 才建） ② namespace 应可注入 | §3（改为不含 Pod）；实现用普通函数（C11 下 namespace 由 team_id 派生，无需注入） |
| xiaods-mac | ① degraded 上报通道须独立于 expose ② Pod 内 localhost 互访声明 ③ nodes 行 k8e 字段语义 ④ §9.6 三件套量化 ⑤ 预拉镜像写实 | §4（上报走 RPC + presence 三态）/ §4 端口表 / §2 字段语义表 / §9.8 / §8（预拉镜像改为埋点口径） |
| weiye | ① status() Succeeded→failed ② ResourceQuota+LimitRange 缺失 ③ initContainer 幂等加固 ④ ip_of 与 Service 开放项矛盾 ⑤ 映射表补 006 复用语义 | 代码已改 / §7 + 代码 / 代码（setup.lock 原子 mkdir + trap）/ §11.5 保留为开放项 / §2 末条 |
| 共同 | 共用 conformance 矩阵 · 镜像 pin+semver · break-glass 演练 · token 负例 | `requirements/002` §9 开放项 |

## 13. 真集群验证结果（kind v1.35）

`tests/k8e-real-cluster-test.sh` 8 断言 **ALL PASSED**；过程中抓到 3 个只有真集群才暴露的 bug（已修，见 commit `f0d4109`）：

1. **命名不一致**：provision 用 `req.name`（`teamx-<os_user>`）命名 PVC/Secret，approve 用 `member_id` 命名 Pod
   → scheduler 报 `persistentvolumeclaim "teamx-ws-<member>" not found`。修：`CreateReq::member_id_hint()` + E2E 守卫。
2. **空白名单 YAML 非法**：`egress:` 空值被 API 当 null 拒 → NetworkPolicy **静默缺失**。修：空白名单输出 `egress: []`。
3. **approve 的异步线程随 CLI 进程退出被杀**：apply 只完成第一个文档（Pod），NetworkPolicy 未写完。
   修：k8e 的 `start_member` 改**同步**（kubectl apply 很快；拉镜像是 kubelet 的事）。

另：侧车容器补显式小规格（否则被 LimitRange 默认 500Mi 抬起 → 调度失败 `Insufficient memory`）。
