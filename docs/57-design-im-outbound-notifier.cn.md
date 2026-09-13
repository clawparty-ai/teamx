# 设计 · IM 出站通知器（im-p1-outbound-notifier）

> 关联：`docs/design/im-access-requirements.md` §5.4/§7/§8（出站通知、幂等/审计、刷屏治理）
> 前置：PR #36（IM 模块骨架：`ImProvider` seam + feishu 原语 + `ImConfig` dry-run 默认开）
> 后续：`im-p1-readonly-query`（依赖 PR #39 `resolve_member`）、`im-p1-inbound-gateway-security`（P2 写动作门槛）

## 1. 定位与范围

Notifier 是 IM 出站通道（台账事件 → IM），P1 **内置 serve**（设计 §5.4 允许），按可拆进程写——核心逻辑全部在 `im/notifier.rs`，serve 只负责周期驱动。

- ✅ 交付：ledger 收集器（封闭枚举白名单 + 心跳过滤）、`im_outbox` 幂等队列、60s digest 聚合、管理/求助类逐条必达、429 指数退避、`im.push` / `im.push_failed` 审计、feishu live transport（tenant token 缓存 + `im/v1/messages`）。
- ❌ 不做：入站网关/验签（P2）、per-member 私聊路由（依赖 PR #39 绑定表，readonly-query 阶段接入）、静默时段/订阅（P3）、模板渲染（与 Issue #15 同源，P3）。

## 2. 架构

```
ledger(events) ──collect──> im_outbox ──flush──> ImProvider(feishu) ──> IM 群
     ▲             (游标 im_state)   │                ▲
     │                               ├─ direct: 逐条立即推     │
     │                               └─ pending: (team,event_type) 组
     │                                            60s 窗口 or 满 20 条
     └── im.push / im.push_failed 审计事件（im.* 不在白名单 → 无回环）
```

- `im/notifier.rs`：`Notifier::run_once(conn, now)` = collect + flush，一次 DB 独占调用，无锁无 channel；`NotifyStats` 供日志/测试。
- `serve.rs::im_notifier_task`：tokio 周期任务（默认 15s，`TEAMX_IM_NOTIFY_INTERVAL_SECS`），DB 操作走 `spawn_blocking`，**IM 平台故障永不阻塞/不终止 server**。
- `im/providers/feishu.rs`：dry-run（默认）只打日志；live 发送取 `tenant_access_token`（内存缓存，过期前 5min 刷新）后 POST `im/v1/messages`；HTTP 429 映射为含 `429` 的错误串，供退避逻辑识别。

## 3. 数据模型（收敛式，**不占 user_version**）

```sql
CREATE TABLE IF NOT EXISTS im_outbox (
  team_id TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
  seq     INTEGER NOT NULL,          -- 台账 seq（幂等键的一半）
  event_type TEXT NOT NULL,
  payload_json TEXT,                 -- 收集时快照，flush 无需回联 events
  direct  INTEGER NOT NULL DEFAULT 0,
  state   TEXT NOT NULL DEFAULT 'pending',  -- pending | direct | pushed | failed
  attempts INTEGER NOT NULL DEFAULT 0,
  last_error TEXT,
  created_at TEXT NOT NULL,
  pushed_at  TEXT,
  PRIMARY KEY (team_id, seq)
);
CREATE TABLE IF NOT EXISTS im_state (key TEXT PRIMARY KEY, value TEXT NOT NULL);
-- im_state 键：notifier_cursor（收集游标=events.id）、im_push_backoff_until、im_push_429_streak
```

**决策：不挂 user_version**（沿用 Issue #13/#40 的口径）——两张表均为全新表、无历史数据，`CREATE TABLE IF NOT EXISTS` 形状自检天然幂等；且与并行分支（PR #39 的 schema v17）零冲突，避免公共/私有产品线版本号重叠的老坑。

## 4. 事件白名单（封闭枚举）

**digest 聚合类**（信息性，60s 窗口 / 满 20 条提前 flush，key=(team, event_type)）：
`progress.published`、`decision.broadcast`、`doc.created/claimed/done/updated/acknowledged/verified/rejected`、`goal.state_changed`、`member.joined/left/state_changed`、`system.nudge`、`system.nudge_lead`。

**direct 逐条必达类**：`doc.help_requested`、`task.nudge`。

**永不进白名单**：
- 心跳类：`progress.published` 且 payload `kind == "session.idle"`（设计 §5.5：心跳类永不进枚举）；
- 未列入枚举的一切（`team.*`、`role.*`、`im.*`、`member.promoted_lead` 等）——扩展 = 代码变更 + 评审（治理冻结）；`im.*` 排除同时保证审计事件不回环。

## 5. 可靠性语义

| 场景 | 行为 |
|---|---|
| 收集幂等 | 游标持久化 `im_state.notifier_cursor`（events.id 单调）；`PRIMARY KEY(team_id,seq)` 兜底，重启重放零重复 |
| 发送成功 | 行 → `pushed`；同事务写 `im.push`（`seqs` 覆盖范围 + `digest` 标志 + `channels` + `dry_run`） |
| 429 限速 | 行保持待发；指数退避 60s×2ⁿ（上限 15min）写入 `im_push_backoff_until`；**不计入失败上限**；streak 成功后清零 |
| 其它错误 | `attempts+1`；达 3 次 → 行 `failed` + 写 `im.push_failed`（reason + retry_count）；否则留待重试 |
| 未配置群 | `TEAMX_IM_NOTIFY_GROUPS` 为空 → 通知器不启动（serve 不 spawn 任务） |

## 6. 配置（全部环境变量，默认安全）

| 变量 | 默认 | 说明 |
|---|---|---|
| `TEAMX_IM_NOTIFY_GROUPS` | 空（禁用） | 目标飞书群 `chat_id`，逗号分隔 |
| `TEAMX_IM_DRY_RUN` | `1` | dry-run：只日志不发送（密钥分级：serve 不持有 secret 时唯一安全态） |
| `TEAMX_IM_NOTIFY_INTERVAL_SECS` | `15` | 收集+flush 周期 |
| `TEAMX_IM_DIGEST_SECS` | `60` | digest 窗口（测试可缩短） |
| `TEAMX_IM_FEISHU_APP_ID/_APP_SECRET` | 空 | live 发送必需（dry-run 不需要） |

## 7. 验证

- 单测 9 个（`im/notifier.rs`）：白名单/心跳过滤、游标幂等与审计不回环、direct 立即推+审计、digest 窗口延迟聚合、满批提前 flush、失败封顶+`im.push_failed`、429 退避不计失败、未配置只收集、渲染封闭性。
- 验收脚本：`tests/im-notifier-test.sh`（真实 serve + dry-run，T-1..T-4，见 docs/58），已注册 `run-all.sh` 15f。
