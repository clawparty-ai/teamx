# teamx Dual-Window Demo Manual Test Guide (Manual Test)

> This file is the companion test runbook for `docs/12-demo-overview.md`: **"I" manually start two opencode windows**; Window A acts as the team lead creating a team and producing a design plan, Window B acts as a reviewer joining and reviewing the plan; the whole collaboration goes through the teamx `/Team` agent + `teamx_*` tools.

## 0. Pre-flight Checks (30 Seconds)

```bash
which teamx          # should print ~/.local/bin/teamx (the plugin finds the CLI via PATH)
cd ~/github/teamx && ./tests/smoke.sh   # core CLI self-check: all PASS means ledger/state machine are fine
```

- If smoke passes fully → any problem must be in the plugin/model layer, narrowing the search scope.
- If the `/Team` command doesn't exist → re-run `./install.sh` and restart opencode.

## 1. Start Two Windows

```bash
cd ~/github/teamx && ./demo/start.sh   # opens two Terminals, each entering demo/workspace and launching opencode
```

(Or manually: open two terminals, both running `cd ~/github/teamx/demo/workspace && opencode`. **Both windows must be in the same directory** so requirement.md / design-plan.md / review-plan.md can be shared.)

## 2. Window A (Owner)

### ① Create Team + Goal

Type:

```
/Team 创建一个团队，名字叫「看板设计团队」。目标：根据当前目录下的 requirement.md 需求，产出一份「轻量任务看板」的设计方案，写入 design-plan.md。
```

✅ You should see: the agent calls `teamx_create_team` → returns an **invite_token** (copy it) + `teamx_set_goal`.

### ② Design Plan + Broadcast

Type:

```
阅读 requirement.md，开始设计，完成后把方案写入 design-plan.md，然后用 teamx 广播"设计方案已完成，请 reviewer 评审"。
```

✅ You should see: the agent reads the requirements with read → writes design-plan.md with write → `teamx_publish decision` (hints it has broadcast). Confirm the file: `ls demo/workspace/` should show `design-plan.md`.

## 3. Window B (Reviewer)

### ③ Join + Request Role

Type:

```
/Team 加入团队，invite_token 是 <从窗口A复制的token>，我叫评审员，申请 reviewer 角色。
```

✅ You should see: `teamx_join` → hint **pending, waiting for owner approval**; `teamx_set_role reviewer`.

## 4. Back to Window A

### ④ Approve + Share Goal

Type:

```
/Team 审批新加入的成员，然后把目标分享给成员。
```

✅ You should see: `teamx_approve` (member becomes active) + `teamx_share_goal` (goal=shared, team=active).

## 5. Back to Window B

### ⑤ Sync + Review + Report

Type:

```
/Team 同步团队最新状态，然后读取 design-plan.md 进行评审，把改进意见写入 review-plan.md，并向团队汇报评审结论。
```

✅ You should see: `teamx_sync` returns the owner's "plan complete" broadcast → read design-plan.md → write review-plan.md → `teamx_publish progress`.

## 6. Back to Window A

### ⑥ Adopt + Iterate + Close

Type:

```
/Team 同步评审意见，阅读 review-plan.md，采纳合理的改进并更新 design-plan.md，用 teamx 广播处理结果，最后关闭目标。
```

✅ You should see: sync → read review-plan.md → update design-plan.md → `teamx_publish decision` → `teamx_close_goal`.

## 7. Final Verification (Any Third Terminal)

```bash
# team_id comes from the output returned when Window A created the team
teamx team status --team <team_id> --json
# you should see team.state=completed, goal.state=closed
teamx events --team <team_id> --json
# the event chain should contain (by seq):
# team.created → goal.set → membership.pending → member.role_set → membership.approved
#   → goal.shared → team.state_changed → decision.broadcast(plan complete)
#   → progress.published(review) → decision.broadcast(disposition)
#   → goal.state_changed(close) → team.completed
ls ~/github/teamx/demo/workspace/   # should contain design-plan.md and review-plan.md
```

## 8. Troubleshooting Common Issues

| Symptom | Cause | Fix |
|---|---|---|
| `/Team` command not found | opencode not restarted after install | restart and try again |
| Agent can't find teamx / "spawn ENOENT" | `~/.local/bin` not on PATH | run `export PATH="$HOME/.local/bin:$PATH"` then restart opencode |
| Window B approval fails "not pending" | old binary (missing approval fix) | re-run `./install.sh` |
| Window B can't see the owner's broadcast | hasn't run `teamx_sync` yet (V1 has no push, protocol-based) | have B's agent run `teamx_sync` first (already required by its prompt) |
| Agent stuck at permission confirmation | model needs to read/write files or run commands | allow it in the window (Always/Once) |
| publish errors that data isn't JSON | model passed data as a bare string | have it use the `{"message": "..."}` format, or ignore it (the core flow doesn't depend on data) |

## 9. Test Record

- Date: ____
- Result: □ All passed　□ Partially passed (issues: __________)
- Event chain complete: □ Yes　□ No (differences: __________)
- Artifacts generated: design-plan.md □　review-plan.md □
