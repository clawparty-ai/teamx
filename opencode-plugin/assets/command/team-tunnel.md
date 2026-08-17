---
description: teamx 反向隧道（expose/list/status/close，暴露本地服务给队友）
agent: teamx
---

用户要管理 teamx 反向隧道（把本地服务暴露给团队成员，网络模式）。根据 $ARGUMENTS 路由：

- `expose --name <n> --port <p> [--lan-ip <ip>]` → teamx_tunnel_expose（provider 侧，打开持久 WS 隧道，返回 server 上的公开端口）
- `list` → teamx_tunnel_list（列出本团队已暴露的隧道：公开端口 + provider LAN IP）
- `status <name>` → teamx_tunnel_status（查看单个隧道：公开端口、LAN IP、是否同网段可直连）
- `close <name>` → teamx_tunnel_close（关闭隧道，释放公开端口）

前置条件：
- 网络模式：TEAMX_SERVER_URL 必须已设置（连接 teamx serve）
- expose 需要本机服务已在本地端口运行

场景示例：开发者（member-b）运行了本地服务，用 `tunnel expose` 暴露；测试员（member-a）通过公开端口访问；若同网段可用 LAN IP 直连（`tunnel status` 会提示）。

用户输入: $ARGUMENTS
