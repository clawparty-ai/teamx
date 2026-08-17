---
description: teamx join team (via invite_token, requires owner approval)
agent: teamx
---

User wants to join a teamx team. Parameters: $ARGUMENTS (should be invite_token and --name display_name).

Steps:
1. Parse invite_token (and optional --name display_name; ask if not provided)
2. Call teamx_join (token, name)
3. Inform the user that the pending application has been submitted and requires owner approval; after approval they can use /team role set to choose a role
