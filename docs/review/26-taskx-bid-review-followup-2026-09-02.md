# taskx 委派模式 Code Review 复查（2026-09-02，第二轮）

> 范围：commit `a5fc92a`（executor 三值）→ `0b607ac`（direct/bid/broadcast 三种委派）→
> `4fb0844`（上轮 review 修复，声称已修 7 项）→ 最新 docs（`63dafd0`/`4a48093`/`edb5d9d`）。
> 上一轮结论见 `docs/review/25-taskx-bid-code-review-2026-09-02.md`。
> 状态：~~上轮声称"已修复"的至少 1 项 HIGH 回归仍在~~ → **全部问题已修复并回归验证**
> （见文末"修复记录"，对应本轮提交）。

## 审查方式

- 逐行 diff：`git show a5fc92a / 0b607ac / 4fb0844`，重点 `crates/teamx/src/commands.rs`。
- 完整上下文：`commands.rs` taskx 相关函数、`doc_flow.rs` 权限模型、`opencode-plugin/src/index.ts` 自动执行路径。
- **端到端复现：使用 `cargo build` 重建的当前源码二进制（v0.3.0）**，在全新临时 team/DB/git 仓库中跑完整
  claim → retract → re-claim → re-bid → done → verify 流程。

> ⚠️ 关键教训：review 记录 `25` 中的"端到端确认 retract 清空 ✅"与本轮实测**矛盾**，
> 怀疑上轮验证使用了旧版二进制或只检查了事件 payload 而未检查 `.meta.json`。

---

## 一、CRITICAL-1：`task retract` 后 assignee 未清空 → 退单后任务死锁（回归，上轮声称已修复）

| | |
|---|---|
| 位置 | `commands.rs:3764-3772`（4fb0844 新增的 H3 strip 逻辑）与 `commands.rs:3788-3796`（retract 清空持久化）冲突 |
| 引入 | `4fb0844`（上轮"修复"提交本身引入的回归） |

### 根因

- 修复前（`0b607ac`）：retract 的 payload 带 `assignee_member_id: ""`，事件原样发出；持久化阶段
  `payload.get("assignee_member_id")` 读到 `""` → `persisted.assignee = None`（清空正常）。
- 修复后（`4fb0844`）：为修 H3（plugin 判断 `assignee_member_id == null` 永不命中），在 emit 前新增
  "空字符串就从 payload **移除该键**"。但 **strip 与 retract 清空读的是同一个 `payload` 变量**，且
  strip 先于持久化执行 → 持久化时键已不存在 → `persisted.assignee` 保留旧值。

### 实测复现（v0.3.0 二进制）

```
create bid 任务 → claim(s-ui-1) → meta: state=claimed, assignee=m-ui-1
retract(s-ui-1)  → meta: state=assigned, assignee=【m-ui-1 残留!】
re-claim(s-ui-2) → "task is already claimed by member m-ui-1"   ❌
re-bid(owner)    → "task is not open for bidding (state `assigned`)" ❌（assignee.is_some()）
```

任务进入**死锁**：原抢单人已退单、其他角色成员抢不了、lead 也 re-bid 不了。`docs/27-taskx` 的
"退单后任何成员可再抢"承诺失效。

代码注释还错误宣称 "The empty string is still meaningful for meta persistence (clears the claim)
— handled separately below"——"下面处理"时键已被删。

### 建议修复

- strip 只作用于**发出事件用的 payload 副本**，不动持久化读取的原始 payload；或
- 清空 assignee 由事件类型 + 目标状态直接推导（如 `doc.retracted` → assignee=None），不依赖 payload 残留字段；
- 补一条端到端单测：claim → retract → 再 claim 必须成功。

---

## 二、HIGH-2：assignee 可以自我 verify（绕过 lead 验收闭环）

| | |
|---|---|
| 位置 | `commands.rs:3694-3701` effective_role 授予逻辑 |
| 矛盾 | CLI help 明示 `verify — team lead`；`docs/27-taskx` 闭环 = "成员 done → **lead** verify" |

- `effective_role` 中 `meta.assignee == actor.id` 即授予 `"lead"`，而 builtin taskx spec 的
  approvers = `["lead", "owner"]` → assignee 获得**全套 lead 权力**，包括 `verify` 自己的任务。
