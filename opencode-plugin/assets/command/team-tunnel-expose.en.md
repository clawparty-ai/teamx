---
description: teamx reverse tunnel - expose a local service to teammates (provider)
agent: teamx
---

User wants to expose a local service to team members (reverse tunnel). Parameters: $ARGUMENTS (format: `--name <tunnel-name> --port <local-port> [--mode local|frp] [--lan-ip <lan-ip>]`). Call teamx_tunnel_expose (name, port, mode, lan_ip).

Notes:
- Requires network mode (TEAMX_SERVER_URL set)
- The local service must already be running on --port
- **Mode (default local)**:
  - `local` (default): the server binds no public port (more secure); teammates use `/team tunnel forward` to map a local port
  - `frp`: the server binds a public port (tcp://<server>:<port>) that anyone can connect to directly
- Same-subnet teammates can reach the LAN IP directly (auto-detected by default)
- The tunnel is persistent; close it with `/team tunnel close <name>` when no longer needed
