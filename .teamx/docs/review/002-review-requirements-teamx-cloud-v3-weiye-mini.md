# Review: requirements/002 v3 闭环验证（weiye-mini）

- reviewer: weiye-mini（非作者；v3 作者 caicai/owner 裁决）
- target: `.teamx/docs/requirements/002-teamx-cloud.md` @ dcb5d84（v3，state=review）
- task: task-89（复核 requirements/002 v3 闭环）
- date: 2026-09-21
- verdict: **CLOSED**（v3 的 4 必改 + 2 缺失 + 4 建议全闭环，需求可定稿）

## 闭环核对（逐项）

### 我的 4 必改（v1 评审 @ a0e804e 提出的）在 v3 全部落实 ✅

1. **FR-11 scale-to-zero × FR-5 异步 provision 交互** ✅ — v3 定义：恢复走同一条异步 provision 路径（FR-5 子集，跳过 workspace 恢复段），状态语义补 stopped→provisioning，不阻塞派任务方
2. **FR-19/NFR-12 join token user 绑定** ✅ — v3 token 验证链挂 user_id + 预期 member_id（一 token 一设备），与 D8 users.id「人」锚统一
3. **NFR-3 两类 Pod 爆炸半径** ✅ — member Pod（可驱逐）vs 平台 Pod（高优先+PDB）分别声明 + RTO 分别有数（v2 已落实，v3 保持）
4. **§7.11 tx-expose 故障注入验收** ✅ — kill 边车→≤90s degraded→自动重注册（v2 已落实，v3 保持）

### 2 缺失需求 ✅

- **FR-11 拉起触发源** ✅ — 定义为仅任务指派触发（讨论 @ 不触发，与唤醒纪律一致）
- **NFR-5 不撒谎量化** ✅ — 探针失联 ≥2 周期即翻转（数字进 NFR-5）

### 4 建议（非阻塞）✅

验收 1 断言集明确 / FR-16 平台自身 bootstrap 语义澄清 / NFR-1 allowlist 初始集（v2 已有）/ §4.4 对齐 FR-5 量化——均落实。

### 附带发现并修复（数据事故）

caicai push dcb5d84（002 v3）时未 pull，force 覆盖丢了我的 design/013 v3 修复（1be1cd7，M0-M12 全覆盖）——**已 cherry-pick 恢复为 8aaa2b6**（dcb5d84..8aaa2b6 fast-forward），grep 验证 CNI 前置校验/activate(id)/opencode.ai 均在远端。流程建议：文档类任务 push 前必须 pull --rebase（与 Issue #20 会话级租约并列的流程风险）。

## 结论

**CLOSED** — requirements/002 v3（dcb5d84 + 013 修复恢复 8aaa2b6）可定稿。v1→v2→v3 全链闭环：三路评审 14 必改 + 6 缺失 → v2 修订 → 三路闭环验证 → v3 修正 → 本次闭环验证。需求可进设计实现（design/013 v3 已同步就绪）。
