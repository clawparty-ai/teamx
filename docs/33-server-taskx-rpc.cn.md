# teamx server 端 taskx（任务）RPC：设计方案

> 状态：**设计（待评审）→ 实施中**
> 背景：网络模式下插件把 `task …` 转成 RPC，服务端 dispatch 无 `task.*` → `unknown rpc method 'task'`。taskx 现状是**本地文档模型**（`.teamx/docs/taskx/*.md + *.meta.json`，git 承载），未随网络模式搬上 server。本文把 taskx 对等服务化，让网络团队成员通过 server 使用任务。
> 目标读者：维护者 / 实现者

---

## 1. 目标与非目标

### 1.1 目标
1. 在 teamx server 上提供 **task 存储 + RPC**，网络模式成员可用任务（My Tasks / /team task / 插件自动 ack/claim）。
2. 语义与本地 taskx 一致：三种委派模式（direct/bid/broadcast）、同一状态机、角色/成员可见性、owner/lead 审批关闭。
3. 每次状态迁移写 **ledger 事件** 并经 hub 推送（成员实时可见），保留审计。
4. 插件与 console 在网络模式下走 `task.*` RPC；本地/项目模式继续走本地 CLI。

### 1.2 非目标（本期）
- 不做本地 taskx 文档与 server 存储的双向同步/迁移（保留两套并存：本地团队用文档，网络团队用 server taskx）。
- 不做文件内容级富文档（task body 仅作字符串存储）。
- 不做任务附件/评论。

## 2. 为什么现在才做（历史）
taskx 诞生于**本地/项目模式**：任务 = 工作区 git 文档。server/网络模式（mTLS、隧道、团队状态机）是后加的，task 命令保持“本地执行”；插件网络模式下把所有 `teamx` 调用统一走 `/rpc`，task 因此被发成不存在的 RPC 方法。属历史设计缺口。

## 3. 数据模型（SQLite 新增表 `taskx`）

```sql
CREATE TABLE IF NOT EXISTS taskx (
  id           TEXT PRIMARY KEY,
  team_id      TEXT NOT NULL,
  title        TEXT NOT NULL,
  body         TEXT,
  state        TEXT NOT NULL DEFAULT 'assigned',   -- assigned/acked/claimed/in_progress/done/verified
  mode         TEXT NOT NULL DEFAULT 'direct',      -- direct/bid/broadcast
  executor     TEXT NOT NULL DEFAULT 'either',      -- either/agent/human
  priority     TEXT NOT NULL DEFAULT 'medium',      -- high/medium/low
  assignee_member_id TEXT,                          -- direct: set at create; bid: set on claim
  role_key     TEXT,                                -- bid/broadcast 目标角色
  created_by_member_id TEXT NOT NULL,
  created_at   TEXT NOT NULL,
  updated_at   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_taskx_team  ON taskx(team_id);
CREATE INDEX IF NOT EXISTS idx_taskx_assign ON taskx(assignee_member_id);
```

### 语义（对齐本地）
- **mine**：`assignee_member_id == 我的 member`，或（未认领时）`role_key` 等于我的角色。
- **可见性**：bid/broadcast 按角色广播，同角色成员可见可抢（claim 先到先得写 assignee）。
- **状态机**：`assigned -> acked -> claimed -> in_progress -> done -> verified`；
  `claimed` 可跳过（小任务直接进 in_progress）；`update` 不改状态；
  `help_requested` 保持 in_progress 并通知 lead；`rejected`（lead）退回 in_progress。
- **授权**：create/verify/reject 需 lead；ack/claim 仅 assignee（或角色成员抢 bid）；done 由执行方；他人只读。

## 4. RPC 方法（serve.rs dispatch 扩展）

| 方法 | 入参（args） | 说明 |
|---|---|---|
| `task.list` | mine/state/assignee/executor | 列任务（含我的 role 匹配） |
| `task.create` | title, body?, assignee?, role?, mode?, executor?, priority?, id?, detail? | lead only；direct/bid/broadcast 同本地解析 |
| `task.ack` | id | assigned→acked（可跳过） |
| `task.claim` | id | bid 抢单：assignee 空且我的 role 匹配时写入 |
| `task.update` | id, progress | 记进度；assigned/acked/claimed→in_progress |
| `task.help` | id, reason | in_progress + help_requested 通知 |
| `task.done` | id, result | → done（候选） |
| `task.verify` | id | lead → verified |
| `task.reject` | id, reason | lead 退回 in_progress |
| `task.log` | id | 返回该任务审计事件流 |

实现路径：新增 `crates/teamx/src/server_tasks.rs`（服务端任务逻辑 + 单测），`serve.rs` dispatch 增加分支，沿用 `emit_json`（事件类型 `doc.created/acknowledged/claimed/updated/done/verified/help_requested/rejected`，payload 含 `doc:taskx`、task id、title、progress/result 等）→ hub 推送。

> 人员判定：沿用网络模式身份 = 证书 CN → `member_id`（`pki::parse_member_cn`）；`team` 缺省单团队时自动解析（同 tunnels/git）。

## 5. 插件 / console 接线

1. `opencode-plugin/src/client.ts` `GROUPED` 增加 `"task"`：
   - argv `task list --mine` → method `task.list`, args `{mine:true}`；
   - slots 补齐：`task.list`、`task.create`（title/assignee/role/mode/executor/priority/detail…）、`task.ack`/`claim`/`update`/`help`/`done`/`verify`/`reject`/`log` 的首个位置参数映射。
2. console `console_data.rs`：
   - My Tasks 面板：**网络模式改走 `task.list` RPC**（当前是本地 CLI，网络团队里为空）；项目模式保持本地 CLI。
3. 命令/agent 帮助无需大改（/team task* 已存在，语义不变）。

## 6. 迁移与测试

- `db.rs` migrate 追加建表（幂等）。
- 单测（server_tasks）：create/list(mine/role)、ack→claim→update→done→verify 全链路、bid 抢单互斥、越权(非 lead create / 非 assignee ack) 拒绝、事件落账。
- 回归：`cargo test -p teamx` 全绿；CI build（win/linux-arm64/macos）与 clippy 零新增。

## 7. 里程碑

1. **Step 1**：`taskx` 表迁移 + `task.list/create` + `task.ack/claim`（主链路：lead 派活→成员看到→ack/抢单）
2. **Step 2**：`update/help/done/verify/reject/log` + broadcast/bid 完整语义 + 事件/hub 推送
3. **Step 3**：插件 GROUPED/slots 接线 + console My Tasks 网络模式 RPC → 端到端验证（东京/首尔/美国 worker）

## 8. 关联
- `crates/teamx/src/commands.rs::cmd_task`（本地语义参照）、`doc_flow.rs`（状态机/事件名）
- `31-member-console-requirements.cn.md` / `32-member-console-design.cn.md`（My Tasks 面板）
- `opencode-plugin/src/client.ts`（cliArgsToRpc GROUPED/slots）
