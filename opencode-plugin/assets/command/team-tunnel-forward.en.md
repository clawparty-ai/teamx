---
description: teamx reverse tunnel - map a teammate's exposed service to a local port (consumer)
agent: teamx
---

User wants to map a teammate's reverse-tunnel service to a local port (consumer-side local forward). Parameters: $ARGUMENTS (format: `--name <tunnel-name> [--local-port <local-port>]`). Call teamx_tunnel_forward (name, local_port).

Notes:
- Requires network mode (TEAMX_SERVER_URL set)
- Default local port = the provider's target port (e.g. 8081); if taken, a random candidate is returned and needs user confirmation before binding
- After success, access `http://127.0.0.1:<local-port>/` just like a local service (bytes bridged over a mTLS WS through the server to the provider)
- Listens on `127.0.0.1` only (does not expose the consumer machine)
- The forward is persistent; tell the user when no longer needed (auto-restored after an opencode restart)
