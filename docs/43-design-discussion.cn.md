# teamx Discussion（成员群组讨论）设计

> 对应 5 项待办中的 T5。对标 ClawParty group-chat：让 teamx 成员（人类 + AI agent）之间能发起/参与**群组讨论**，作为现有异步 ledger/task 协作之外的实时讨论通道。
> 状态：**设计（idea → 待 review）**；更新：2026-09-07
> 相关代码/文档：`crates/teamx/src/events.rs`、`server_tasks.rs`、`docs/27-taskx.cn.md`、ClawParty `groupchats.rs`

## 1. 背景与动机

teamx 现在的协作模型是**异步事件/任务**：publish 事件、taskx 状态机、提问/回答（ask/respond 一对一）。它擅长"留痕、验收、接力"，但缺少一个**轻量群聊层**：
- 多个成员针对一个主题快速交换意见/澄清；
- owner 与一组成员实时对齐（而不是写一串正式事件）；
- agent 成员之间围绕当前任务的即时讨论（群聊是比多篇 progress 更自然的形式）。

对标 ClawParty 的 group-chat（`groupchats.rs`：建群/加人/消息/历史 REST），teamx 引入 **discussion**：一次围绕**某主题/任务/目标**的群组讨论。

## 2. 目标 / 非目标

**目标**
1. 成员可**创建 discussion**（主题可关联某 team / task / 普通闲聊）并邀请/自动纳入成员。
2. 成员可**在 discussion 内发消息**（文本；可选 @member 定向），消息带作者/时间，留痕可回溯。
3. 提供 **list / messages / leave / close** 等管理。
4. 与现有模型一致：走 **ledger 事件**落库（可审计、可同步到各成员 opencode 会话），并在 user-console / admin-console 呈现。

**非目标（本期）**
- 不做实时低延迟聊天协议（不做独立 WS 聊天流；复用 teamx 既有 WS 推送做近实时提醒）。
- 不做富文本/附件/私聊（本期仅团队内群讨论，文本为主）。
- 不做复杂权限分级：**团队内成员可见/可参与**（discussion 归属某个 team）。

## 3. 术语与信息模型

| 概念 | 说明 |
|---|---|
| `discussion` | 团队内一个讨论主题：`{id, team_id, title, topic?, created_by, created_at, closed_at?}`；可选关联 `task_id` / `goal_id` |
| `message` | 讨论内一条消息：`{id, discussion_id, author_member_id, body, created_at}`；body 可为文本或轻量 markdown |
| 参与者 | 默认团队全员可见可发言；不维护显式成员表（v1）——需要@提醒时按文本 `@display_name` 提示对应成员 |

关系：`discussion` 属于 `team`；可零个或多个 `task`/`goal` 上下文；消息序列不可变追加。

## 4. 与既有模型的关系（关键设计决策）

teamx 已有：事件 ledger（`events` 表 + publish）、taskx 文档状态机、`ask/respond`（一对一提问置 waiting）、WS 推送。

**决策**：

| # | 决策 | 理由 |
|---|---|---|
| D1 | discussion/message 以**两类 ledger 事件**表达：`discussion.created`、`discussion.message`（每条消息一个事件，payload 含 discussion_id/body/title） | 复用现有事件总线：成员 sync 即可看到、WS 可推送、审计天然留痕；不引入第二套存储 |
| D2 | 不新建独立 discussion 表存消息正文；**事件即存储**（读历史 = events 过滤 `type IN ('discussion.created','discussion.message')`） | 与现有 `log`/`sync` 心智一致、最少 schema 变更 |
| D3 | 权限：**同 team 成员**可创建 discussion、发消息、看历史；owner/lead 可 close 任意 discussion | 与团队边界一致；close = 追加 `discussion.closed` 事件（不删除历史） |
| D4 | discussion 可挂 `task_id`：在 taskx 视图里能按 task 看到关联讨论线程 | 让讨论围绕任务沉淀，不散落在事件流 |
| D5 | @提醒：v1 仅文本约定 `@display_name`（不单独建提醒事件）；系统可在 UI 高亮 | 降低实现成本 |
| D6 | 近实时：复用现有 WS 推送（事件广播）；UI 轮询/刷新获取 | 不新建聊天 WS 协议 |
| D7 | **消息支持引用（reference/reply）**：`discussion.message` 事件 payload 支持 `reply_to`（同 discussion 内被引用消息）与 `quotes[]`（引用任意可寻址 ledger 对象）。引用以**引用事件坐标 + 原文字面快照**双份落事件，展示不依赖二次查询、审计完整 | 聊天需要"引用前文/引用某条任务进展"作为讨论上下文，否则长讨论会丢失指代 |
| D8 | 事件坐标寻址统一为 `(team_id, seq)`；正文快照做**只读语义的引用**（引用的是不可变历史消息，事件不删除故坐标稳定） | 事件即存储下消息不可变，引用不会因原文"编辑/删除"悬空 |

