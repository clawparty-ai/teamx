---
description: teamx create team (become owner)
agent: teamx
---

User wants to create a team via teamx. Team name: $ARGUMENTS.

Steps:
1. Call teamx_create_team (name is $ARGUMENTS)
2. Show the returned invite_token to the user, explaining it needs to be shared with members
3. Ask the user if they want to immediately teamx_set_goal to draft the team goal
