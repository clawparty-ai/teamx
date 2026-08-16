---
description: teamx 启动网络模式服务器
agent: teamx
---

用户要启动 teamx 网络模式服务器。参数: $ARGUMENTS（可选 --addr/--port/--db）。调用 teamx_serve_start。幂等：已在运行时直接返回状态。启动成功后展示 server URL，并提示成员配置 TEAMX_SERVER_URL=<url> 连接。
