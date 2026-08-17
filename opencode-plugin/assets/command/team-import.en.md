---
description: teamx import invitation letter (member unpacks mTLS certificate + claims pending seat)
agent: teamx
---

User wants to import an invitation letter. Parameters: $ARGUMENTS (letter single line `teamx-inv:v1:...` or .json file path, optional `--name <display_name>`). Call teamx_team_import (letter, name).

Key points:
- On success: mTLS certificate/private key is saved to `~/.teamx/letters/<invitation_id>/`, seat becomes pending, waiting for owner approval.
- With a local shared DB, this completes in one step (save certificate + claim seat); cross-machine setups only save locally, requiring `TEAMX_SERVER_URL` to be set for the plugin to complete the claim on the server.
- Tip: for real-time push, set `TEAMX_SERVER_URL=https://<owner_lan_ip>:5781` and restart opencode.
