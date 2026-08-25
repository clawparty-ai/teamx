# teamx Three-Person Collaboration Manual Test (owner + contributor + reviewer)

> This is a **step-by-step executable** test runbook (corresponding to test case **TM-04**). You play three members using three opencode windows, walking the full flow of "create team → join → approve → design → review → clarify → finalize → archive", then verify the ledger with the CLI at the end.

## 0. Prerequisites

1. `./install.sh` has been run (Rust CLI + the three plugin pieces installed) and opencode has been **restarted** (the `/Team` command takes effect).
2. Verify the commands work:
   ```bash
   which teamx          # should print ~/.local/bin/teamx
   teamx --version      # prints a version number
   ```
3. Launch three windows (all in the same `demo/workspace` directory):
   ```bash
   cd ~/github/teamx && ./demo/start.sh 3
   ```
   Three Terminals open: **Window A=owner, B=contributor (designer), C=reviewer**.
   All three share `~/.teamx/teamx.db` (global DB) and the files under `demo/workspace/`.

## 1. Test Steps (Strictly in Order, Interleaved Across Three Windows)

### Step 1｜Window A: Create Team + Goal

Type:

```
/Team 创建一个团队，名字叫「产品评审组」。目标：根据当前目录 requirement.md 完成「轻量任务看板」产品方案设计，并由 reviewer 评审定稿。
```

✅ Expected:
- The agent calls `teamx_create_team`; the reply contains an **invite_token** (a 32-character string) — **copy it for later**.
- The goal is `proposed` (drafted), and you are the owner.

> Record: team id = ________, invite_token = ________

### Step 2｜Window B: Join + Request Contributor

Type (replace `<token>` with the token from Step 1):

```
/Team 加入团队，invite_token 是 <token>，我叫设计者，申请 contributor 角色。
```

✅ Expected: `teamx_join` → hint **pending, waiting for owner approval**; `teamx_set_role contributor` (still pending).

### Step 3｜Window C: Join + Request Reviewer

Type:

```
/Team 加入团队，invite_token 是 <token>，我叫评审员，申请 reviewer 角色。
```

✅ Expected: `teamx_join` → pending; `teamx_set_role reviewer` (still pending).

### Step 4｜Window A: Approve Both Members + Share Goal

Type:

```
/Team 审批所有待审批的成员，然后把目标分享给成员。
```

✅ Expected: `teamx_approve` × 2 (members become active, roles retained) + `teamx_share_goal` (team=active, goal=shared).

### Step 5｜Window B: Design Plan + Report

Type:

```
/Team 同步团队状态，阅读 requirement.md 完成设计方案写入 design-plan.md，然后向团队汇报「设计方案完成，请评审」。
```

✅ Expected: `teamx_sync` → read requirements → generate `design-plan.md` → `teamx_publish progress`.
File check: `ls demo/workspace/` should show `design-plan.md`.

### Step 6｜Window C: Review + Report

Type:

```
/Team 同步团队状态，读取 design-plan.md 进行评审，把改进意见写入 review-plan.md，然后向团队汇报「评审完成」。
```

✅ Expected: `teamx_sync` shows B's progress → read the plan → generate `review-plan.md` → `teamx_publish progress`.

### Step 7｜Window A: Clarify + Adopt

Type:

```
/Team 同步进展，向设计者提一个澄清问题，得到答复后采纳评审意见并广播处理结果。
```

✅ Expected: `teamx_sync` → `teamx_ask` (the designer becomes waiting; you get a question id) → **wait for B to answer first** (you may see "waiting for member response" at this point).

### Step 8｜Window B: Answer + Report Completion

Type:

```
/Team 同步状态，回答 owner 的澄清问题，然后报告目标已达成。
```

✅ Expected: `teamx_respond` (back to active) → `teamx_publish achieved` (goal=achieved).

### Step 9｜Window A: Close + Archive

Type:

```
/Team 验证并关闭目标，然后归档团队。
```

✅ Expected: `teamx_close_goal` (team=completed) → `teamx_archive` (team=archived).

## 2. Verification (Any Third Terminal)

Replace `<team_id>` with the team id recorded in Step 1:

```bash
# 1) Status: should be archived / closed
teamx team status --team <team_id> --json

# 2) Audit replay (member names resolved)
teamx log --team <team_id>

# 3) Artifacts
ls ~/github/teamx/demo/workspace/   # should contain requirement.md / design-plan.md / review-plan.md
```

✅ Expected event chain (`teamx log`, ascending by seq; order should be roughly):

```
team.created → goal.set → membership.pending ×2 → member.role_set ×2
→ membership.approved ×2 → goal.shared → team.state_changed
→ progress.published(design) → progress.published(review)
→ clarification.asked → clarification.responded
→ decision.broadcast(adopted) → goal.achieved
→ goal.state_changed(close) → team.completed → team.state_changed(archive)
```

## 3. Troubleshooting Common Issues

| Symptom | Cause | Fix |
|---|---|---|
| `/Team` isn't a command | opencode not restarted after install | restart opencode |
| Agent says it can't find teamx / spawn fails | `~/.local/bin` not on PATH | run `export PATH="$HOME/.local/bin:$PATH"` then restart |
| Step 4 approval fails "not pending" | old binary | re-run `./install.sh` |
| B/C can't see each other's progress | no `teamx_sync` yet (V1 has no push, protocol-based) | have the respective window's agent run `teamx_sync` first |
| Agent stuck at permission confirmation | model needs to read/write files or run commands | click Allow in the window (Always) |
| publish errors that data isn't JSON | model passed a bare string | just ignore it (automatically falls back to message) |
| Owner cannot `leave` | intentional restriction (prevents orphaned teams) | wrap up with `teamx_close_goal` + `teamx_archive` |

## 4. Test Record

- Date: ________
- Tester: ________
- Result: □ All passed　□ Partially passed (failed step: ________)
- Final state: team=________ / goal=________ (expected archived / closed)
- Event chain complete: □ Yes　□ No (differences: ________)
- Artifacts: design-plan.md □　review-plan.md □

---

> Equivalent automated verification: the script that runs the same event chain without a real model is `tests/three-member.sh` (run directly via `./tests/three-member.sh`). To confirm the underlying logic first, run it before following this runbook.
