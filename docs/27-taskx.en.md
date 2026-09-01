# teamx Tasks (taskx): Document-Centric Team Task Mechanism

> Bilingual: this is the English version; 中文版见 [taskx.cn.md](27-taskx.cn.md)

---

## 1. What is taskx

**taskx** is teamx's built-in task document type. It lets a team lead assign tasks to a specific member or role; members complete tasks document-centrically and submit results, and the lead verifies to close the loop — all with a state machine, audit trail, acknowledgements, and git versioning.

taskx is built on the principle of **document-centricity**: a task *is* a document, not a database row.

- **Content lives in git**: `.teamx/docs/taskx/<id>.md` is the task body (goals, acceptance criteria, progress, result), versioned with the repository — who changed what is visible at a glance;
- **State lives in `.meta.json`**: the current state, assignee, executor, priority, and full transition history of each task live in `.teamx/docs/taskx/<id>.meta.json`;
- **Transitions live in the ledger**: every state change (create, acknowledge, claim, done, verify…) is an auditable event visible to the whole team.

**No TEAM.md declaration needed** — taskx is a built-in document type, available out of the box on any team.

## 2. Task lifecycle

```
assigned → acked → claimed → in_progress → done → verified
             │        │
             ├─(small tasks may skip claimed)→ in_progress
             └→ help_requested (blocked; state unchanged) → in_progress
claimed → retracted → assigned (retract; reopened for bidding)
done → rejected →（sent back）→ assigned / in_progress
```

| State | Meaning |
|---|---|
| `assigned` | Task published. direct/broadcast: assignee fixed; bid: awaiting a claim |
| `acked` | Member auto-acknowledged (confirmed receipt) |
| `claimed` | (bid) a member claimed the task |
| `in_progress` | In execution |
| `done` | Member submitted; awaiting lead verification |
| `verified` | Lead verified; loop closed |
| `help_requested` | Member requested help (notifies lead; state unchanged) |

## 3. Task delegation modes (assign_mode)

taskx supports three delegation modes that decide "who does the task":

| Mode | Specified by | Who does it | Instances |
|---|---|---|---|
| **direct (assign)** | `--assignee <member>` | the named member | 1 |
| **bid (claim, default)** | `--role <role>` | role members compete, first-come-first-served | 1 |
| **broadcast** | `--role <role> --mode broadcast` | every role member does their own copy | one per member |

### Bid mode

`task create --role reviewer` (bid is the default for `--role`):

1. the lead publishes the task **without a fixed member** (assignee empty), broadcast to all members of the role;
2. any role-matching member (or a lead) runs `task claim <id>` to **claim** — first come, first served; the claimer becomes the assignee;
3. execution proceeds normally (claim → done → verified);
4. if the claimer (or their user) has no time, they run `task retract <id>` to **retract** — the task returns to `assigned` (open pool), **no blacklist**: any member (including the one who just retracted) may claim again;
5. a lead can run `task re-bid <id>` to re-broadcast so role members know it is claimable again.

> Retract permission: the claimer may retract their own claim; **a lead may retract any**. Retracting is not a penalty — it simply means "not doing this one for now" and does not affect later claims.

### Broadcast mode

`task create --role reviewer --mode broadcast`:

- creates one independent instance per role member (id suffixed `@<member>`, e.g. `review@rev-1`);
- each member owns their instance, runs its own state machine, and closes it independently;
- `task list --role <r>` aggregates the whole-team progress.

## 4. Commands

```bash
# Lead dispatches (three delegation modes)
teamx task create "Fix login bug" --assignee <member_id>              # direct
teamx task create "Review code" --role reviewer                      # bid (default)
teamx task create "Review all" --role reviewer --mode broadcast      # broadcast
# Optional: --executor either|agent|human (default either), --priority, --id, --detail

# Member operations
teamx task ack <id>                 # auto-ack (the plugin usually does this)
teamx task claim <id>               # claim (bid mode, first-come-first-served)
teamx task retract <id>             # retract (claimer or lead)
teamx task update <id> --progress "60% done"
teamx task help <id> --reason "blocked on third-party API docs"
teamx task done <id> --result "fixed and tested"

# Lead verification / re-broadcast
teamx task verify <id>              # close the loop
teamx task reject <id> --reason "missing edge-case tests"
teamx task re-bid <id>              # re-broadcast (when the task is open)

# Viewing
teamx task list [--mine] [--state <s>] [--executor either|agent|human]
teamx task log <id>                 # full audit history
```

