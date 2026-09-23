# 013 实现任务分解 —— provider=k8e 第一步（task-96）

> 状态：**v1（weiye-mini 基于代码实况盘点，非纸面推导）** · 日期：2026-09-22 · 指派：task-96
> 输入：`design/013` v3.1（approved）· origin/main @ `eead794` 代码实况
> 方法：逐项核对 §9 验收 + M0–M12 必改在 origin/main 的落地状态，已实现项标 ✅ 并附证据位置，未实现项进 workstream。
> 说明：origin/main 上第一版骨架已大量落地（`k8s.rs` 1004 行 / 51 个函数、`K8sProvider` 四方法、真集群 8 断言全绿），
> 本分解只列**剩余缺口**，不重复已完成工作。

---

## 0. 代码实况快照（origin/main @ eead794）

已落地（不再立项）：

| 项 | 证据 |
|---|---|
| `Kubectl` 封装 + 命名规范（ns/pod/pvc/secret + labels） | `k8s.rs:24-176` |
| `K8sProvider` provision（PVC+Secret+Quota+LimitRange，**不含 Pod**）/ start_member / stop / destroy（Pod→PVC→Secret→NP） | `k8s.rs:732-798` |
| `CloudProvider` trait 装配 + `provider.name()=="k8e"` 分支 | `provision.rs:797` |
| approve 分支建 Pod（裁决 V / M1） | `commands.rs:2739-2764` |
| member Pod 清单（5 容器 + PSA restricted + seccomp + fsGroup + liveness/readiness probes + 显式小规格侧车） | `k8s.rs:262-429` |
| NetworkPolicy（deny-all egress + 白名单 + `egress: []` 空白名单修正 + 封 IMDS） | `k8s.rs:680-730`、`TEAMX_K8S_EGRESS_ALLOW` |
| 真集群 8 断言脚本（kind，已全绿）+ stub 脚本 | `tests/k8e-real-cluster-test.sh` / `tests/k8e-provision-test.sh` |
| member/server Dockerfile、`scripts/dev-cluster-setup.sh`（§14 BYO 接入） | `docker/member/Dockerfile` 等 |
| 租户 server / platform-console / 证书换发（第二步内容，先行部分） | main 近期 commits |

---

## 1. Workstreams（剩余缺口，按依赖排序）

### WS-A · 回收清单 provider 维度（M2 / §9.5）——**首个 PR 切片，本任务已开工**
- **事实**：`destroy_checklist()`（`provision.rs:1107`）仍硬编码 8 项 AWS 清单（release-eip/security-group/delete-os-user 等对 k8e 无意义）；
  `K8sProvider::destroy_member()` 实际产出 4 步（pod/pvc/secret/networkpolicy），两套清单**脱节**；
  `provision.rs:2439` 测试断言 `len()>=8` 对 k8e 行为失真。
- **验收**：k8e 节点的 checklist 与 destroy_member 实际执行步骤一致（5 项，含 provider 无关的 revoke-member-cert）；AWS 路径不变；单测 + stub 测试按 provider 分支断言。

### WS-B · CNI 前置校验（M0，最高优先 / §7 + §9.7b）
- **事实**：全仓无 CNI 探测代码。flannel 默认 CNI 下 deny-all egress / 封 IMDS **静默失效**，§7 安全清单空转。
- **任务**：provision 前探测 NetworkPolicy 支持性（部署 deny-all NP → 临时 Pod 连外验证 → 回收）；不支持 → **阻断**或显式降级声明（node 记录 `security_degraded=true`，不许静默继续）。
- **验收**：§9.7b ①②（Pod 连意外外网必须失败；CNI 不支持时 provision 阻断或降级声明）。

### WS-C · degraded 上报独立于 expose（§4 / FR-10 / §9.6）
- **事实**：`nodes` 状态机已支持 degraded 迁移（`provision.rs:21,42-47`），但 k8e main 容器**无** localhost 探针→RPC 上报路径；
  docker/member 运行时无 degraded 逻辑。
- **任务**：main 容器内探针（ttyd 7681 / fs 4197 / expose 失联检测）→ 经 mTLS RPC 直报 server（不过 expose 边车）→ presence 三态落库；恢复自动回 ready。
- **验收**：§9.6（探针接入 server 侧 presence，在线状态不撒谎）+ kill 边车自愈断言（xiaods-mac #6631 三件套的 Pod 变体）。

### WS-D · 冷启动三段埋点（§9.7 / FR-15）
- **事实**：k8s.rs 无任何计时/埋点字段。
- **任务**：① t0=approve → Pod Running ② workspace 恢复（PVC 挂载 + initContainer 完成）③ agent 热身（opencode serve 就绪）；三段耗时落 node 事件（JSON 字段），计时起点 = approve。
- **验收**：三段埋点可在 node log / JSON 中逐段读到。

