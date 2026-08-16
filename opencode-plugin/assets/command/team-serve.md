---
description: teamx 网络模式 serve（start/status/stop/token）
agent: teamx
---

用户要管理 teamx 网络模式的内嵌服务器。根据 $ARGUMENTS 路由：

- `start [--addr <ip>] [--port <port>]` → teamx_serve_start（spawn 本地 teamx serve，幂等；返回 server URL）
- `status` → teamx_serve_status（查询运行状态）
- `stop` → teamx_serve_stop（停止子进程）
- `token <member_id>` → teamx_serve_token（签发成员连接 token，N2 才支持）

先调用 teamx_sync 确认状态。若用户输入 `serve` 无子参数，默认执行 status。启动后展示 server URL 并提示：其他成员配置 TEAMX_SERVER_URL=<url> 后即可连接（N0 阶段用 session key 连接）。

用户输入: $ARGUMENTS
