# taskx 委派模式 Code Review（2026-09-02）

> 范围：taskx bid/broadcast 委派实现（commit `2a4d5b8` ~ `0b607ac`）
> 状态：**发现已修复**（commit `4fb0844`）
> 审查方式：深度代码走查 + 交叉验证 agent + 端到端漏洞复现

---

## 一、审查范围

| 文件 | 关注点 |
|---|---|
| `crates/teamx/src/commands.rs` | `cmd_task`、`task_claim`、`task_retract`、`task_rebid`、`broadcast_role_rebid`、`cmd_publish_doc`（effective_role 授权） |
| `crates/teamx/src/doc_flow.rs` | `builtin_taskx_spec`、`classify_event`（doc.retracted）、`apply_event` |
| `opencode-plugin/src/index.ts` | bid 自动抢单、digest 任务注入 |

## 二、发现的 7 个问题

### HIGH

**H1 — 权限越权：同角色成员可操作他人任务**
`commands.rs` 的 `effective_role` 中 `role_match` 只检查 `assignee_role` 与 `actor.role` 匹配，**不检查 assign_mode 是否为 bid、不检查 actor 是否是 assignee、不检查事件类型**。由于 direct/broadcast 任务也写入 `assignee_role`，任何该角色成员都能对其他成员的 direct/broadcast/bid 任务执行 `done`/`verify`/`retract`。
- **复现**：broadcast 任务，成员 B（同角色）对 A 的实例执行 `task done` —— 成功（越权）。
- **修复**：`role_match` 限定为 `bid 模式 && assignee 为空 && publish_type == doc.claimed && 角色匹配`。仅"抢单"这一动作允许角色成员执行，其余事件要求 assignee 或 lead。

**H2 — claim TOCTOU 竞态**
`task_claim` 先读 meta（校验 `state==assigned`、`assignee.is_none()`），再调 `cmd_publish_doc` 写入。校验与写入之间无锁/无事务包裹，两个进程（成员会话）并发抢单可能都通过校验，后写者覆盖前写者。
- **修复**：新增 `with_task_lock`（`flock` 跨进程排他锁）包裹读-校验-写；Windows 降级为无锁（DB 事务仍串行化 ledger 写入）。

**H3 — plugin 自动抢单是死代码**
server 在 bid 创建/退单时发 `"assignee_member_id": ""`，但 plugin 判断 `== null`，`"" == null` 为 false，导致 `bidOpen` 过滤恒为空——**fresh bid 任务和退单任务从未被自动抢单**。
- **复现**：创建 bid 任务后，同角色 agent 会话不自动 claim。
- **修复**：server 端空 assignee 时从事件 payload 移除该字段，plugin 的 `== null` 判断命中。

### MEDIUM

**M1 — `task update` / `task help` 无权限门**
`doc.updated` 分支跳过 `can_advance`，`doc.help_requested` 完全绕过状态机。任何成员都能往他人任务写进展、或对 lead 刷求助通知。
- **修复**：help_requested 要求 assignee 或 lead。

**M3 — rebid 定向广播导致误执行**
`broadcast_role_rebid` 给每个角色成员发带 `assignee_member_id` 的 rebid 事件，plugin 的 `shouldAutoExecute` 会唤醒**所有**成员去执行同一开放任务（并发抢 + 浪费）。
- **修复**：改为**单条无定向** rebid 事件，只触发自动抢单路径，不触发 auto-execute。

**M4 — git commit 静默失败**
`auto_git_commit` 丢弃所有 git 退出码；若 commit 失败（非 git 仓库/冲突），任务变更不会进共享仓库，成员看不到。
- **修复**：捕获 commit 状态并 `eprintln` 警告（保持 best-effort）。

### LOW

**L1 — broadcast 实例 id 碰撞**
实例 id 用 member id 前 8 字符作后缀，两个成员 UUID 前 8 位相同（32-bit 生日碰撞）时 id 冲突。
- **修复**：改用完整 member id（UUID 文件系统安全）。

**L3 — verify 可从任意状态**
forward 跳转允许 `assigned → verified` 直接跳，lead 可验证从未 done 的任务。
- **修复**：`doc.verified` 要求前置状态为 `done`。

## 三、未修改项（设计如此）

- **L2 — reject 保留 assignee**：打回语义是"同一人重做"，保留 assignee 合理，不改。

## 四、验证

| 项 | 结果 |
|---|---|
| 全量单测 | 106 通过 |
| H1 端到端 | 同角色成员 done 他人实例被拒 ✅ |
| H3 端到端 | bid 创建事件 payload 无 assignee_member_id ✅ |
| L3 端到端 | verify 非 done 状态被拒 ✅ |
| M3 端到端 | rebid 单条无定向事件 ✅ |
| Windows 编译 | 通过（with_task_lock cfg 拆分）✅ |

## 五、遗留建议（未在本轮处理）

- `effective_role` 对覆盖 `_spec/taskx.json` 的团队：assignee 授权依赖内置 spec 的 `"lead"` approver，若团队覆盖 spec 改变 owner/approver，assignee 可能失去推进权。建议文档化或动态解析 spec。
- `myMemberId` 使用 `data?.teams?.[0]`，多团队成员可能漏任务；建议遍历所有 teams。
