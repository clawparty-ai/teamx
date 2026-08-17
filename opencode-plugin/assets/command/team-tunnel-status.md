---
description: teamx 反向隧道 - 查看单个隧道状态（公开端口/直连判断）
agent: teamx
---

用户要查看某个反向隧道的状态。参数: $ARGUMENTS（隧道名）。调用 teamx_tunnel_status。展示：公开端口、provider LAN IP、目标端口。若需要判断能否直连，对比本机来源 IP 与 LAN IP 是否同网段。额外参数: $ARGUMENTS
