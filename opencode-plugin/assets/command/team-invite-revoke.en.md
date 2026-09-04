---
description: teamx revoke invitation (owner/co-lead)
agent: teamx
---

User (owner/co-lead) wants to revoke an invitation. Parameters: $ARGUMENTS (invitation id). Call teamx_team_invite_revoke (id).

Key points:
- After revocation, the client certificate issued by this invitation is rejected on connect; online members will have their WebSocket connection actively disconnected.
- It is recommended to call teamx_team_invite_list first to confirm the invitation id to revoke.
