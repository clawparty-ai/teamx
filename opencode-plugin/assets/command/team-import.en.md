---
description: teamx import invitation letter (member unpacks mTLS certificate + claims pending seat)
agent: teamx
---

User wants to import an invitation letter. Parameters: $ARGUMENTS (letter single line `teamx-inv:v1:...` or .json file path, optional `--name <display_name>`). Call teamx_team_import (letter, name).

Key points:
- On success: mTLS certificate/private key is saved to `~/.teamx/letters/<invitation_id>/`, seat becomes pending, waiting for owner approval.
- With a local shared DB, this completes in one step (save certificate + claim seat); cross-machine setups only save locally, requiring a server connection to complete the claim.
- **Auto-connect**: the letter embeds its server URL; the plugin discovers it automatically on startup and enters network mode (no need to set `TEAMX_SERVER_URL` manually). The first RPC auto-claims the seat via `team.import` and retries.
- Tip: for real-time push, restart opencode after importing the letter; the plugin auto-connects to `https://<owner_lan_ip>:5781`.
