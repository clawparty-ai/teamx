# teamx 企业版：成员工时 / 财务 / 质量分析 + team-ui 看板（设计）

> 状态：**设计中**（enterprise 分支）
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
| 数据存储位置 | **发送到 team lead 的 serve**，存 serve 的 SQLite（中央存储）；单机无 serve 时不落库（或本地暂存待同步） |
| 节点归属 | 每个 activity **记录来源节点**（`node_id` = 该机器 `~/.teamx/instance.json` 的 `instance_id`，`node_name` = hostname） |
| 独立数据表 | 新 `activity` 表（**不**混入 `events` 账本，避免污染 sync/推送） |
| 传输通道 | 插件 event hook 收集 → 经 RPC 批量发送到 serve（`activity.push`） |
| 看板 | `teamx ui` 本地 web server，读 serve 数据（或本地 DB），默认仅本机访问 |

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
  kind          TEXT NOT NULL,               -- tool_call | step_finish | command | file_edit | work_session
  detail        TEXT,                        -- JSON 详情（tool 名/文件/命令等，不含敏感参数）
  tokens_input  INTEGER,                     -- step 输入 token
  tokens_output INTEGER,                     -- step 输出 token
  tokens_reasoning INTEGER,                  -- step 推理 token
  cost          REAL,                        -- 成本（USD，来自 step-finish）
  created_at    TEXT NOT NULL
);
CREATE INDEX idx_activity_team_time ON activity(team_id, started_at);
CREATE INDEX idx_activity_member_time ON activity(member_id, started_at);
CREATE INDEX idx_activity_node ON activity(node_id);
```

### 2.2 采集维度 → activity 行

| opencode event | kind | 记录内容 | 时间/耗时 | token/cost |
|---|---|---|---|---|
| `message.part.updated`（ToolPart） | `tool_call` | `detail: {tool, state}`（tool 名 + 状态，**不含参数**） | started_at=事件时间 | 无 |
| `message.part.updated`（StepFinishPart） | `step_finish` | `detail: {reason}` | started_at=事件时间，duration=step 耗时 | ✅ tokens + cost |
| `command.executed` | `command` | `detail: {name}`（命令名，**不含参数**） | started_at=事件时间 | 无 |
| `file.edited` | `file_edit` | `detail: {file}` | started_at=事件时间 | 无 |
| `session.idle` | `work_session` | `detail: {sessionID}`；计算上次 idle 到本次 idle 的间隔为一段工作 | started_at=段开始，ended_at=本次 idle，duration=间隔 | 无 |

> **敏感信息保护**：tool 参数、命令参数、文件内容一律**不记录**，只记录名称/路径。成员隐私与数据最小化。

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
        │ 收集 + 本地排队（内存缓冲，防风暴）
        ▼
  activity.push RPC（mTLS，批量，带 node_id/node_name）
        ▼
team lead serve ──► SQLite：activity 表（中央事实源）
        │
        ▼
  teamx ui（owner 机器上运行）──► 读 serve（RPC/HTTP）或本地 DB
        ▼
  Web 看板（127.0.0.1:PORT）—— 交互式统计查询
```

- **防风暴**：tool_call 事件可能很频繁。插件用**批量队列**（如每 2s 或每 20 条 flush 一次）发送，避免 RPC 洪泛。
- **离线**：成员断线时队列滞留，重连后补发（幂等）。
- **无 serve**（单机 V1）：插件可本地暂存（`~/.teamx/activity-cache.json`），或直接忽略（看板需 serve 数据）。

---

## 5. `teamx ui` 命令（看板）

### 5.1 CLI

```
teamx ui [--addr 127.0.0.1] [--port 5782] [--db <path>]
```

- 启动 HTTP server，默认 `127.0.0.1:5782`（仅本机）。
- 读取数据源：`--db` 指定本地 SQLite，或默认经 `TEAMX_SERVER_URL` 的 serve RPC（读 activity）。

### 5.2 页面（单页 HTML + 原生 JS，无外部依赖）

**看板（默认页）**：
- 概览卡片：团队总工时 / 总 token / 总成本 / 活跃成员数
- 成员活动柱状图（按成员聚合耗时/token）
- 节点分布（按 node 聚合）
- 活跃时间热力图（按小时/天）
- 最近 activity 列表

**交互式查询（表单/过滤）**：
- 按成员 / 节点 / 时间范围 / kind 过滤
- 聚合：总耗时、总 token（input/output/reasoning）、总成本
- tool 使用分布（top N tool）
- 文件改动统计（top N 文件/目录）
- 导出 CSV

### 5.3 后端查询 API（serve 新增 RPC 或 ui 内建）

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
| **A1** | `activity` 表 + 迁移；serve 新增 `activity.push` RPC（批量插入，带 node_id/node_name） | RPC 插入后可查询 |
| **A2** | 插件 event hook 扩展：tool_call / step_finish / command / file_edit 采集 + 批量队列发送 | serve 收到成员活动 |
| **A3** | work_session 计算（session.idle 间隔） | 工时估算可用 |
| **A4** | `teamx ui` 命令 + web 看板（概览 + 交互查询 + CSV） | 浏览器可见统计 |
| **A5** | 权限：owner 看全部，成员看自己 + 匿名团队聚合 | 看板按角色展示 |

---

## 7. 待确认 / 风险

| # | 问题 | 建议 |
|---|---|---|
| Q1 | 单机 V1（无 serve）时 activity 是否要落库？ | 可本地缓存待同步；V1 阶段可先忽略 |
| Q2 | 看板数据源默认本地 DB 还是 serve？ | `teamx ui` 默认读本地 DB（owner 常在本机）；可 `--server` 指向远程 serve |
| Q3 | step-finish 的 cost 字段是否总是可用？ | 取决于模型/provider；缺失时为 NULL，财务聚合按可用值 |
| Q4 | 历史数据回填？ | 不自动；从启用后开始记录（追加式） |
| R1 | tool_call 事件风暴 | 批量队列 + 去抖（2s/20条） |
| R2 | 成员隐私（敏感参数） | 只记名称不记参数；文档明示 |
