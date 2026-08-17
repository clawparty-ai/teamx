---
description: teamx reverse tunnel - expose a local service to teammates (provider)
agent: teamx
---

User wants to expose a local service to team members (reverse tunnel). Parameters: $ARGUMENTS (format: `--name <tunnel-name> --port <local-port> [--lan-ip <lan-ip>]`). Call teamx_tunnel_expose (name, port, lan_ip).

Notes:
- Requires network mode (TEAMX_SERVER_URL set)
- The local service must already be running on --port
- Returns the public port on the server; show it to the user to share with teammates (e.g. tcp://<server>:<public-port>)
- Same-subnet teammates can reach the LAN IP directly (auto-detected by default)
- The tunnel is persistent; close it with `/team tunnel close <name>` when no longer needed
