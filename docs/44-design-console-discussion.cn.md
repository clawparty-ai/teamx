# user-console Discussion 页面设计（BBS 形态）

> user-console = 原 `teamx console`（member web console）。本设计在 console 增加 **Discussion（讨论）** 页面，让成员查看/参与团队讨论线程（对标 BBS / 论坛楼层）。
> 状态：**设计 → 开发**；更新：2026-09-07
> 底层能力：`docs/43-design-discussion.cn.md`（discussion 事件模型 + CLI/RPC + 引用），已实现 `discussion create/list/open/say/reply/close` 与 serve RPC。
> 相关代码：`crates/teamx/src/console_html.rs`（页面 HTML/CSS/JS 常量）、`crates/teamx/src/console.rs`、`console_data.rs`（panel 后端）

## 1. 目标

1. Console 新增 **Discussions** 导航页，查看当前团队的话题列表（BBS 形态）。
2. 打开一个话题，按"楼层"查看讨论线程：作者、时间、正文、**引用块**（reply_to/quotes 显示被引用原话）。
3. 用户可直接发言回复；agent 成员也可自动参与（经 CLI/RPC `discussion say`），console 刷新/轮询后可见 agent 的回复。
4. 允许新建话题与关闭话题（owner/lead）。

## 2. 现状与复用

- `discussion.rs` 已实现 CLI：`create / list / open / say / reply / close`；serve 端已做 RPC 映射（`discussion.*`），网络模式成员可用。
- console 是 network/local 双模式，panel 后端 `fetch_panel` 目前支持 `teams / events / tunnels / tasks`，通过 `net_rpc(url, method, args)` 调 serve。
- console 前端为单页 vanilla JS（`console_html.rs` 内 `const JS`），nav 三键 + `showView(name)` 切视图。

**缺口**：console 尚无 discussion 数据通道与 UI。

## 3. 页面与交互设计

### 3.1 导航与视图
- Nav 增加第 4 个按钮 **`<button data-view="discussions">Discussions</button>`**。
- 新增 `<div class="view" id="view-discussions">`。

### 3.2 两级界面（BBS）
默认进入**话题列表**：

```
Discussions                              [+ 新建话题]
┌──────────────────────────────────────────────────────────┐
│ 💬 Review round2 候选            caishu · 3 回复 · 5m ago │   ← 标题/发起人/回复数/最后活跃
│ 🔒 已关闭：旧话题归档           zhaoz  · 12 回复 · 2d ago │   ← closed 话题灰显 + 🔒
└──────────────────────────────────────────────────────────┘
```

点话题 → **话题详情**（BBS 楼层）：

```
← 返回列表          标题：Review round2 候选        [刷新]
┌──────────────────────────────────────────────────────────┐
│ #1  caishu · 今天 12:36                                  │
│    开个线程讨论 zhongm 的 C/D 候选                        │
│ #2  zhaoz · 今天 12:40                                   │
│    ▎引用 caishu #1                                        │
│    ▎"开个线程讨论 zhongm 的 C/D 候选"                     │
│    收到，DOI 我抽查 3 条 OK                               │
│ #3  caishu · 今天 12:41                                   │
│    辛苦，report 列成表                                    │
┌──────────────────────────────────────────────────────────┐
│ [输入回复…]                          (回复 #2 引用) (发送)│
└──────────────────────────────────────────────────────────┘
```

### 3.3 交互
- **话题列表**：每条 = 标题、发起人、回复数、最后活跃时间；closed 显示 🔒 灰显；有未读/新消息可高亮（v1 不做未读计数，靠刷新）。
- **话题详情**：
  - 楼层递增编号（按 seq 升序），每条：作者、时间、正文；若带 reply_to 显示引用块（"引用 <author> #<楼层>"+被引原文快照）；quotes 渲染为多条引用。
  - 底部输入框：直接回复到话题末尾；可选 **回复引用某条**（点某楼层"引用"→ 输入框顶部显示被引摘要，发送带 reply_to）。
- **新建话题弹层**：标题 + 首条正文（可关联 task——下拉可选当前团队 task，v1 可选）。
- **关闭话题**：详情页 owner/lead 可见"关闭讨论"按钮。
- **自动刷新**：进入详情后每 15s 轮询一次（复用 `panel` 通道的 events 心跳节奏），让 agent 新回复出现。

### 3.4 数据通道（console 后端）
在 `console_data.rs` 增加 panel handler，使前端能获取结构化数据：

| panel id | 用途 | 底层调用 |
|---|---|---|
| `discussions` | 话题列表 | `discussion.list` RPC（或本地 CLI） |
| `discussion` | 单个话题详情 | `discussion.open` RPC（返回 messages 楼层） |
| 写操作 | 新建/发言/回复/关闭 | 经 console `/api/…` post 代理 → 对应 RPC/CLI |

实现方式（保持与现有 panels 一致）：
- `fetch_panel(ctx, "discussions")` → 调用 `net_rpc(url, "discussion.list", {})`（network）或 `local_cli(["discussion","list",...])`（local）。
- `fetch_panel(ctx, "discussion")`（带 query id）→ `discussion.open`。
- 写操作（say/reply/create/close）需要**console 侧提供 POST**：新路由 `/api/discussion/{action}` 携带 body，经 `spawn_blocking` 调 `run_cli`/`net_rpc`，返回 `{ok, ...}`。
  - 简单起见：所有写操作也走 `panel` 相似函数 `run_discussion_action(action, payload, ctx)`。

> 复用 console 已有 `Ctx`（mode/url/session）与 `net_rpc` / `local_cli` 辅助。写操作需要 session 身份：network 模式证书自动；local 模式取 workspace session（复用 require_local_session）。

## 4. 事件 / 引用模型（对齐 43 设计）

- 楼层编号 = 该话题内 messages 的序号（1 起），与事件 seq 映射：`floor = message 在 open 返回数组中的序号`；`message_id = <thread>#<seq>`。
- 引用渲染：`reply_to = {message_id, text}` → 显示 "引用 <author> #<floor>：text 快照"（快照已在事件中，前端不需再查）；`quotes[]` 多项同理。
- agent 参与：agent 通过 CLI/插件 `discussion say/reply` 发言 → 事件进 ledger → console 轮询 `discussion.open` 看到新楼层。

## 5. 约束与兼容
- 仅显示当前团队讨论（list/open 已按 team 隔离）。
- closed 话题只读不可再发言；owner/lead 可 close（后端已校验）。
- 分页：话题详情若楼层很多，先展示最近 200 条并支持"加载更早楼层"（参考 Events 分页）；列表先展示最近 50 话题。
- 时间显示沿用 `tf()`（本地时间）。
- 不改动 discussion 后端事件模型；纯 console 层新增。

## 6. 验证方法
1. `cargo build -p teamx` + `cargo test -p teamx` 全绿。
2. 本地起 console（network 指向测试 server）：
   - 有话题则列表展示；无则提示可新建。
   - 打开话题看到楼层；reply 带引用显示引用块。
   - 输入框发言成功，刷新出现自己新楼层。
   - 另一身份（agent/CLI）`discussion say` 后 15s 轮询可见 agent 回复。
   - owner/lead 关闭话题后按钮消失、再发言被拒。
3. 回归：Teams / My Tasks / Events 三个既有视图不受影响。

## 7. 相关文档
- `docs/43-design-discussion.cn.md`（discussion 底层设计）
- `crates/teamx/src/discussion.rs`、`console_html.rs`、`console_data.rs`
