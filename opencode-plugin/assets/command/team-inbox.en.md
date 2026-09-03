---
description: teamx show my unified inbox (tasks + questions + unread nudges)
agent: teamx
---

User wants to see their unified inbox. Call teamx_inbox (ack defaults to true, i.e. mark nudges as read).

Show the grouped result:
- tasks: my open tasks (not done/verified) assigned to me / my role
- questions: open questions directed at me
- nudges: unread team nudges (offline/restart backlog) with the server cursor last_seq

Use ack=false if the user wants to keep them unread for later.