### WS-E · k8e 字段语义防线（§2 表）
- **事实**：`cmd_node_ssh`（`commands.rs:6675`）对 k8e 行仍拼 `ssh -i <key> ubuntu@<ip>`（keypair_ref 为空 → 输出坏命令）；§2 明确「node ssh 对 k8e 行应报错而非空指针」。
- **任务**：按 provider 分支——k8e 报明确错误（提示经 ttyd/fs 隧道）；destroy 路径确认不走 userdel/delete-keypair（WS-A 清单分支后自然达成）。
- **验收**：k8e 节点 node ssh 返回明确错误；EC2 路径不变。

### WS-F · E2E 主链与真集群补断言（§9.1–9.4 / 9.8 / 9.9）
- **事实**：`tests/node-provision-test.sh` 无 k8e 变体（主链未在 k8e 下端到端跑过 stub 之外的链路）；真集群脚本缺跨租户负例、拔节点演练、workspace 断言、隧道三件套量化。
- **任务**：
  1. 主链变体（复用 node-provision-test.sh 结构，provider=k8e + stub kubectl）
  2. 真集群补：跨租户负例 ①B 不可达 K8s API ②A 访问不到 B 的 Pod/PVC/Secret ③mTLS 只能触达本租户 server；拔节点 → Pod 重建 + PVC 恢复清单一致；`members.workspace`==PVC 挂载点；隧道三件套（liveness / idle 首字节≤3s / 10MB 100%）
- **验收**：§9 全项绿；run-all 接入。

### WS-G · 镜像与发布流程（开放项 2）
- **任务**：member 镜像 registry/tag 规范（pin + semver）、多架构构建（arm64 台式机 + amd64）、CI 发布路径。
- **验收**：镜像可从声明 registry 拉取且 tag 可追溯（manifest 记录 digest）。

### WS-H · 决策收口（提请 owner/评审拍板，不阻塞 WS-A~F）
- NFR-10 PVC 快照频率/保留期具体值（开放项 1）
- Pod 默认 requests/limits 规格（开放项 4，当前 `default_resources()` 的值需评审确认）
- setup initContainer vs `kubectl exec` bootstrap 取舍（开放项 3，真集群已走 initContainer 且修了幂等——建议定为 A 并关闭）

---

## 2. 首个可合并 PR 切片（WS-A 实施单）

| 项 | 内容 |
|---|---|
| 分支 | `fix/destroy-checklist-provider-dim`（基 origin/main） |
| 改动 1 | `provision.rs:destroy_checklist()`：按 `node.provider` 分支——`k8e|k8s|kubernetes` → `[delete-pod, delete-pvc, delete-secret, delete-networkpolicy, revoke-member-cert]`（与 `K8sProvider::destroy_member` 实际步骤对齐 + 证书吊销是 server 侧 provider 无关动作）；其余 provider 维持原 8 项 |
| 改动 2 | `provision.rs:2439` 断言改按 provider：EC2 行 `>=8` 保持；新增 k8e 行用例（精确 5 项、含 revoke-member-cert） |
| 改动 3 | `tests/k8e-provision-test.sh` T-K8 段补 checklist 结构断言（与 destroy_member 步骤一致） |
| 回归 | `cargo test -p teamx`（provision 模块）+ `tests/k8e-provision-test.sh` 全绿；不触碰 EC2 路径 |
| 依据 | design/013 §9.5 M2 原文点名（provision.rs:768-785 硬编码 8 项须带 provider 维度） |

> 偏差说明（评审请关注）：设计 §3 写「k8e 返回 Pod/PVC/Secret 三项」，现实现 destroy 多删一条 NetworkPolicy（真集群遗留物，不删则残留）。
> 分解按**实现对齐**（5 项含 NP + 证书吊销），如评审认为应严格三项，请在评审时裁决 NP 步骤归属（建议：保留，列为第 4 步）。

---

## 3. 排期与依赖

```
WS-A（本任务，PR 切片①）──┐
WS-E（小，可与 WS-A 同 PR 或紧随）──┼→ WS-F（E2E 汇总）→ §9 全项验收
WS-B（M0，安全前置）────────┤
WS-C（degraded 通道）───────┤
WS-D（埋点）───────────────┘
WS-G（镜像发布）── 独立线，不阻塞
WS-H（决策）── 提请拍板，随评审收口
```

建议后续派单：WS-B 归 M0 最高优先（安全空转风险），WS-C 需 docker/member 运行时改动（可与 xiaods-mac 真机视角联动验证）。

