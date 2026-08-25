# teamx 三人协作 Demo：设计 + 实现 + 评审

> 场景：**"我"手动启动三个 opencode 窗口**，在 teamx 里组成一个三人团队，完成"需求 → 方案设计 → 评审 → 定稿"的完整协作闭环：
> - **窗口 A（owner）**：创建团队、起草目标、审批成员、协调澄清、广播决策、关闭/归档。
> - **窗口 B（contributor，设计者）**：加入团队、申请 contributor 角色、产出设计方案 `design-plan.md`、汇报进展。
> - **窗口 C（reviewer，评审员）**：加入团队、申请 reviewer 角色、评审方案、产出 `review-plan.md`、汇报评审结论。

对比二人 demo：三人版多了**多成员审批**、**两类执行角色（contributor/reviewer）并行协作**、**成员间通过 owner 中转的交叉同步**，更能体现 teamx 的团队语义。

## 0. 前置条件

1. 已执行 `./install.sh` 并重启 opencode（`/Team` 命令可用）。
2. 三个窗口**都在 `demo/workspace/` 目录**运行 opencode（共享 `requirement.md` 与产出文件）。
3. 启动：`./demo/start.sh 3`（打开三个 Terminal 窗口）。

## 1. 数据流（三节点）

```
┌── 窗口 A：owner ────────────────┐      ┌── 窗口 B：contributor ──────┐
│ /Team agent ─ teamx_* 工具      │      │ /Team agent ─ teamx_* 工具   │
└──────────────┬──────────────────┘      └──────────────┬───────────────┘
               │ spawn `teamx <cmd>`                    │ spawn `teamx <cmd>`
               ▼                                        ▼
        ┌─────────────────────────────────────────────────────┐
        │   teamx CLI → SQLite 账本 ~/.teamx/teamx.db（唯一权威）│
        └─────────────────────────────────────────────────────┘
               ▲                                        ▲
┌──────────────┴──────────────────┐      ┌──────────────┴───────────────┐
│ /Team agent ─ teamx_* 工具      │      │ workspace/ 共享文件：           │
└── 窗口 C：reviewer ─────────────┘      │  requirement.md / design-plan.md│
                                        │  / review-plan.md               │
```

- **协作平面（账本）**：owner 广播、成员汇报、澄清问答都落 `events`；每个 agent 行动前 `teamx_sync` 拉增量。
- **产物平面（文件）**：contributor 写 `design-plan.md`，reviewer 读之并写 `review-plan.md`，owner 汇总后更新。

## 2. 流程（三窗口操作）

### 窗口 A（owner）：建队 + 目标

```
/Team 创建团队「产品评审组」。目标：根据当前目录 requirement.md 完成「轻量任务看板」产品方案设计，并经 reviewer 评审定稿。
```

预期：`teamx_create_team` → 返回 **invite_token**（抄给 B、C）+ `teamx_set_goal`。

### 窗口 B（contributor）：加入 + 申请角色

```
/Team 加入团队，invite_token 是 <token>，我叫设计者，申请 contributor 角色。
```

预期：`teamx_join`（pending）+ `teamx_set_role contributor`（保持 pending 待审批）。

### 窗口 C（reviewer）：加入 + 申请角色

```
/Team 加入团队，invite_token 是 <token>，我叫评审员，申请 reviewer 角色。
```

预期：`teamx_join`（pending）+ `teamx_set_role reviewer`（保持 pending）。

### 窗口 A（owner）：审批两人 + 分享目标

```
/Team 审批所有待审批成员，然后把目标分享给成员。
```

预期：`teamx_approve` × 2（成员 active，角色保留）+ `teamx_share_goal`（team active）。

### 窗口 B（contributor）：设计 + 汇报

```
/Team 同步团队状态，阅读 requirement.md 完成设计方案写入 design-plan.md，然后向团队汇报「设计方案完成」。
```

预期：`teamx_sync` → 读需求 → 写 `design-plan.md` → `teamx_publish progress`。

### 窗口 C（reviewer）：评审 + 汇报

```
/Team 同步团队状态，读取 design-plan.md 进行评审，把改进意见写入 review-plan.md，然后向团队汇报「评审完成」。
```

预期：`teamx_sync`（看到 B 的 progress）→ 读方案 → 写 `review-plan.md` → `teamx_publish progress`。

### 窗口 A（owner）：澄清 + 采纳 + 协调

```
/Team 同步进展。向设计者提一个澄清问题，得到答复后，采纳评审意见并广播处理结果。
```

预期：`teamx_sync` → `teamx_ask`（目标成员变 waiting）→ 等 B 答复 → `teamx_publish decision`。

### 窗口 B（contributor）：答复澄清 + 报告完成

```
/Team 同步状态，回答 owner 的澄清问题，然后报告目标已达成。
```

预期：`teamx_respond` → `teamx_publish achieved`。

### 窗口 A（owner）：关闭 + 归档

```
/Team 验证并关闭目标，然后归档团队。
```

预期：`teamx_close_goal`（team completed）→ `teamx_archive`（team archived）。

## 3. 验证（任意终端）

```bash
teamx team status --team <team_id> --json     # team=archived, goal=closed
teamx events --team <team_id> --json          # 完整事件链
ls demo/workspace/                            # requirement.md / design-plan.md / review-plan.md
```

**预期事件链**（按 seq）：
`team.created → goal.set → membership.pending×2 → member.role_set×2 → membership.approved×2 → goal.shared → team.state_changed → progress.published(设计) → progress.published(评审) → clarification.asked → clarification.responded → decision.broadcast(采纳) → goal.achieved → goal.state_changed(close) → team.completed → team.state_changed(archive)`

## 4. 讲解要点

| 环节 | 讲什么 |
|---|---|
| 多成员审批 | 一个 owner 审批两个 pending 成员，角色各自保留 |
| 并行角色 | contributor 产出、reviewer 评审，两条执行线经账本同步 |
| 澄清闭环 | owner→member 的 ask/respond 显式置 waiting→active |
| 归档 | completed → archived 是完整生命周期终点 |

## 5. 自动化等价验证

三人流程已有 CLI 级自动化测试 `tests/three-member.sh`（不依赖真实模型，跑通同一事件链），由 `tests/run-all.sh` 调用。
