---
description: teamx 反向隧道 - 列出本团队已暴露的服务
agent: teamx
---

用户要查看本团队已暴露的反向隧道。调用 teamx_tunnel_list。展示每个隧道：名称、server 公开端口、provider LAN IP。若没有隧道，提示用户用 `/team tunnel expose` 暴露。额外参数: $ARGUMENTS
