---
description: teamx publish/broadcast (progress/decision/update/blocked/resumed/achieved/refine; optional --assignee for targeted dispatch)
agent: teamx
---

User wants to publish a teamx event. Event type: $1, optional data: $2. Call teamx_publish (type=$1, data=$2 or {"message":...}). Valid types: progress / decision / update / blocked / resumed / achieved / refine / start / activity.

**Targeted dispatch (assignee)**: When the owner wants to explicitly assign a task to a specific member, pass assignee (member id):
- `teamx_publish {type: "decision", data: {message: "Please complete X"}, assignee: "<member_id>"}`
- Events with assignee are tagged with `assignee_member_id`/`assignee_name` in the payload; **only that member auto-executes**; other members receive a normal broadcast without auto-execution.
- Publish without assignee is a normal broadcast (informational) and does not trigger any member auto-execution.
