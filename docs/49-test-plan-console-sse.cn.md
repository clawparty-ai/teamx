# 测试方案：user-console SSE 事件推送（替换 discussion 轮询）

> 对应设计 `docs/48-design-console-sse.cn.md`。
> 状态：**方案（待执行）**；更新：2026-09-08

## 1. 范围
- console 新增 `GET /api/stream` SSE（network 模式增量推送；local 模式仅心跳）。
- 前端 Discussion 由 SSE 驱动刷新（移除 15s 轮询，保留 60s 兜底）。
- Events 视图实时追加（可选）。

## 2. 用例清单

### 2.1 后端 SSE（集成，curl）
| ID | 用例 | 期望 |
|---|---|---|
| S-01 | 未带 cookie 访问 /api/stream | 401 |
| S-02 | 带 cookie，network 模式：连接后 serve 有新的 discussion.message | SSE 收到 `event: event` + `data:{type:"discussion.message",…}` |
| S-03 | 连接后无新事件 | 每 ~15s 收到 heartbeat，无 event 行 |
| S-04 | 同标签重连（断开再 EventSource） | 只推重连后的新事件，不重放历史 |
| S-05 | Local 模式 | 只 heartbeat，不推事件 |

### 2.2 前端
| ID | 用例 | 期望 |
|---|---|---|
| F-01 | 打开 Discussion 列表，他人新建话题 | 列表即时出现（无需手动刷新） |
| F-02 | 打开话题详情，他人/agent 回复 | 楼层增量追加，正在输入的内容不清空、无整页闪动 |
| F-03 | 无新消息 | 页面完全静止 |
| F-04 | SSE 断线（后端重启） | EventSource 自动重连，恢复后继续收新事件；60s 兜底补拉不遗漏 |
| F-05 | 关闭话题 | 详情刷新为已关闭态（隐藏输入） |

## 3. 测试策略
| 层 | 手段 |
|---|---|
| 后端 SSE | curl `-N` 对 running console：造事件断言流内容 / 心跳 / 401（新增 `tests/console-sse-test.sh`） |
| 前端 | 浏览器手工 E2E（F-01..05）；node 语法检查；J 级 |
| 回归 | `cargo test` + `tests/ui-api-test.sh` + `tests/discussion-rpc-test.sh` |

## 4. 入口 / 出口
- `cargo test -p teamx` 全绿。
- `tests/console-sse-test.sh` 通过（curl 断言 SSE 帧）。
- 前端语法 + 手工 E2E。
