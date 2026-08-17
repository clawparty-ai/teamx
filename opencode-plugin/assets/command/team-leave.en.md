---
description: teamx leave a team
agent: teamx
---

User wants to leave a teamx team. Run teamx_sync + teamx_status first to confirm the current team, then call teamx_leave. After leaving, the session is no longer a team member and its membership cache is invalidated. Note: the **owner cannot leave** (there is no ownership transfer mechanism); if the owner wants out, use `/team destroy`. Additional parameters: $ARGUMENTS
