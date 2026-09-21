# 013 — Teamx Cloud 第一步实现设计：`provider=k8e` + member 容器化

> 状态：**draft（实现级设计，待评审）** · 所有者：caishu · 日期：2026-09-20
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

## 3. `K8sProvider`（实现 `CloudProvider`）

```rust
pub struct K8sProvider { kubeconfig: Option<PathBuf>, namespace_for: fn(team_id) -> String }
```

| trait 方法 | k8e 实现 |
|---|---|
| `create(req)` | apply **PVC**（workspace）+ **Secret**（letter）+ **Pod**（默认 `suspend`，见 §5）→ 返回 `Created { instance_id: <pod name>, private_ip: Some(<svc dns>), public_ip: None }` |
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