> 不采用「独立 SQL 表 + 独立聊天 API」的 ClawParty 式实现：teamx 的核心价值是**一切入 ledger**，discussion 作为事件类型融入，天然获得跨成员同步与审计。

## 5. CLI / RPC 设计

沿用 teamx 命令风格（本地 CLI + 网络 RPC）：

```
teamx discussion create  --title "<主题>" [--task <task_id>] [--session s] [--team t]
teamx discussion list    [--session s] [--team t]              # 未 close 的讨论
teamx discussion open    <id> [--session s] [--team t]         # 查看某讨论及消息
teamx discussion say     <id> "<正文>" [--session s] [--team t]
teamx discussion reply   <id> <message_id> "<正文>" [--session s] [--team t]   # 引用同讨论某消息后发言
teamx discussion close   <id> [--session s] [--team t]         # lead/owner 关闭
```

| 方法 | 事件 | 说明 |
|---|---|---|
| `discussion.create` | `discussion.created` | payload: `{discussion_id, title, task_id?}` |
| `discussion.say` | `discussion.message` | payload: `{discussion_id, message_id, body, reply_to?, quotes?}` |
| `discussion.close` | `discussion.closed` | payload: `{discussion_id}` |
| `discussion.list/open` | 只读 | list=聚合活跃 discussion；open=返回该 discussion 消息时间线 |

## 5.1 引用聊天记录 / 引用对象（D7/D8）详细设计

> 这是 discussion 里最关键的一环：长讨论中发言必须能锚定"我在回应哪句话 / 哪个候选 / 哪条 progress"，否则读历史会丢失上下文。

### 5.1.1 被引用对象（Reference target）

一条消息可引用三类目标：

| 类型 | 坐标 | 示例 | 用途 |
|---|---|---|---|
| **同 discussion 内消息** | `(seq)` 即 `message_id` | 引用上一句 "DOI 我抽 3 条验证 OK" | 最常见的"回复某句话" |
| **任意 ledger 事件**（task/progress/decision…） | `(team_id, seq)` + `type` | 引用 `progress.published` 或 task 状态事件 | 讨论围绕某次具体汇报/任务流转 |
| **结构化对象**（task / card / doc） | 对象 id（如 task_id / doc id） | 引用 taskx `search-round2-cd` | 把讨论挂到任务，并在任务视图呈现 |

### 5.1.2 事件 payload 表示

`discussion.message` 的 payload 扩展：

```jsonc
{
  "discussion_id": "dsc_abc123",
  "message_id": "dsc_abc123:msg_0",
  "author_member_id": "…",
  "body": "这条 C1 与已有卡片重复，建议合并",
  // 单引用：对同讨论某条消息的回复（语义 reply_to）
  "reply_to": { "message_id": "dsc_abc123:msg_7" },
  // 多引用：可同时引用多个对象（事件/任务/卡片），用于"综合前几条"
  "quotes": [
    { "kind": "event", "team_id": "…", "seq": 688, "type": "doc.created", "text": "检索第二批…(摘要快照)" },
    { "kind": "task", "task_id": "search-round2-cd", "title": "检索第二批论文候选…", "text": "(任务标题快照)" },
    { "kind": "message", "message_id": "dsc_abc123:msg_7", "text": "DOI 我抽 3 条验证 OK(快照)" }
  ]
}
```

### 5.1.3 关键设计：坐标 + 快照（不依赖二次查询）

每个 quote 项同时携带：
1. **坐标**（`kind`+ id / seq / task_id）：用于 UI 点击跳转、会话内可点击溯源；
2. **`text` 快照**（原文字面截断，如 ≤500 字）：即使消息历史很长、前端无需重查也能直接展示被引内容；也避免"引用悬空"。

> 因为事件即存储且事件**不可变不删除**，坐标永远稳定；快照用于即时渲染，坐标用于精确溯源。两者缺一不可：只有快照无法跳转/结构化，只有坐标需要额外查询且显示为空引用风险。

### 5.1.4 引用链与深度