- 上轮 L3 只堵了"非 done 状态不能 verify"，未堵"谁能 verify"。

### 实测复现

assignee 对 broadcast 实例 `done` 后直接 `verify` 自己的任务 → `state=verified` 成功
（事件 `doc.verified`，`by=ui-dev`）。lead 验收环节被完全跳过。

### 建议

- 若意图是 assignee 自主闭环小任务：在文档/CLI 明示并放开设计；
- 否则 `verify`/`reject` 应要求 `is_lead_actor`，且"assignee 替身 lead"应限定事件集合
  （ack/claim/update/done），不授予 verify。

---

## 三、MEDIUM-3：`task update`（doc.updated）越权仍在（上轮 M1 只修了一半）

| | |
|---|---|
| 位置 | `commands.rs:3705-3724` |
| 现状 | 只对 `help_requested` 加了"assignee 或 lead"门；`doc.updated` 走 `apply_event` 的 `Updated` 分支，**任何成员可更新任何任务** |

### 实测复现

`m-ui-2` 成功 `update` 了 `m-ui-1` 的 broadcast 实例（`"ok": true`）——同角色非 assignee 可往他人任务写进展 / 刷审计历史。

### 影响与建议

- 影响比 help 小（不改状态），但会污染他人任务的 meta 历史与 `updated_at`。
- 若认为"任何人可补充进展"是有意设计，请显式说明；否则补 `doc.updated` 的 assignee/lead 门。

---

## 四、LOW-4：广播实例 id 用完整 member id 作后缀（上轮 L1 修复）的边界未覆盖

- `format!("{base_id}@{mid}")` 现在把完整 UUID 拼进 doc id / 文件名，`@` + 36 字符 uuid 令文件名很长；
- 后续 digest / `task list --mine` / 自动 ack 匹配都依赖 member id 精确一致，但**没有测试锁定
  超长 id 与 `@` 在文件名、日志、plugin 匹配中的边界**；
- 建议加一个单测锁定广播实例 id 的全链路（create → list → done）。

---

## 五、LOW-5：`with_task_lock` 细节

- 锁文件命名：`meta_path.with_extension("meta.lock")`，meta 为 `<id>.meta.json` → 替换出
  **`<id>.meta.meta.lock`**（重复 `.meta`），实测确认。建议改为直接拼接 `<id>.meta.json.lock`。
- Windows（`#[cfg(not(unix))]`）降级为无锁，注释称"DB 事务串行化 ledger"——但 TOCTOU 根源是
  "读 meta 文件 → 校验 → 写 DB"跨文件系统无原子性，Windows 上并发 claim 仍可能双赢。文档应如实说明平台差异。
- flock 只覆盖 `task_claim`；绕过它的直发 `doc.claimed`（手动 `publish doc.*`）路径不受锁保护，
  当前 plugin 抢单走 `task claim`（已锁）可接受，建议注释说明。

---

## 六、确认修复有效的部分

| 项 | 验证 |
|---|---|
| H1 done/verify 越权 | 同角色成员 done 他人 broadcast 实例被拒 ✅（`role ui-dev may not advance`） |
| H2 flock | 两进程并发抢单仅一方成功 ✅ |
| H3 事件 payload | bid 的 `doc.created` / `doc.retracted` 事件中无 `assignee_member_id` 键 ✅ |
| M3 rebid | 单条无定向 `doc.rebid`，无 assignee_member_id ✅ |
| M4 git commit | commit 失败会 eprintln 警告 ✅ |
| L3 verify 前置状态 | lead 在 `claimed` 状态 verify 被拒（仅 `done` 可 verify）✅ |
| M1 help 权限门 | 非 assignee 对他人任务 help 被拒 ✅ |
| 单测 | `cargo test -p teamx`：106 passed（doc_flow 状态机层）✅ |

---

## 七、验证命令

```bash
cargo build -p teamx              # 重建当前源码（勿用 ~/.local/bin 的旧版 teamx）
cargo test -p teamx               # 106 passed
# E2E：team create → 注入同角色成员 → task create(bid/broadcast)
#     → claim → retract → 检查 .teamx/docs/taskx/<id>.meta.json 的 assignee
```

---

## 八、修复优先级建议

