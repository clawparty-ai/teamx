---
description: teamx view current team status
agent: teamx
---

User wants to view teamx team status. Call teamx_sync first to pull latest events, then call teamx_status to display full status (team/goal/members/roles/pending questions). If the current session belongs to multiple teams, use --team to specify the target team. Additional parameters: $ARGUMENTS

If pending members are found (state is pending), **only prompt, never auto-approve**: list the pending members and explain that the owner can execute `approve <member_id>` or `deny <member_id>` to decide. Never call teamx_approve / teamx_deny on your own.

After displaying the status, output an ASCII flowchart at the end (in a ```text code block) to visually explain how teamx team collaboration works. Do not use mermaid. Follow the reference style below: horizontal pipeline, owner/member dual swimlanes, states marked with [ brackets ] for team/goal state machine, arrows ──► for main flow, │ ▼ ▲ for branches, with optional Chinese notes at line end. Replace the [ state ] markers with the actual current team state, and output a "● Current Phase" line at the end indicating the current phase meaning and suggested next step. Keep alignment, single line no more than 80 characters.

Reference style (the `[states]` are placeholders; replace with actual states; update the ● line based on actual state):

```text
                    teamx Collaboration Flowchart
                    ═════════════════════════════════════════════

   owner:  [create_team] ──► [set_goal] ──► [share_goal] ──► [close_goal] ──► [archive]
           (forming)          (proposed)    (in_progress)    (achieved)       (archived)
              │                  │               │    ▲            │               │
              ▼                  ▼               ▼    │            ▼               ▼
   member:  invite_token      join         [active]◄──┘        [achieved]      [completed]
             share token     [pending]          │ collaboration   (member reports) (owner verifies)
                                             ▼
                              progress / ask / respond / decision / update
                              blocked / resumed / refine

   ● Current Phase: forming (recruiting) — goal not yet shared, next step: /team goal share
```
