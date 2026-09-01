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
done → rejected →（sent back）→ assigned / in_progress
```

| State | Meaning |
|---|---|
| `assigned` | Lead has dispatched; awaiting member receipt |
| `acked` | Member auto-acknowledged (confirmed receipt) |
| `claimed` | Member claimed (optional; for larger tasks) |
| `in_progress` | In execution |
| `done` | Member submitted; awaiting lead verification |
| `verified` | Lead verified; loop closed |
| `help_requested` | Member requested help (notifies lead; state unchanged) |

## 3. Commands

```bash
# Lead dispatches (specific member or role; executor marks human/agent)
teamx task create "Fix login bug" --assignee <member_id> --executor agent
teamx task create "Review design doc" --assignee <member_id> --executor human --priority high

# Member operations
teamx task ack <id>                 # auto-ack (the plugin usually does this)
teamx task claim <id>               # claim (optional)
teamx task update <id> --progress "60% done"
teamx task help <id> --reason "blocked on third-party API docs"
teamx task done <id> --result "fixed and tested"

# Lead verification
teamx task verify <id>              # close the loop
teamx task reject <id> --reason "missing edge-case tests"

# Viewing
teamx task list [--mine] [--state <s>] [--executor agent|human]
teamx task log <id>                 # full audit history
```

## 4. Human / agent tasks

taskx uses the `executor` field to mark who performs a task:

- **`executor=agent`** (default): an AI session can execute it. An opencode member auto-acknowledges on receipt, the task appears in `task list --mine`, shows as 🤖 in the digest, and auto-execute drives the agent to start working;
- **`executor=human`**: needs a person. An opencode member auto-acknowledges but does **not** auto-execute — the user is prompted "there is a task that needs a human", and it shows as 👤 in the digest.

This lets the lead clearly separate "what machines can do" from "what must be done by a human", preventing the AI from running ahead on tasks that need human judgment.

## 5. Auto-acknowledgement

When a `doc.created` (taskx) event reaches a member and `assignee_member_id` is the current member, the opencode plugin **automatically calls `teamx task ack`** — no user action needed. The acknowledgement is written to the ledger (`doc.acknowledged`), so the lead sees the task has been received.

## 6. Completion and verification

1. Member runs `teamx task done <id> --result "..."` → task moves to `done`; the `doc.done` event notifies the lead via reactions;
2. The lead sees "task completed, awaiting verification" in the digest / notifications;
3. The lead reviews the task doc + git artifacts → `teamx task verify <id>` → loop closed;
4. If unsatisfactory, the lead runs `teamx task reject <id> --reason "..."` and the task returns to `assigned`.

## 7. Help and collaboration

When blocked mid-execution, a member can run `teamx task help <id> --reason "..."`:
- writes a `doc.help_requested` event (**state unchanged**; the task stays where it is);
- reactions notify the lead;
- the lead reviews the task doc and the reason, responds via `task update` or `ask`, or reassigns.

## 8. TODO nudges

The server's nudge pass periodically scans unfinished tasks:
- for every assignee with open tasks (`assigned/acked/claimed/in_progress`), it emits a `task.nudge` directed event;
- the member's opencode session reacts: the digest shows "my tasks", and appendPrompt wakes it (agent tasks continue; human tasks remind the user).

So even if a member session stops mid-way, the server reminds it to keep pushing its unfinished tasks.

## 9. Git integration

After each task event (create/claim/progress/done/verify…), teamx **auto `git commit + push`** by default; use `--no-push` to disable auto-commit (the member then commits manually). The git history of the task document is the complete content evolution of the task.

## 10. Team perspective

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