---

## 4. 收敛更新（owner 裁决 seq 7844 · task-97 执行 · 2026-09-22）

> 本节为 task-97 收敛结果，前文 WS-A..H 为分解基线；裁决要点与本节为准。

### 4.1 裁决落地状态

| 裁决项 | 状态 | 证据 |
|---|---|---|
| M0/G1 权威 = `feat/k8e-cni-precheck-m0` @ `354a560`（完整 probe + 降级通道 + 接线 + T-K14） | ✅ 已定 | 分支已推 GitHub origin；PR 待 owner 开/授权 |
| PR #103 剔除 M0-lite（`cni_supports_network_policy`/`TEAMX_K8S_ALLOW_NO_CNI_POLICY`），保留 M2 + G4 白名单 | ✅ 已落 | 重建分支 `task97-pr103-rebuild`（`afbdbb4`=M2 + `a0bb102`=M6 白名单，M0-lite 零残留，cargo 397P/0F），已推 origin，**PR head 切换待 owner**（或按现 #103 合并后重开） |
| 分解文档归一 | ✅ 本节 | 另两份已标 superseded（见 4.4） |
| opencode.ai 出网 | 后续切片 | 推荐 egress proxy sidecar（muse-egress 先例）；取舍在 PR 描述注明 |
| G3 命名熵 8→16 | ✅ **代码已达标，无需改** | k8s.rs:155 `short_id` `take(16)`，全部命名函数经它；单测断言 ≥16；014 §2 已同步（M5 裁决行 + N7） |
| G5/M9 Secret 存续期 | ✅ **代码已达标，无需改** | 唯一 secret 删除点 = `destroy_member()` 清单（k8s.rs:949 `delete-secret`）；stop/start/recreate 路径不触碰 Secret |
| 单一会话执行 | ✅ | task-97 由本会话承接（progress seq 7847 已声明） |

### 4.2 新增 Workstream：WS-I · G2 activate trait（P0，approve 门归一）

- **现状（eead794/354a560 实测）**：`CloudProvider` trait（provision.rs:127-141）**无 `activate` 方法**；approve 路径 commands.rs:2762 对 k8e 特判直调 `K8sProvider::start_member(team_id, member_id, server_url, repo_url, session)`——provider 抽象被绕过，违背 design/013 §5「按 trait 分支」与 task-86 M1 的落点约定。
- **任务**：① trait 增 `fn activate(&self, instance_id: &str, ctx: &ActivateCtx) -> Result<(), String>`（默认实现返回"provider 不支持"，EC2 即默认）；`ActivateCtx` 携带 server_url/repo_url/session（从 nodes 行取，落库字段核对）。② `K8sProvider::activate` 委托现 `start_member`。③ commands.rs:2762 改走 trait（按 provider 分支仅剩 EC2=k8e 均走 activate）。④ 单测：k8e approve 走 activate（stub 断言 Pod+NP apply）；EC2 路径回归不变。
- **验收映射**：§9.1 主链 approve 段；依赖：M0 合入后（354a560 含 k8e approve 分支上下文）。

### 4.3 断言补充（并入 WS-F，来自 2cfa6a8 W3/W4/W5 细化）

- W3 跨租户负例三件套细化：①B member Pod 内 `curl -k https://kubernetes.default` 必须失败（SA 无绑定）；②A 租户 Pod 访问 B 租户 Pod IP/PVC（DNS+IP 双向）必须超时（NetworkPolicy+namespace 隔离）；③跨租户 mTLS 握手必须失败（各自 CA，assert peer cert CN/链不互信）。
- W4 拔节点演练：`docker stop` 节点容器 → Pod 重建（Deployment/裸 Pod 需重拉起机制注明——第一步裸 Pod 由 approve 重建）→ PVC 重挂后 `find /workspace -type f | sort` 与基线 diff 为空。
- W5 隧道三件套落点：liveness=边车内 `nc localhost 7681/4197`；首字节=`curl -w %{time_starttransfer}` ≤3s（idle 后首连）；10MB=`dd`+sha256 对比 100%。均写入真集群脚本断言（WS-F 第 2 项）。

### 4.4 superseded 标记

- `.teamx/docs/design/013-impl-task-breakdown.md`（79a097a）→ superseded，增量（G2 spec/opencode.ai 机制 §3.2/G5 冲突标注）已并入本文 §4.2/4.3 与 WS-H。
- `.teamx/weiye-mini/plans/013-step1-impl-breakdown.md`（2cfa6a8）→ superseded，其 W1/W2（M0/埋点）与 W3/W4/W5 细化断言已并入 WS-B/WS-D/§4.3。
