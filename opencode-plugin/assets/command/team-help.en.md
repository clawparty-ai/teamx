---
description: teamx help (list all subcommands and flat aliases)
agent: teamx
---

List all teamx subcommands and flat alias commands with brief descriptions. Additional parameters: $ARGUMENTS

Role command details (highlights):
- Built-in roles: owner/observer/supervisor/contributor/subtask-implementer/reviewer, any member can directly `role set <role>` to choose.
- Custom roles: `role propose <key> <label> [desc]` to propose your own job role (must differ from built-in roles) → owner `role approve <key>` to approve (auto-grants to proposer) or `role deny <key>` to deny → owner can use `role update <key> --description ...` to modify any role description.

After the subcommand list, output an ASCII flowchart of the teamx collaboration flow (in a ```text code block) to help understand how it works. Follow the reference style below: horizontal pipeline, owner/member dual swimlanes, states marked with [ brackets ] for team/goal state machine, arrows ──► for main flow, │ ▼ ▲ for branches, with optional Chinese notes at line end. Replace the [ state ] markers with the actual current team state (can call teamx_status first to check), and output a "● Current Phase" line at the end indicating the current phase meaning and suggested next step. Keep alignment, single line no more than 80 characters.

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
