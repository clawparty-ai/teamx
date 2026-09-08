# user-console 事件推送（SSE）设计：替换轮询

> 让 console 页面从"每 N 秒轮询 REST"改为"服务端推送"（SSE），提升 Discussion / Events 的实时性，去掉周期刷新。
> 状态：**设计定稿（待测试方案 → 开发）**；更新：2026-09-08
> 相关：`crates/teamx/src/console.rs`（HTTP 服务）、`console_data.rs`（数据/网络）、`console_html.rs`（前端）、`docs/43/44/46/47`（discussion 系列）
> 背景：页面此前用 setInterval 每 15s 拉 `/api/panels/...` 实现"看新回复"，造成周期刷新体验。opencode 插件端有 serve `/ws` 实时推送，但 console 页面无此通道。

## 1. 目标
1. Discussion 列表/详情在"有新消息/新话题"时即时更新，去掉 15s 整页轮询。
2. Events 视图也能实时追加新事件（可选，顺带）。
3. 体验：有新内容才动 DOM，无内容静止、不打断输入。

## 2. 现状
- console 是 REST 拉模型：前端 `fetch("/api/panels/*")`。
- serve 端 `/ws` 给在线 opencode 插件实时推 ledger 事件；console 页面未接入。
- 浏览器 EventSource(SSE) 不适合直接带 mTLS 客户端证书连 serve（浏览器客户端证书选择不可控 + 跨源）。

## 3. 方案（Console 作为 SSE 中继）

```
浏览器                          console (本地 127.0.0.1, token 鉴权)           serve (HK, mTLS)
   │  EventSource /api/stream        │   后台循环: /rpc events(after=seq)     │
   │  <-- data:{event} --------------- 每 2s 拉增量 --------------------------->
   │  收到 discussion.message 等       │
   └ 增量更新当前视图（不整页刷新）
```

- **新端点**：console `GET /api/stream`（SSE，text/event-stream）。鉴权沿用 cookie token（同其它 API）。
- **后端中继**：Network 模式下，后台以该 workspace 成员证书对 serve 每 2s 调一次 `events` RPC（`after=<last_seq>`，team=该会话 team）；有增量事件即经 SSE 写出。Local 模式：本地 `events` CLI 增量（或返回空，见 §6）。
- **seq 水位**：每个 SSE 连接内维护 `last_seq`（起始从当前 events 最新 seq 快照，避免历史回放；此后只推新增）。
- **前端消费**：`EventSource('/api/stream')`；收到事件 → 按 `type` 触发：
  - `discussion.created / discussion.message / discussion.closed`：若在 Discussions 页 → `loadDiscussions()` 或对当前详情做一次 `pollDiscussion()`（有新增才刷新楼层）；若当前不在讨论页 → 只更新未读数/角标（可选）。
  - `progress.published / doc.* / goal.*` 等：Events 视图实时追加（若开启）。
- **移除 Discussion 的 15s setInterval**；改为：SSE 事件驱动 + 一个低频兜底（如 60s，仅用于 SSE 断线补偿），避免长断线时永久静默。

## 4. SSE 帧格式
```
event: event
data: {"type":"discussion.message","payload":{...},"seq":N,"team_id":"..."}

event: heartbeat
data: {"at":"..."}            // 每 ~15s 空行/heartbeat 保持连接（NAT/代理防掐）
```

## 5. 与现有轮询的关系（迁移）
| 现状 | 改后 |
|---|---|
| Discussion: `setInterval(pollDiscussion, 15000)` | SSE 驱动；`pollDiscussion` 保留为"SSE 触发的一次增量拉取"与"断线兜底" |
| Events: 手动刷新 / 翻页 | 加 SSE 追加（若开启） |
| Teams/Tasks | 不动（拉取型视图） |

> `pollDiscussion` 已实现"无新增不动 DOM"，正好可被 SSE 作为触发函数复用：收到 `discussion.message` → 调一次 `pollDiscussion()`；其内部对比消息数，无新增就不动。这样即使多成员同时刷，也只更新到最新楼层。

## 6. Local 模式（无 serve）
- console 跑在本地纯 CLI workspace 时无 serve 可拉。SSE 端点仍可连，但后端轮询改为读本地 DB 增量？console 是"view-only 读 CLI 所见"，本地模式事件在 workspace teamx.db。
- v1 取舍：**Local 模式 SSE 只发心跳不推事件**（前端自动回退到低频兜底轮询）。Network 模式是主要场景（远程团队）。

## 7. 约束与兼容
- SSE 连接数 = 浏览器标签页数；每连接 2s 一次 /rpc 拉取，一个用户 2-3 标签 = 每秒几次 RPC，量级可接受；避免为每个面板开独立 SSE，一个 `/api/stream` 全事件复用。
- 鉴权 cookie；EventSource 同源自动带 cookie。
- 需处理 SSE 断开重连（EventSource 自动重连；后端幂等：重连后从最新 seq 快照继续，不重放历史）。
- Discussion/Events 之外的视图不订阅（保持拉取）。

## 8. 待办（开发顺序）
1. console 后端 `/api/stream` SSE handler + 后台增量拉取（network）+ heartbeat。
2. console_data 暴露"拉 events 增量"函数（复用 events RPC / events_data 改造为 after seq）。
3. 前端 EventSource 接入：Discussion 列表/详情增量刷新（复用 pollDiscussion）、Events 可选追加。
4. 移除 Discussion 15s 轮询，保留 60s 兜底。
5. Local 模式回退处理。

## 9. 相关文档
- `docs/43/44/46/47`（discussion 参与机制/UI/测试）
- `docs/08-test-plan.cn.md`（测试分层约定）
