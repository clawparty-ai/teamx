---
description: teamx soft-destroy a team (owner, hide + keep data)
agent: teamx
---

User wants to destroy a teamx team. Run teamx_sync + teamx_status first to confirm you are the owner, then call teamx_team_destroy. Destroy is a **soft delete**: the team is marked destroyed, hidden from all member lists, and its outstanding invitations are revoked, but the data (members/events/goals/roles) is preserved for audit. This is irreversible — confirm with the user before executing. Additional parameters: $ARGUMENTS
