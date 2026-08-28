# 24 — Code Review 报告：DocSpec 文档生命周期体系综合评审（2026-08-28）

> 范围：`main` 分支 DocSpec 全部实现（T1–T5 + CR-022 修复），
> 重点走读 `teamfile.rs`（解析）、`doc_flow.rs`（生命周期引擎）、
> `commands.rs`（CLI 集成）、`teamfile-test.sh`（端到端测试）。
> 含 CR-022 发现的问题与修复验证。

---

## 1. 解析层（`teamfile.rs`）

### 1.1 `doc_field_name` — 字段别名匹配

设计合理：中文精确前缀、英文大小写不敏感。已验证 `"titles: data"` 不会误匹配 `"title"`（前缀匹配后无冒号分隔符 → 落空，正确 fall through）。

**残留 Low 项**：
- 别名排序依赖（如 `"创建"` vs `"创建者"`）：当前长别名在前，正确；但调换顺序会引入 bug。记为维护注意事项。
- 无重复 key 检测：两个 `### requirements` 会共存于 `tf.docs`，下游需处理。

### 1.2 `split_states` — 状态链分割

仅按 `->`/`→` 切分，逗号不再作分隔符（G3 修复）。正确。

### 1.3 `is_safe_key_segment` — 路径安全校验

pub 函数，拒绝空、`.` 前缀、`/`、`\`、控制字符。S1 修复核心。正确。

---

## 2. 生命周期引擎（`doc_flow.rs`）

### 2.1 `apply_event` — 纯函数零副作用

设计 §6.4 要求：校验失败不产生任何变更。实现正确——所有校验在 `Ok(next)` 返回前完成；`meta.clone()` 后修改 clone，失败路径不触及原 meta。

`actor_label` 参数（L1）将审计 `by` 从裸角色改为 `"显示名 (角色)"`，解决了同角色不同人不可区分的问题。

### 2.2 `validate_transition` — 动态状态机

允许向前跳步（`ti > fi`）、禁止向后（需 `backward=true`）、禁止 no-op。设计取舍已文档化（M3 暂缓）。

### 2.3 `DocMeta::save` — 原子写（M2）

临时文件 + `rename`，崩溃不留半写文件。并发场景（同一 meta 两次写）有 tmp 争用，但 CLI 串行无风险。

### 2.4 `classify_event` — 事件分类

`doc.created` → Created, `doc.updated` → Updated, `doc.rejected/reopened` → Backward, 其余 → Forward。上游白名单（S4）确保未知事件不会到达此处。

---

## 3. CLI 集成（`commands.rs`）

### 3.1 `cmd_publish_doc` — 入口防护

| 防护 | 位置 | 作用 |
|------|------|------|
| S1 `is_safe_key_segment` | L2411-2422 | 阻断 `doc_key`/`doc_id` 路径穿越 |
| S2 `doc.reaction` 拒绝 | L2423-2428 | 阻断系统通知被当生命周期事件 |
| S4 `KNOWN_DOC_EVENTS` 白名单 | L2429-2443 | 拒绝未知 `doc.*` 事件 |

### 3.2 Reactions — S3 pending 排除

`.filter(|m| m.state != "left" && m.state != "denied" && m.state != "pending")` — 仅 active/idle/waiting 成员接收定向通知。

### 3.3 审计 — L1 actor_label

`format!("{} ({})", actor.display_name, actor_role)` — 包含成员身份+角色，可区分同角色不同人。

---

## 4. 测试覆盖

| 测试集 | 覆盖 | 结论 |
|--------|------|------|
| `cargo test` 89 | T1 单元 17 + T3 `doc_flow` 9 + T4/T5 集成 8 + 其他 55 | ✅ 全绿 |
| TF-101~108 | Bootstrap 解析/letter/AGENTS/docs | ✅ 全绿 |
| TF-201 | 审批流程 + reactions 通知（修正：先 approve 再发事件） | ✅ 全绿 |
| TF-202 | 重复创建/向后转移/未知状态/缺失 key | ✅ 全绿 |
| TF-203 | 未注册 doc type 拒绝 | ✅ 全绿 |
| TF-204 | 完整 6 步生命周期 `opened→closed` | ✅ 全绿 |
| TF-205 | 多 doc type 独立性 | ✅ 全绿 |
| TF-206 | 路径穿越拒绝（S1） | ✅ 全绿 |
| TF-207 | doc.reaction 拒绝（S2） | ✅ 全绿 |
| TF-208 | 未知事件拒绝（S4） | ✅ 全绿 |
| smoke/cli/concurrency | 回归 | ✅ 全绿 |

---

## 5. 设计一致性

| 设计要求 | 实现 | 状态 |
|----------|------|------|
| §1: 通用事件命名空间 | `doc.created/updated/reviewed/approved/rejected/reopened/closed` | ✅ |
| §2: `## 文档` 章节格式 | 解析 `### key` + 字段别名 | ✅ |
| §3: 模板机制 | 解析但未强制（G1 暂缓） | ⚠️ 已知缺口 |
| §4: 声明式状态机 | `validate_transition` 动态校验 | ✅ |
| §5: 不新增 CLI 子命令 | 通过 `publish doc.*` 驱动 | ✅ |
| §6.3: 变更响应闭环 | reactions 匹配事件名 + 定向通知 | ✅ |
| §6.4: 校验失败零副作用 | `apply_event` 纯函数 | ✅ |

---

## 6. 残留项（已知、非阻断）

| ID | 严重度 | 描述 | 建议 |
|----|--------|------|------|
| M1 | Med | 文档根取 CWD，团队在 DB；多目录场景静默失败 | 需把项目根写入 teams 表 + bootstrap 改动 |
| M3 | Med | `validate_transition` 允许跳步 | 设计取舍，测试已明确；若需严格相邻再收紧 |
| G1 | Med | `模板` 字段未强制生成骨架 | 记为已知缺口，待定实现或文档化 |
| L-dup | Low | 无重复 key 检测（doc/member） | 下游可加去重或告警 |
| L-state | Low | 状态名未做 `is_safe_key_segment` 校验 | 低风险（状态名不进路径） |
| L-alias | Low | 别名排序依赖长别名在前 | 维护注意事项 |

---

## 7. 结论

DocSpec 文档生命周期体系（T1–T5 + CR-022 修复）实现完整、测试扎实、设计一致。所有 High/Med 安全问题已修复（S1 路径穿越、S2 doc.reaction、M2 原子写）。残留项均为设计取舍或低风险边界，不阻塞使用。

**无新增需要修复的 issue。**
