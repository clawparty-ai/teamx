# teamx 企业版：成员工时 / 财务 / 质量分析 + team-ui 看板（设计）

> 状态：**实施中**（enterprise 分支）
> 目标读者：实现者、team lead（owner）、成员
> 关联：`docs/network-mode.md`（网络模式/中央 serve）、`opencode-plugin/src/index.ts`（event hook）、`crates/teamx/src/db.rs`（schema）、`crates/teamx/src/serve.rs`（HTTP/WS）

---

## 0. 目标

记录团队中**每个成员节点**的活动明细（时间 / 耗时 / token / 成本 / 执行内容），全部**汇总到 team lead（owner）的 serve 数据库**，供：

1. **工时分析**：谁、什么时候、工作了多少时间。
2. **财务分析**：每个成员/节点/团队消耗的 token 与成本（USD）。
3. **工作质量分析**：执行了什么（tool-call / 写代码 / 写文档 / 跑命令），改动量，效率。

提供 `teamx ui` 命令启动一个 **web 看板**，可交互查询统计。

---

## 1. 核心决策（已确认）

| 决策点 | 结论 |
|---|---|
| 数据存储位置 | **发送到 team lead 的 serve**，存 serve 的 SQLite（中央存储）；单机 V1 **也落库**（本地 activity 表） |
| 节点归属 | 每个 activity **记录来源节点**（`node_id` = 该机器 `~/.teamx/instance.json` 的 `instance_id`，`node_name` = hostname） |
| 独立数据表 | 新 `activity` 表（**不**混入 `events` 账本，避免污染 sync/推送） |
| 传输通道 | 插件 event hook 收集 → 经 RPC 批量发送到 serve（`activity.push`） |
| 看板 | `teamx ui` web server：**仅 team lead 可启动**，**HTTPS 自签名 + 随机 token**（`https://127.0.0.1:9527/?token=<随机>` 打印到命令输出），读 serve DB |
| 参数记录 | **完整记录**（tool/命令参数、用户消息全文，不截断，用于审计） |
| cost | 有则记，无则不记（可空） |
| 回填 | 不回填，从启用后追加 |
| 防风暴 | 批量队列 + 去抖（2s / 20条） |
| human 记录 | 完整记录用户消息/审批/命令（human_input/human_approval/human_command） |
| work_session | idle 间隔 + `has_human` 标记（区分纯 AI vs 人参与） |

---

## 2. 数据模型

### 2.1 `activity` 表（serve 数据库）

```sql
CREATE TABLE IF NOT EXISTS activity (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  team_id       TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
  member_id     TEXT NOT NULL,
  node_id       TEXT NOT NULL,              -- 来源节点 instance_id（~/.teamx/instance.json）
  node_name     TEXT,                        -- 来源节点 hostname（可读标识）
  started_at    TEXT NOT NULL,               -- 事件发生/工作段开始（RFC3339）
  ended_at      TEXT,                        -- 工作段结束（idle 触发时为段尾）
  duration_ms   INTEGER,                     -- 耗时（毫秒）
  kind          TEXT NOT NULL,               -- 见下表
  detail        TEXT,                        -- JSON 详情（tool/命令参数、用户消息全文）
  tokens_input  INTEGER,                     -- step 输入 token
  tokens_output INTEGER,                     -- step 输出 token
  tokens_reasoning INTEGER,                  -- step 推理 token
  cost          REAL,                        -- 成本（USD，来自 step-finish，可空）
  has_human     INTEGER NOT NULL DEFAULT 0,  -- work_session: 该时段内是否有 human 活动
  created_at    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_activity_team_time ON activity(team_id, started_at);
CREATE INDEX IF NOT EXISTS idx_activity_member_time ON activity(member_id, started_at);
CREATE INDEX IF NOT EXISTS idx_activity_node ON activity(node_id);
```

### 2.2 采集维度 → activity 行

| opencode event | kind | 记录内容（detail 完整，不截断） | 时间/耗时 | token/cost |
|---|---|---|---|---|
| `message.part.updated`（ToolPart） | `tool_call` | `{tool, state, arguments}`（tool 名 + 完整参数 + 状态） | started_at=事件时间 | 无 |
| `message.part.updated`（StepFinishPart） | `step_finish` | `{reason}` | started_at=事件时间，duration=step 耗时 | ✅ tokens + cost |
| `command.executed` | `command` | `{name, args}`（命令 + 完整参数） | started_at=事件时间 | 无 |
| `file.edited` | `file_edit` | `{file}` | started_at=事件时间 | 无 |
| `message.updated`（UserMessage, role=user） | `human_input` | `{sessionID, text}`（用户消息全文） | started_at=事件时间 | 无 |
| `permission.replied` | `human_approval` | `{permissionID, response, title}` | started_at=事件时间 | 无 |
| `tui.command.execute` | `human_command` | `{name, args}` | started_at=事件时间 | 无 |
| `session.idle` 间隔 | `work_session` | `{sessionID}`；计算上次 idle 到本次 idle 的间隔；`has_human`=该时段内是否有 human 活动 | started_at=段开始，ended_at=本次 idle，duration=间隔 | 无 |

> **human-in-the-loop 定义**：`human_input`（用户发消息）、`human_approval`（审批/拒绝权限）、`human_command`（执行斜杠命令）均为**人主动触发的动作**；`work_session` 是人在场的连续工作段（idle 间隔），以 `has_human` 区分"纯 AI 自动执行"与"人参与"。

---

## 3. 节点标识

- **node_id**：每台机器 `~/.teamx/instance.json` 的 `instance_id`（`crypto.randomUUID()`，安装时生成，稳定）。
- **node_name**：`os.hostname()`（可读，便于看板识别）。
- serve 端还可结合 **peer IP**（`ConnectInfo<SocketAddr>`，已有）交叉验证节点真实性。

