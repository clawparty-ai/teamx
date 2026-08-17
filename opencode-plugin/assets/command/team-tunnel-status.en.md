---
description: teamx reverse tunnel - inspect one tunnel (public port / direct-connect hint)
agent: teamx
---

User wants to inspect a reverse tunnel's status. Parameters: $ARGUMENTS (tunnel name). Call teamx_tunnel_status. Show: public port, provider LAN IP, target port. To judge direct connectivity, compare the requester's source IP with the LAN IP for the same subnet. Additional parameters: $ARGUMENTS
