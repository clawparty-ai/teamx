---
description: teamx invite member (owner issues mTLS client certificate + invitation letter)
agent: teamx
---

User wants to invite a member as owner (network mode). Parameters: $ARGUMENTS (format: `"<role>: <description>"`, optional `--name-hint <name>` `--server-url <url>`). Call teamx_team_invite (role_desc, name_hint, server_url).

Key points:
- `--server-url` must be the **LAN IP** of the owner's machine (e.g. https://192.168.1.5:5781), not 127.0.0.1, otherwise members cannot connect.
- Returns a single-line invitation letter (`teamx-inv:v1:...`), prompt the user to send it to the target member via a secure channel; the member imports it with `/team import`.
- Each invitation corresponds to one member seat (pending); after import the member still needs owner `approve` before they can work.
- Chinese role labels auto-derive a role key (like `role-<hex>`); ASCII labels (e.g. reviewer) are used directly as the key.

Call teamx_sync first to confirm you are the owner and know the server URL (can check with /team serve status).
