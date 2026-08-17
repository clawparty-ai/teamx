---
description: teamx start network mode server
agent: teamx
---

User wants to start the teamx network mode server. Parameters: $ARGUMENTS (optional --addr/--port/--db). Call teamx_serve_start. Idempotent: returns status directly if already running. After successful start, show server URL and prompt members to set TEAMX_SERVER_URL=<url> to connect.
