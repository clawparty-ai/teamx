# Review: task-101 三对象（#103 修订 a0bb102 / M0 354a560 / G2 49264d1）

- reviewer: weiye（co-lead，架构/评审；三次改派后由本会话执行，用户指令"运行"）
- date: 2026-09-23
- verdict: **三对象全部 PASS**（判据见各节；非阻塞建议 4 条）
- 验证方式：三提交分别**真实检出实跑**（非采信作者声明）——单测逐态全绿 + k8e stub E2E ALL PASSED + 互斥残留零命中

## A) a0bb102 —— M6 默认出网白名单基线（PR #103 修订）→ **PASS**

- `DEFAULT_EGRESS_ALLOWLIST` 回落语义正确：未设 env → 基线（非空）；显式空 env → `egress: []`（deny-all 保持可用）。
- **IMDS 永不在白名单**有单测断言 ✅。
- 与 M0 分支的互斥物（`cni_supports_network_policy` / `ALLOW_NO_CNI_POLICY`）**零残留**（tree grep 确认）——owner 裁决 seq 7844 的收敛执行到位。
- k8s 单测 10P/0F（本机实跑）。

**建议（非阻塞）**：基线含全部 RFC1918（10/8、172.16/12、192.168/16）——集群 Pod CIDR 通常落在 10/8 内，意味着**跨租户 Pod IP 在网络层默认可达**（NetworkPolicy 的 ipBlock 按 CIDR 放行，不看 namespace）。不阻塞（租户间主隔离在 CA/serve 层），但建议：① 部署侧用 `TEAMX_K8S_EGRESS_ALLOW` 收窄到实际 server ClusterIP/网段；② 文档如实声明"默认基线下跨租户 Pod 网络可达，强隔离档需收窄"（与 D17 诚实声明口径一致）。

## B) 354a560 —— M0 CNI NetworkPolicy 前置校验 → **PASS**

- **探测语义正确**：一次性 probe namespace + deny-all egress NP + one-shot probe Pod（`nc` 连外）→ `Failed=Enforced / Succeeded=NotEnforced / 其他=Unknown(阻断)`。这正是 M0 要的"把安全控制生效变成可证伪断言"。
- **显式降级通道**：`TEAMX_K8S_CNI_DECLARED=<cni>` 跳过探测但 node note **强制带降级声明**（不许静默继续）✅（D17 口径）。
- 失败路径走既有 `fail_job` + node failed（不新增状态节点，幂等键不变）✅；result JSON 带 `cni` 字段 ✅。
- 清理尽力而为；单测 3 条（终态判定/manifest 含显式空 `egress: []`/声明过滤）+ stub E2E T-K10 ✅；real-cluster 脚本显式声明 kindnet ✅。
- 本机实跑：k8s 11P + provision 24P；**k8e stub E2E ALL PASSED**。
- **IMDS 不作探测目标**有断言（避免探测行为本身触碰元数据面）✅。

**非阻塞 2 点**：① probe Pod 带 `teamx.io/managed=true` 标签——`list()` 若跨 namespace 按该标签列 Pod，probe 存续的几秒会瞬时污染节点列表（自清理，影响极小；建议 list 排除 `purpose=cni-probe`）；② 默认 30s probe 超时在冷集群（首次拉 busybox）可能偏紧 → Unknown → 阻断——这是 **fail-closed 的正确选择**（env 可调），仅提示运维知晓。

## G2) 49264d1 —— approve 门归一（`CloudProvider::activate` + `ActivateCtx`）→ **PASS**

- 正好闭环我 design/013 评审的 **M1**：approve 门移除 k8e 内联特判，统一走 trait `activate()`；AWS/EC2 默认 Err → 回落既有 SSH/systemd 路径，**行为逐字不变**（代码与守卫逐字保留）。
- 不对称告警正确：k8e activate 失败可见（Pod 未建须告警），AWS 默认 Err 静默回落 ✅。
- 幂等键/裁决 V/状态机语义不变；`K8sProvider::activate` 委托 `start_member`（同一幂等键）✅。
- 单测 3 条（AWS 显式不支持 / FakeProvider 回落 / k8e trait 接线防漏实现）+ 本机实跑 k8s 8P / provision 27P / approve 5P ✅。

**非阻塞 1 点**：`if let Ok(p) = provider_for(...)` —— 若 provider_for 对 k8e 节点意外失败，会**静默**落回 systemd 路径（无 k8e 告警）。实际概率极低（provider 名由同一代码写入 node），但建议加一行"provider_for 失败且 provider=k8e → 告警"，与 k8e activate 失败的可见性对齐。

## 交叉核验

- 三提交互不包含（A 在 #103 线、B/G2 同基 main HEAD `eead794`）——B 与 G2 都改 `k8s.rs`/`provision.rs`，合并顺序由 owner 定，预期冲突面小（不同 impl 块/区域）。
- 互斥残留零命中 ✅（owner 裁决 seq 7844 的收敛彻底）。
- 本机实跑记录：B=k8s 11P + provision 24P；G2=k8s 8P + provision 27P + approve 5P；A=k8s 10P；E2E ALL PASSED（354a560，含 014 的 T-K10~K13）。

## 结论

**三对象 PASS，可进 owner 合并队列**。合并建议顺序：B（M0 前置校验，安全基座）→ G2（approve 门归一）→ A（M6 基线）；A 与 B 都改 `k8s.rs`，后合者需 rebase（冲突面预计很小）。4 条非阻塞建议已列，不阻塞合并。

—— weiye（co-lead · 架构/评审，task-101 三次改派后执行）
