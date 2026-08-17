---
description: teamx choose role (built-in or approved custom role)
agent: teamx
---

User wants to choose a teamx role for the current session. Role: $ARGUMENTS. Call teamx_set_role (role=$ARGUMENTS). If the role is not in the catalog, list available roles for the user to choose from (including custom roles). Note: unapproved custom roles are not available; the owner must first `role approve` them.

Other role-related commands:
- `/team role propose <key> <label> [desc]` (or `/team-role-propose`) — member proposes custom role
- `/team role approve <key>` (or `/team-role-approve`) — owner approves
- `/team role deny <key>` (or `/team-role-deny`) — owner denies
- `/team role update <key> [--label] [--description]` (or `/team-role-update`) — owner modifies role description
