# Review: design/013 — Teamx Cloud 第一步实现设计（provider=k8e + member 容器化）

- reviewer: weiye（co-lead，架构/评审；task-85）
- target: `.teamx/docs/design/013-teamx-cloud-impl-design-step1.md` @ 台账（作者 caishu，reviewer≠author ✅）
- date: 2026-09-21
- verdict: **CHANGES-REQUESTED（4 必改 + 2 建议；改动均为局部，方向正确，改完无需整轮重审——我可快速复核）**

## 总评

抽象映射（006→k8e）、复用面（nodes/provision_jobs/幂等键/状态机/destroy 清单）、验收设计（E2E 复用 + 跨租户负例 + 拔节点 + workspace 断言 + 三段埋点）都成立，且把第一轮 C 项（C3/C6/C8/C9/C10/C11/C15/C16）全部落到了具体机制。**方向正确，可作为第一步的实现骨架**。但有 4 处必改——其中 CNI 一条不做，§7 的全部安全控制就是**装饰**。

## 必改（4）

### ① CNI 前置条件缺失：默认 flannel **不执行 NetworkPolicy**，§7 会整体失效 🔴

k3s/k8e 默认 CNI（flannel）**不实现 NetworkPolicy**——§7 的「默认 deny-all egress + 封 IMDS」如果跑在默认 CNI 上，NetworkPolicy 对象**静默无效**，安全控制全部落空且无报错（最危险的一种失效）。

**必改**：
1. 把「集群 CNI 必须支持 NetworkPolicy 执行（kube-router/calico/cilium，k8e 需显式启用）」列为 **provider 前置校验**（provision 时探测或文档硬性要求）；
2. §9 验收加一条**负例**：member Pod 内尝试连 IMDS `169.254.169.254` 与任意外网地址，**必须失败**——把「安全控制生效」从文档变成可证伪断言。

### ② 「letter Secret 注入后可选立即删除」与 §9.3 拔节点演练自相矛盾 🔴

Pod **重调度**（§9.3 拔节点演练就是这一幕）会重新执行 initContainer → 需要再次读 letter Secret；若按 §7「注入后可选立即删除」，重建的 Pod 要么挂载失败要么 setup 失败——**§9.3 必然失败**。

**必改**：v1 明确「**不删除**，Secret 生命周期 = 座位生命周期，destroy 时随清单删除」；「短 TTL 引导凭据」的语义（#6071）在此不适用——letter 是长生命周期的成员凭据，不是一次性引导凭据。若坚持短 TTL，必须定义「谁在 Pod 重调度时重放 Secret」（一个不存在的组件）。

### ③ §9.2 跨租户负例在第一步下是空集，需重写为真实边界 🟠

第一步只有**一个 server**（D13 第一步：member Pod 直连现有 server），不存在「B 租户的 server」——§9.2「A 租户访问不到 B 租户的 server」无从断言。第一步的真实租户边界是 **namespace 之间**。

**必改**：§9.2 改为——A 团队 namespace 的 Pod：① K8s API 不可达（SA 无 RBAC）；② 访问不到 B 团队的 Pod IP/PVC/Secret（NetworkPolicy 隔离验证）；③ 只能连到自己租户 server 的白名单出口。（「B 租户的 server」负例留给第二步——每租户 server 落地时自然出现。）

### ④ `create` 缺「确保 namespace 存在」步骤；`nodes.ip` 语义与自家开放项矛盾 🟠

1. §3 的 `create` 只 apply PVC/Secret/Pod，**没有创建 namespace**（`tenant-<team_id 前 8>`）——首次 provision 会直接失败。补：create 先确保 Namespace 存在（或由平台预建 + provider 校验，二选一写明）。
2. §2/§3 让 `ip_of` 返回「Service DNS（集群内）」，但 ① server 在**集群外**（kubeconfig 模式），该 DNS 不可解析；② §11-5 自己把「要不要建 Service」列为开放项——**正文与开放项自相矛盾**。改：第一步 `ip` 用 Pod IP（注明「集群外不可路由，仅供诊断」）或 `None`，Service 留给 §11-5 的结论。

## 建议（2，非阻塞）

1. **`status()` 的 `Succeeded` 映射错了**：member Pod 是常驻负载，主容器退出 = **failed/degraded**，不应映射到 `ready`——否则「agent 崩溃退出」的 Pod 在 `node status` 里显示健康（又一个"不撒谎"问题，与 D5 同族）。
2. `runAsNonRoot` + PVC 的写权限：initContainer 需以同 UID 修卷属主或用 `fsGroup`——不写清楚，第一次部署就会卡在 Permission denied（值得在 §4 加一行）。

## 缺失需求 / 风险

- **CNI 前置**进 `requirements/002` 的 NFR（建议 NFR-1 下加一条「集群必须具备 NetworkPolicy 执行能力并经验证」）。
- 存量 EC2 节点与 k8e 节点在 `nodes` 表共存（provider 列）——destroy 清单按 provider 分支（k8e 无 keypair/安全组），设计已含但 `node ssh` 的 kubectl-exec 形态建议给出命令示例，避免实现时现场发明。

## 结论

**CHANGES-REQUESTED**：4 条必改都是局部修改（前置校验 + 负例 + 两处语义修正 + namespace 步骤），不动骨架。改完后我可以做快速增量复核，不必整轮重审。

—— weiye（co-lead · 架构/评审，task-85）
