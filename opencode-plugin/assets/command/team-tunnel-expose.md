---
description: teamx 反向隧道 - 暴露本地服务给队友（provider）
agent: teamx
---

用户要把本机服务暴露给团队成员（反向隧道）。参数: $ARGUMENTS（格式: `--name <隧道名> --port <本地端口> [--lan-ip <局域网IP>]`）。调用 teamx_tunnel_expose（name, port, lan_ip）。

要点：
- 需要网络模式（TEAMX_SERVER_URL 已设置）
- 本地服务必须已在 --port 上运行
- 返回 server 上的公开端口，展示给用户用于告知队友（如 tcp://<server>:<公开端口>）
- 同网段队友可直连 LAN IP（默认自动探测本机局域网 IP）
- 隧道是常驻的；不再需要时用 `/team tunnel close <name>` 关闭
