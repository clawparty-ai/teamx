---
description: teamx 反向隧道 - 把队友暴露的服务映射到本地端口（consumer）
agent: teamx
---

用户要把队友通过反向隧道暴露的服务映射到本机端口（消费端本地转发）。参数: $ARGUMENTS（格式: `--name <隧道名> [--local-port <本地端口>]`）。调用 teamx_tunnel_forward（name, local_port）。

要点：
- 需要网络模式（TEAMX_SERVER_URL 已设置）
- 默认本地端口 = provider 的 target 端口（如 8081）；若被占用会返回随机候选端口，需要用户确认后才绑定
- 成功后访问 `http://127.0.0.1:<本地端口>/` 就如同访问本地服务（字节经 mTLS WS 经 server 桥接到 provider）
- 监听地址固定 `127.0.0.1`（仅本机可访问），不会暴露消费端机器
- 转发是常驻的；不需要时告知用户关闭（重启 opencode 会自动恢复）