- 允许引用一条本身带引用的消息（嵌套引用）但 **UI 只展开一层**（被引消息再带引用时显示"↪ 查看上一引用"），防引用递归。
- 服务端对 `reply_to`/`quotes` 做**存在性校验**（被引消息/对象属于本 team 且存在），但不做深度限制。

### 5.1.5 UI 呈现

- 消息气泡顶部显示被引用块（引用方：头像+名字+原文快照单行/多行灰底），可点击跳转/高亮目标消息；
- 目标消息若在同一屏内，点击后滚动并闪烁高亮；
- 消息正文与引用块分离，引用不进入正文全文检索（避免重复命中）。

### 5.1.6 发送侧便利（CLI/插件）

- CLI：`discussion reply <id> <message_id> "<正文>"` 便捷生成 `reply_to`；
- 插件：agent 在讨论里可用工具传 `quote_seq`（对某条 event seq）发言，让 AI 引用具体汇报。


## 6. opencode 插件 / UI

- 插件新增只读/交互命令：`/team discussion list`、`/team discussion say <id> <text>`（工具化），agent 成员可用 `teamx_discussion_say` 参与讨论。
- user-console（member console）："Discussions" 面板——列活跃讨论、点开看消息流、可发言。
- admin-console（teamx-ui）：目标/任务详情页显示关联 discussion 线程。
- 消息在成员 opencode 会话的 digest / toast 提示（新消息 = 一条新事件，复用现有通知）。

## 7. 数据流示例

```
（owner）teamx discussion create --title "review zhongm round2 候选"
   → events: discussion.created {discussion_id, title, task_id:search-round2-cd}
   → WS 推送 → zhaoz/caicai 收到 digest "新讨论: review zhongm round2 候选"

（caicai）teamx discussion say <id> "C/D 各 15 条，DOI 我抽 3 条验证 OK，review report 我来写"
   → events: discussion.message {discussion_id, body}
   → owner digest 提示

（zhaoz）teamx discussion reply <id> <caicai_msg_id> "辛苦，report 里把 3 条 DOI 的核对结果列成表"
   → events: discussion.message {discussion_id, body, reply_to:{message_id:caicai_msg_id},
       quotes:[{kind:"message", message_id:caicai_msg_id, text:"DOI 我抽 3 条验证 OK…"(快照)}]}
   → UI: zhaoz 消息上方显示被引用的 caicai 原文块（可点击跳转）

（owner）teamx discussion close <id>
   → events: discussion.closed
```

## 8. 约束与兼容

- 新增事件类型不破坏既有 sync/ledger 解析（读取方对未知 type 应跳过/显示为 raw）。
- 消息正文长度上限（如 ≤4KB）防滥用；每个 quote 的 `text` 快照上限（如 ≤500 字）、单条消息 `quotes` 数量上限（如 ≤8）。
- 引用校验：`reply_to`/`quotes` 引用的目标必须属于本 team 且存在；目标为事件时校验其 seq 确实存在（快照由服务端从原事件正文生成，不允许客户端伪造 text）。
- 保持 owner/team 隔离：discussion 归属 team，跨队不可见；引用解析同样限制在本 team 内。
- 文档中英双语同步（如落实）。

## 9. 后续可选项（不阻塞 v1）
- 显式参与者表 + 邀请/踢人；
- @成员提醒事件（针对性 nudge）；
- 消息编辑/撤回（事件即存储下为"追加更正消息"+ 引用显示已被更正标记，不原地改历史）；
- 讨论→决策自动沉淀（close 时可选归档为 decision 事件）。

## 10. 验证方法
1. 单机两成员：A create discussion，B sync 收到事件；B say，A open 可见消息时间线；owner close。
2. **引用**：B `reply` 到 A 的消息 → open 里消息带引用块，含被引原文快照；点击可跳转/高亮 A 的原消息；引用一条 `progress`/task 事件 → 消息带结构化引用，UI 能溯源。
3. **引用完整性**：构造对不存在 seq 的引用 → 服务端拒绝；跨 team 引用 → 拒绝。
4. 关联 task：create 带 `--task`，task log 可见关联事件。
5. 权限：非本队成员（另一 team 证书）list/say 应被拒。
6. 回归：既有 events/log/sync/task 测试全绿；UI 新增面板不破坏 KanBan/Timeline。

## 11. 相关文档
- `docs/27-taskx.cn.md`（任务/事件模型）
- `docs/04-design-team-invite.cn.md`（成员身份）
- `docs/41-design-kanban-detail.cn.md` / `docs/42-design-teamx-agents.cn.md`（同期设计）
- ClawParty `groupchats.rs`（对标参考）
