---
description: teamx reverse tunnels (expose/list/status/close - expose a local service to teammates)
agent: teamx
---

User wants to manage a teamx reverse tunnel (expose a local service to team members, network mode). Route by $ARGUMENTS:

- `expose --name <n> --port <p> [--lan-ip <ip>]` → teamx_tunnel_expose (provider side; opens a persistent WS tunnel, returns the public port on the server)
- `list` → teamx_tunnel_list (list tunnels exposed by the current team: public port + provider LAN IP)
- `status <name>` → teamx_tunnel_status (inspect one tunnel: public port, LAN IP, same-subnet direct-connect hint)
- `close <name>` → teamx_tunnel_close (close a tunnel, freeing its public port)

Prerequisites:
- Network mode: TEAMX_SERVER_URL must be set (connect to teamx serve)
- expose requires the local service already running on the local port

Scenario: the developer (member-b) runs a local service and exposes it with `tunnel expose`; the tester (member-a) accesses it via the public port; if on the same subnet, direct access via the LAN IP is possible (`tunnel status` hints at it).

User input: $ARGUMENTS