1. **CRITICAL-1（先修）**：strip 只影响发出的事件，清空 assignee 由 `doc.retracted` 事件语义推导；补端到端单测。
2. **HIGH-2**：与需求确认 verify/reject 是否仅限真正 lead；若是，`effective_role` 的 lead 替身限定事件集合。
3. **MEDIUM-3**：决定 `doc.updated` 是否需要 assignee/lead 门。
4. LOW 项随后清理。

## 九、遗留建议（上轮未处理 + 本轮新增）

- review 记录 `25` 的验证方式存疑：建议固化"验证前必须 `cargo build` 重建二进制"的流程，避免旧二进制误判。
- `effective_role` 对覆盖 `_spec/taskx.json` 的团队：assignee 授权依赖内置 spec 的 `"lead"` approver，
  团队覆盖 spec 改变 owner/approver 时 assignee 可能失去推进权——建议文档化或动态解析 spec。
- plugin `myMemberId` 使用 `data?.teams?.[0]`，多团队成员可能漏任务；建议遍历所有 teams。
- 单测仅覆盖 doc_flow 状态机层，**没有任何命令层端到端测试覆盖 claim→retract→re-claim / self-verify 防线**，
  这是 CRITICAL-1 与 HIGH-2 漏网的主因。

---

## 十、修复记录（2026-09-02）

以下问题已全部修复并回归验证：

| 问题 | 修复 | 验证 |
|---|---|---|
| CRITICAL-1 retract 后 assignee 未清空 | `cmd_publish_doc`：H3 的 strip 只作用于**发出事件的副本**（`event_payload`），持久化仍读原始 payload 的空字符串来清空 assignee | 新增 E2E 测试 `taskx_bid_retract_clears_assignee_for_reclaim` + CLI 实测 retract 后 assignee=None、他人可 re-claim |
| HIGH-2 assignee 自我 verify | taskx 命令层门：`doc.verified`/`doc.rejected` 要求 `is_lead_actor`（owner 或 co-lead）；assignee 的 lead 替身不扩展到验收/打回 | 新增 E2E 测试 `taskx_assignee_cannot_self_verify` + CLI 实测 assignee verify 被拒、lead verify 通过 |
| MEDIUM-3 `task update` 越权 | taskx 命令层门：`doc.updated` 要求 assignee 或 lead | 新增 E2E 测试 `taskx_update_requires_assignee_or_lead` |
| LOW-4 广播完整 member id 后缀边界 | 补 create→done 全链路单测（含超长 id、`@` 匹配） | `taskx_broadcast_uses_full_member_id_instances` |
| LOW-5 `with_task_lock` 锁文件 | 锁文件移到系统临时目录（FNV-1a 确定性命名），不再污染团队 git 仓库；修正 `.meta.meta.lock` 双后缀；补充 Windows 无 flock 的真实 TOCTOU 说明 | `task_lock_path_is_temp_and_deterministic` + CLI 实测项目目录无 lock 文件残留 |
| 遗留：spec 覆盖时 assignee 授权 | 新增 `doc_flow::assignee_advance_role`：优先 `lead`，被覆盖则回退到 approvers 首个 / owner，始终返回 `can_advance` 接受的角色 | `assignee_advance_role_follows_the_actual_spec` 单测 |
| 遗留：plugin 多团队 myMemberId | `myMemberIds` + `myTeamForEvent`（按事件 `team_id` 定位团队），auto-ack / bid 抢单 / auto-execute / `assignedToMe` 全部团队感知；保留 `myMemberId` 向后兼容导出 | plugin 单测新增多团队用例（beta 团队任务不被 teams[0] 遮蔽），`bunx tsc --noEmit` + `bun run build` 通过 |
| 补命令层 E2E 防线 | tests 模块新增 `with_task_cwd`（全局锁 + 临时 git 仓库），串行化 cwd 依赖 | `cargo test -p teamx` 112 passed |

**回归验证**（`cargo build` 重建后 CLI 实测）：claim→retract→re-claim ✅；assignee self-verify 被拒且 lead verify 闭环 ✅；
bid/retract 事件 payload 无 `assignee_member_id`（H3）✅；并发抢单单赢（H2 flock）✅；`cargo clippy -p teamx` 无新增警告。

