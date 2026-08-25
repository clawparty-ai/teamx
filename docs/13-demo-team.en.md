# teamx Three-Person Collaboration Demo: Design + Implementation + Review

> Scenario: **"I" manually start three opencode windows**, forming a three-person team in teamx to complete the full collaboration loop of "requirements → design → review → finalization":
> - **Window A (owner)**: creates the team, drafts the goal, approves members, coordinates clarifications, broadcasts decisions, closes/archives.
> - **Window B (contributor, designer)**: joins the team, requests the contributor role, produces the design plan `design-plan.md`, reports progress.
> - **Window C (reviewer)**: joins the team, requests the reviewer role, reviews the plan, produces `review-plan.md`, reports the review conclusion.

Compared with the two-person demo, the three-person version adds **multi-member approval**, **two execution roles (contributor/reviewer) collaborating in parallel**, and **cross-sync between members relayed through the owner**, better showcasing teamx's team semantics.

## 0. Prerequisites

1. `./install.sh` has been run and opencode restarted (the `/Team` command is available).
2. All three windows run opencode **inside the `demo/workspace/` directory** (sharing `requirement.md` and output files).
3. Launch: `./demo/start.sh 3` (opens three Terminal windows).

## 1. Data Flow (Three Nodes)

```
┌── Window A: owner ──────────────┐      ┌── Window B: contributor ─────┐
│ /Team agent ─ teamx_* tools     │      │ /Team agent ─ teamx_* tools  │
└──────────────┬──────────────────┘      └──────────────┬───────────────┘
               │ spawn `teamx <cmd>`                    │ spawn `teamx <cmd>`
               ▼                                        ▼
        ┌─────────────────────────────────────────────────────┐
        │   teamx CLI → SQLite ledger ~/.teamx/teamx.db (single source of truth)│
        └─────────────────────────────────────────────────────┘
               ▲                                        ▲
┌──────────────┴──────────────────┐      ┌──────────────┴───────────────┐
│ /Team agent ─ teamx_* tools     │      │ workspace/ shared files:     │
└── Window C: reviewer ───────────┘      │  requirement.md / design-plan.md│
                                         │  / review-plan.md            │
```

- **Collaboration plane (ledger)**: owner broadcasts, member reports, and clarification Q&A all land in `events`; each agent runs `teamx_sync` to pull increments before acting.
- **Artifact plane (files)**: the contributor writes `design-plan.md`; the reviewer reads it and writes `review-plan.md`; the owner consolidates and updates.

## 2. Flow (Operations Across Three Windows)

### Window A (Owner): Create Team + Goal

```
/Team 创建团队「产品评审组」。目标：根据当前目录 requirement.md 完成「轻量任务看板」产品方案设计，并经 reviewer 评审定稿。
```

Expected: `teamx_create_team` → returns an **invite_token** (copy for B and C) + `teamx_set_goal`.

### Window B (Contributor): Join + Request Role

```
/Team 加入团队，invite_token 是 <token>，我叫设计者，申请 contributor 角色。
```

Expected: `teamx_join` (pending) + `teamx_set_role contributor` (stays pending awaiting approval).

### Window C (Reviewer): Join + Request Role

```
/Team 加入团队，invite_token 是 <token>，我叫评审员，申请 reviewer 角色。
```

Expected: `teamx_join` (pending) + `teamx_set_role reviewer` (stays pending).

### Window A (Owner): Approve Both Members + Share Goal

```
/Team 审批所有待审批成员，然后把目标分享给成员。
```

Expected: `teamx_approve` × 2 (members active, roles retained) + `teamx_share_goal` (team active).

### Window B (Contributor): Design + Report

```
/Team 同步团队状态，阅读 requirement.md 完成设计方案写入 design-plan.md，然后向团队汇报「设计方案完成」。
```

Expected: `teamx_sync` → read requirements → write `design-plan.md` → `teamx_publish progress`.

### Window C (Reviewer): Review + Report

```
/Team 同步团队状态，读取 design-plan.md 进行评审，把改进意见写入 review-plan.md，然后向团队汇报「评审完成」。
```

Expected: `teamx_sync` (sees B's progress) → read the plan → write `review-plan.md` → `teamx_publish progress`.

### Window A (Owner): Clarify + Adopt + Coordinate

```
/Team 同步进展。向设计者提一个澄清问题，得到答复后，采纳评审意见并广播处理结果。
```

Expected: `teamx_sync` → `teamx_ask` (target member becomes waiting) → wait for B's reply → `teamx_publish decision`.

### Window B (Contributor): Answer Clarification + Report Completion

```
/Team 同步状态，回答 owner 的澄清问题，然后报告目标已达成。
```

Expected: `teamx_respond` → `teamx_publish achieved`.

### Window A (Owner): Close + Archive

```
/Team 验证并关闭目标，然后归档团队。
```

Expected: `teamx_close_goal` (team completed) → `teamx_archive` (team archived).

## 3. Verification (Any Terminal)

```bash
teamx team status --team <team_id> --json     # team=archived, goal=closed
teamx events --team <team_id> --json          # full event chain
ls demo/workspace/                            # requirement.md / design-plan.md / review-plan.md
```

**Expected event chain** (by seq):
`team.created → goal.set → membership.pending×2 → member.role_set×2 → membership.approved×2 → goal.shared → team.state_changed → progress.published(design) → progress.published(review) → clarification.asked → clarification.responded → decision.broadcast(adopted) → goal.achieved → goal.state_changed(close) → team.completed → team.state_changed(archive)`

## 4. Talking Points

| Stage | What to say |
|---|---|
| Multi-member approval | One owner approves two pending members; each keeps their requested role |
| Parallel roles | The contributor produces, the reviewer reviews — two execution lines synced via the ledger |
| Clarification closed loop | owner→member ask/respond explicitly toggles waiting→active |
| Archiving | completed → archived is the end of the full lifecycle |

## 5. Automated Equivalent Verification

The three-person flow already has a CLI-level automated test, `tests/three-member.sh` (no real model needed; drives the same event chain), invoked by `tests/run-all.sh`.
