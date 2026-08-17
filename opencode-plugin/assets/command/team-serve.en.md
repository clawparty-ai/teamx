---
description: teamx network mode serve (start/status/stop/token)
agent: teamx
---

User wants to manage the teamx network mode embedded server. Route based on $ARGUMENTS:

- `start [--addr <ip>] [--port <port>]` → teamx_serve_start (spawn local teamx serve, idempotent; returns server URL)
- `status` → teamx_serve_status (query running status)
- `stop` → teamx_serve_stop (stop subprocess)
- `token <member_id>` → teamx_serve_token (issue member connection token, only supported in N2)

Call teamx_sync first to confirm status. If user types `serve` with no sub-parameters, default to status. After starting, show the server URL and prompt: other members can connect after setting TEAMX_SERVER_URL=<url> (N0 phase connects using session key).

User input: $ARGUMENTS