## 5. Human / agent tasks

taskx uses the `executor` field to mark who performs a task, with three delegation types:

| executor | Meaning | Member-side behavior |
|---|---|---|
| **`either`** (default) | A human or an agent may do it | The agent auto-executes; digest shows 👤🤖 and the user is reminded "you can take over at any time" |
| **`agent`** | Must be done by an AI | The agent auto-executes; digest shows 🤖 |
| **`human`** | Must be done by a person (hard constraint) | **No auto-execute**; appendPrompt reminds the user; the agent is explicitly told it **MUST NOT** take over; digest shows 👤 |

This lets the lead clearly separate "what machines must do", "what must be done by a human", and "either is fine". The default `either` lets an agent push work forward efficiently while keeping the user able to take over; `human` hard-prevents the AI from running ahead on tasks that need human judgment.

### Examples

```bash
teamx task create "Fix login bug" --assignee <member_id>            # default either
teamx task create "Train model" --assignee <member_id> --executor agent
teamx task create "Sign contract" --assignee <member_id> --executor human
```

## 6. Auto-acknowledgement

When a `doc.created` (taskx) event reaches a member and `assignee_member_id` is the current member, the opencode plugin **automatically calls `teamx task ack`** — no user action needed. The acknowledgement is written to the ledger (`doc.acknowledged`), so the lead sees the task has been received.

## 7. Completion and verification

1. Member runs `teamx task done <id> --result "..."` → task moves to `done`; the `doc.done` event notifies the lead via reactions;
2. The lead sees "task completed, awaiting verification" in the digest / notifications;
3. The lead reviews the task doc + git artifacts → `teamx task verify <id>` → loop closed;
4. If unsatisfactory, the lead runs `teamx task reject <id> --reason "..."` and the task returns to `assigned`.

## 8. Help and collaboration

When blocked mid-execution, a member can run `teamx task help <id> --reason "..."`:
- writes a `doc.help_requested` event (**state unchanged**; the task stays where it is);
- reactions notify the lead;
- the lead reviews the task doc and the reason, responds via `task update` or `ask`, or reassigns.

## 9. TODO nudges

The server's nudge pass periodically scans unfinished tasks:
- for every assignee with open tasks (`assigned/acked/claimed/in_progress`), it emits a `task.nudge` directed event;
- the member's opencode session reacts: the digest shows "my tasks", and appendPrompt wakes it (agent tasks continue; human tasks remind the user).

So even if a member session stops mid-way, the server reminds it to keep pushing its unfinished tasks.

## 10. Git integration

After each task event (create/claim/progress/done/verify…), teamx **auto `git commit + push`** by default; use `--no-push` to disable auto-commit (the member then commits manually). The git history of the task document is the complete content evolution of the task.

## 11. Team perspective

- **Full transparency**: tasks, states, assignees, and executors live in the team ledger and git — anyone can look them up;
- **Auditable**: `task log <id>` shows every transition (who, when, which event);
- **Measurable**: `task list` filters by state/executor/assignee; combined with nudge events it supports team progress management.

## Quick start

```bash
# 1. Lead dispatches (assuming the owner is the executor)
teamx task create "Write weekly report" --assignee <member_id> --executor agent --session <s>

# 2. Member side (plugin auto-acks, then)
teamx task list --mine --session <s>      # see my open tasks
teamx task update <id> --progress "..."   # progress
teamx task done <id> --result "done"      # complete

# 3. Lead verifies
teamx task verify <id> --session <s>
```
