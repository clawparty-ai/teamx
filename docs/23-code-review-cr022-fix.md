# 23 — Code Review 报告：CR-022 修复（c3905f5）

> 范围：commit `c3905f5`（CR-022 修复 S1/S2/S3/S4/G2/G3/G4/L1/M2），
> 覆盖 `commands.rs`、`doc_flow.rs`、`teamfile.rs`、`teamfile-test.sh` 四个文件的 144 行新增/33 行删除。
> 本轮聚焦于**修复本身的正确性与副作用**，不重审 T1–T5 原始代码。

---

## 逐项评审

### S1（High → 已修）：路径穿越防护

**改动**：`teamfile.rs` 新增 `pub fn is_safe_key_segment`；`cmd_publish_doc` 在 `doc_key` / `doc_id` 提取后立即校验。

| 项 | 结论 |
|---|---|
| 校验覆盖 | ✅ `doc_key`（来自 payload `doc` 字段）和 `doc_id`（来自 payload `id` 字段）均校验；两者在任何路径构建之前拦截 |
| 校验逻辑 | ✅ 拒绝空、`.` 前缀、`/`、`\`、控制字符——与 `is_safe_member_key` 逻辑一致（复用） |
| 副作用 | ⚠️ `is_safe_member_key` 从私有改为 `pub` 且内部委托 `is_safe_key_segment`，增加了公共 API 表面积（但逻辑无变化） |
| 遗漏 | ⚠️ `load_spec` / `meta_path` 本身不校验（纯路径构建器）；仅 `cmd_publish_doc` 入口拦截。若未来有其他代码路径调用 `load_spec`（如批量扫描），则需补充校验。当前仅 CLI 入口，可接受 |
| 实测 | ✅ TF-206 覆盖 id 路径穿越 + key 路径穿越；手动验证 `../../../tmp/...` 不泄漏文件 |

**判定**：✅ 合格。遗漏的批量扫描场景记为待观察项。

### S2（Med → 已修）：`doc.reaction` 拒绝

**改动**：`cmd_publish_doc` 入口 `if publish_type == "doc.reaction"` 返回清晰错误。

| 项 | 结论 |
|---|---|
| 位置 | ✅ 在 S4 白名单之前（先给 `doc.reaction` 明确消息，再走白名单兜底） |
| 反应发射路径 | ✅ 反应通过 `emit_json` 直接写台账，不经 `cmd_publish_doc`，不受此 guard 影响 |
| 遗漏 | ⚠️ `doc.reaction` 未列入 `KNOWN_DOC_EVENTS`，白名单自然拒绝它。S2 guard 的价值在于给出更友好的错误消息（"system notification"），而非技术上防止穿透。合理 |
| 实测 | ✅ TF-207 覆盖 |

**判定**：✅ 合格。

### S3（Low → 已修）：排除 pending 成员

**改动**：反应目标过滤新增 `&& m.state != "pending"`。

| 项 | 结论 |
|---|---|
| 语义正确性 | ✅ pending 成员未经审批，不应收到定向任务 |
| 副作用 | ⚠️ TF-201 原依赖 pending reviewer 被通知；修复方案为测试中先 `team approve` 再发事件——**更贴近真实工作流**（正确修复） |
| 实测 | ✅ TF-201 调整后通过；TF-202~208 不受影响 |

**判定**：✅ 合格。测试修复比过滤修复更重要（体现了真实审批流程）。

### S4（Low → 已修）：事件白名单

**改动**：`KNOWN_DOC_EVENTS` 静态列表 + `if !contains` 拒绝。

| 项 | 结论 |
|---|---|
| 列表完整性 | ✅ 7 个事件覆盖设计 §1 决策 2 全部声明事件（created/updated/reviewed/approved/rejected/reopened/closed） |
| `doc.closed` 分类 | ⚠️ `doc.closed` → `classify_event` → `Forward`（非 Backward）。这意味着 `doc.closed` 需要 `to` 状态且要求 `can_advance`。语义上"关闭"是向前动作（进入终态），合理。但若作者意图是"关闭 = reject 的变体"，则分类不同。当前行为与 `doc.reviewed/approved` 一致，可接受 |
| 遗漏 | ✅ `doc.reaction` 不在列表中，被 S4 自然拒绝（S2 给更友好消息） |
| 实测 | ✅ TF-208 覆盖 |

**判定**：✅ 合格。

### G2（Low → 已修）：注释死引用

**改动**：`doc_flow.rs` 模块注释中 `load_spec / save_spec` → `load_spec`（单数）。

判定：✅ 简洁正确。

### G3（Low → 已修）：`split_states` 逗号分隔

**改动**：移除 `flat_map(|s| s.split([',', '，']))`。

| 项 | 结论 |
|---|---|
| 语义正确性 | ✅ 状态链用 `->` 连接是设计约定（§2 示例 + 所有测试）；逗号作分隔符是历史误解 |
| 兼容性 | ⚠️ 若已有 TEAM.md 使用 `状态流: a, b, c`（逗号无箭头），升级后会解析为单个状态 `"a, b, c"` 而非三个状态。但此写法从未被文档推荐，且 `split_states` 注释已明确说明 |
| 实测 | ✅ TF-108 验证 `states` 解析（用 `->`），不受影响 |

**判定**：✅ 合格。若遇到兼容性问题，可考虑 warn 但不阻断。

### G4+L1（Low → 已修）：单次 `db_now` + 审计标签

**改动**：`apply_event` 新增 `actor_label` 参数；`now` 取一次复用；`MetaStep.by = actor_label`。

| 项 | 结论 |
|---|---|
| 语义正确性 | ✅ `actor_role` 用于权限，`actor_label` 用于审计——两职责分离 |
| 调用处 | ✅ `cmd_publish_doc` 构造 `"{} ({})" .format(display_name, role)`；测试中 role=label |
| `by` 格式 | ✅ `"文档驱动项目 (owner)"` — 包含角色+成员身份，可区分同角色不同人 |
| 测试兼容 | ✅ `apply_created_permissions` 断言 `history[0].by == "pm"`（label="pm"）仍通过 |
| 遗漏 | ⚠️ 若 `display_name` 为空（理论上不应发生，但防御性），`by` 格式为 `" (role)"`。可加 fallback：`if display_name.is_empty() { role } else { format!("{} ({})", ...) }`。当前 display_name 由 TEAM.md 提供，几乎不为空，可接受 |

**判定**：✅ 合格。display_name 空值为边缘场景，记为后续加固项。

### M2（Med → 已修）：原子写

**改动**：`DocMeta::save` 改为 `write(tmp)` + `rename(tmp, path)`。

| 项 | 结论 |
|---|---|
| 原子性 | ✅ `rename` 在 POSIX 上是原子操作（同文件系统），崩溃不会留下半写文件 |
| 并发安全 | ⚠️ 临时文件名是确定性的（`path.with_extension("meta.json.tmp")`），两个并发写同一 meta 会争用同一 tmp 文件。一个写完 tmp，另一个覆盖 tmp，然后 rename 时后者覆盖前者——中间结果可能丢失。但 CLI 单会话串行执行，不构成实际风险 |
| 平台 | ⚠️ `std::fs::rename` 在 Windows 上如果目标已打开会失败（但 teamx 主要跑 Linux/macOS） |
| 实测 | ✅ TF-201~205 的 meta 持久化测试隐式覆盖（写入后读回正确） |

**判定**：✅ 合格。并发 tmp 争用记为已知限制（单会话场景无风险）。

---

## 测试评审

| 测试 | 覆盖 | 结论 |
|---|---|---|
| TF-201 | 审批流程 + reactions 通知 | ✅ 修正为先 approve 再发事件，更贴近真实工作流 |
| TF-206 | S1 路径穿越（id + key） | ✅ 正向覆盖 |
| TF-207 | S2 doc.reaction 拒绝 | ✅ 正向覆盖 |
| TF-208 | S4 未知事件拒绝 | ✅ 正向覆盖 |
| cargo test 89 | 单元测试（含 `apply_event` 新签名） | ✅ 全绿 |
| smoke/cli/concurrency | 回归 | ✅ 全绿 |

**缺失测试**（非阻断，可后续补充）：
- M2 并发 tmp 争用（CLI 场景无风险，仅理论）
- L1 display_name 空值边缘

---

## 总结

| 维度 | 评分 |
|---|---|
| 修复正确性 | ✅ 9/9 项均正确实现，无逻辑回归 |
| 副作用控制 | ✅ 仅 TF-201 需调整（审批流程），无其他测试受影响 |
| 安全性 | ✅ S1 路径穿越已堵；S2/S4 收紧事件处理 |
| 代码质量 | ✅ 注释、doc-comment、命名均一致 |
| clippy | ✅ 回到基线 21（无新增），`err(&format!→format!` 修正了 4 个既存警告 |

**无新增需要修复的 issue。** 暂缓项（M1/M3/G1）已在 CR-022 标注，本轮不涉及。

建议：下一轮可考虑 (a) `display_name` 空值 fallback、(b) `load_spec`/`meta_path` 入口级校验加固（为未来批量扫描场景准备），但非必须。
