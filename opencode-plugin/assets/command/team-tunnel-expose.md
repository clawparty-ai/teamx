---
description: teamx 反向隧道 - 暴露本地服务给队友（provider）
agent: teamx
---

用户要把本机服务暴露给团队成员（反向隧道）。参数: $ARGUMENTS（格式: `--name <隧道名> --port <本地端口> [--mode local|frp] [--lan-ip <局域网IP>]`）。调用 teamx_tunnel_expose（name, port, mode, lan_ip）。

要点：
- 需要网络模式（TEAMX_SERVER_URL 已设置）
- 本地服务必须已在 --port 上运行
- **模式（默认 local）**：
  - `local`（默认）：server 不暴露任何端口，更安全；队友用 `/team tunnel forward` 在本地映射端口访问
  - `frp`：server 暴露公开端口（tcp://<server>:<端口>），队友直接连接即可
- 同网段队友可直连 LAN IP（默认自动探测本机局域网 IP）
- 隧道是常驻的；不再需要时用 `/team tunnel close <name>` 关闭