---

## 4. 数据流

```
member 节点（opencode + teamx 插件）
  event hook：session.idle / message.part.updated / command.executed / file.edited
              / message.updated(UserMessage) / permission.replied / tui.command.execute
        │ 收集 + 本地排队（内存缓冲，防风暴：2s 或 20 条 flush）
        ▼
  [有 serve] activity.push RPC（mTLS，批量，带 node_id/node_name）
        ▼
team lead serve ──► SQLite：activity 表（中央事实源）
        │
        ▼
  teamx ui（仅 team lead 机器启动）──► 读 serve DB（查询 RPC）
        ▼
  HTTPS 看板（127.0.0.1:9527/?token=<随机>）—— 交互式统计查询

  [无 serve，单机 V1]
  插件 ──► 本地 activity 表（~/.teamx/teamx.db），后续 serve 存在时可同步
```

- **防风暴**：tool_call 事件可能很频繁。插件用**批量队列**（每 2s 或每 20 条 flush 一次）发送，避免 RPC 洪泛。
- **离线**：成员断线时队列滞留，重连后补发（幂等）。
- **本地落库（Q1）**：单机 V1 无 serve 时，activity 写入本地 `~/.teamx/teamx.db` 的 activity 表；有 serve 时经 RPC 上报中央库。

---

## 5. `teamx ui` 命令（看板）

### 5.1 CLI

```
teamx ui [--addr 127.0.0.1] [--port 9527]
```

- **仅 team lead（owner）可启动**：启动前校验当前会话身份（mTLS 证书 CN → member_id → 是否 owner），非 owner 拒绝。
- **HTTPS 自签名证书**：复用 pki 模块生成（无外部依赖）。
- **随机 token**：启动时生成 32 字节随机 token，URL `https://127.0.0.1:9527/?token=<token>` **打印在命令输出**。
- **数据源**：读 serve DB（查询 RPC，mTLS 连接 team lead 自己的 serve）。

### 5.2 安全流程

```
teamx ui
  ├─ owner 身份校验（mTLS 证书 CN → member_id → 是否 owner）
  ├─ 自签名 HTTPS 证书生成
  ├─ 生成随机 token（32 字节 hex）
  ├─ 打印 https://127.0.0.1:9527/?token=<token>
  └─ 浏览器访问：?token= 校验 → 设置 cookie → 后续请求带 cookie
```

### 5.3 页面（单页 HTML + 原生 JS，无外部依赖）

**看板（默认页）**：
- 概览卡片：团队总工时 / 总 token / 总成本 / 活跃成员数 / 活跃节点数
- **人 vs AI 分布**（human_input/approval/command vs tool_call/step_finish）
- 成员活动柱状图（按成员聚合耗时/token/成本）
- 节点分布（按 node 聚合）
- 活跃时间热力图（按小时/天）
- 最近 activity 列表

**交互式查询（表单/过滤）**：
- 按成员 / 节点 / 时间范围 / kind 过滤
- 聚合：总耗时、总 token（input/output/reasoning）、总成本
- tool 使用分布（top N tool）
- 文件改动统计（top N 文件/目录）
- 明细表（完整 detail 可展开）+ CSV 导出

### 5.4 后端查询 API（serve 新增 RPC，owner 可用）

```
activity.summary   {team, from, to} → 聚合概览
activity.by_member {team, from, to} → 按成员聚合
activity.by_node   {team, from, to} → 按节点聚合
activity.tools     {team, from, to} → tool 分布
activity.files     {team, from, to} → 文件改动
activity.rows      {team, from, to, kind, member, node, limit} → 明细
```

---

## 6. 实施里程碑

| 阶段 | 内容 | 验收 |
|---|---|---|
| **A1** | `activity` 表 + 迁移；serve 新增 `activity.push` RPC（批量插入，带 node_id/node_name）+ 查询 RPC | RPC 插入后可查询 |
| **A2** | 插件 event hook 扩展：tool_call / step_finish / command / file_edit / human_input / human_approval / human_command 采集 + 批量队列发送 | serve 收到成员活动 |
| **A3** | work_session 计算（session.idle 间隔 + has_human） | 工时估算可用 |
| **A4** | `teamx ui` 命令（HTTPS + token + owner 校验）+ web 看板（概览 + 交互查询 + CSV） | 浏览器可见统计 |
| **A5** | 权限：owner 全量 + team-ui；成员写自己节点、读自己 + 匿名聚合 | 看板按角色展示 |

---

## 7. 决策记录（均已确认）

| # | 问题 | 决策 |
|---|---|---|
| Q1 | 单机 V1（无 serve）时 activity 是否要落库？ | **需要**（本地 activity 表） |
| Q2 | team-ui 谁能启动 / 数据源？ | **仅 team lead 能启动**，读 serve DB（teamx-server） |
| Q3 | step-finish 的 cost 字段是否总是可用？ | 有则记，无则不记（可空，聚合跳过 NULL） |
| Q4 | 历史数据回填？ | **不回填**，从启用后追加 |
| R1 | tool_call 事件风暴 | 批量队列 + 去抖（2s/20条） |
| R2 | 敏感参数是否记录？ | **完整记录**（tool/命令参数、用户消息全文），用于审计 |
| D1 | detail 大小上限？ | **不截断**（完整） |
| D2 | team-ui 认证方式？ | **HTTPS 自签名 + 随机 token**（`https://127.0.0.1:9527/?token=<随机>` 打印输出），非 mTLS |
| D3 | human 操作记录粒度？ | **完整**（用户消息全文） |
| D4 | work_session "人在场"判定？ | idle 间隔 + `has_human` 标记 |
