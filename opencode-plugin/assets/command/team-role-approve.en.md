---
description: teamx approve custom role (owner, auto-grants to proposer)
agent: teamx
---

User (owner) wants to approve a custom role. Parameters: $ARGUMENTS (role key). Call teamx_role_approve (role=$ARGUMENTS). After approval the role enters the catalog and is automatically granted to the proposer. If the role does not exist or is not in proposed state, show an error.
