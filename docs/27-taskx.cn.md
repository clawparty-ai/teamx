# teamx 任务（taskx）：以文档为中心的团队任务机制

> 中英双语：本文件为中文版；English version: [taskx.en.md](27-taskx.en.md)

---

## 一、什么是 taskx

**taskx** 是 teamx 内置的任务文档类型。它让 team lead 可以把任务"派发"给指定成员或角色，成员以文档为中心完成任务、提交成果，lead 验收闭环——整个过程有状态机、有审计、有回执、有 git 版本控制。

taskx 的设计原则是 **以文档为中心**：任务就是一份文档，而不是数据库里的一条记录。

- **内容在 git**：`.teamx/docs/taskx/<id>.md` 是任务本体（目标、验收标准、进展、结果），随仓库版本化，谁改谁提交一目了然；
- **状态在 `.meta.json`**：每个任务的当前状态、assignee、executor、优先级和完整流转历史都记录在 `.teamx/docs/taskx/<id>.meta.json`；
- **流转在 ledger**：每一次状态变化（创建、回执、认领、完成、验收…）都是一条审计事件，团队全员可见、可追溯。

**不需要在 TEAM.md 里声明**——taskx 是内置文档类型，任何团队开箱即用。

## 二、任务生命周期

```
assigned → acked → claimed → in_progress → done → verified
             │        │
             ├─(小任务可跳过 claimed)→ in_progress
             └→ help_requested（求助，状态不变）→ in_progress
claimed → retracted → assigned（退单，重新开放抢单）
done → rejected →（打回）→ assigned / in_progress
```

| 状态 | 含义 |
|---|---|
| `assigned` | 任务已发布。direct/broadcast 下指定了 assignee；bid 下等待成员抢单 |
| `acked` | 成员已自动回执（确认收到） |
| `claimed` | （bid）成员已抢单 |
| `in_progress` | 执行中 |
| `done` | 成员提交完成，待 lead 验收 |
| `verified` | lead 验收通过，闭环 |
| `help_requested` | 成员求助（通知 lead，状态不变） |

## 三、任务委派模式（assign_mode）

taskx 支持三种委派方式，决定"任务给谁做"：

| 模式 | 指定方式 | 谁做 | 实例数 |
|---|---|---|---|
| **direct（指派）** | `--assignee <member>` | 指定的成员 | 1 |
| **bid（抢单，默认）** | `--role <role>` | 该角色成员竞争，先到先得 | 1 |
| **broadcast（广播）** | `--role <role> --mode broadcast` | 该角色每个成员各做一份 | 每成员 1 个 |

### 抢单模式（bid）

`task create --role reviewer`（默认即 bid）：

1. lead 发布任务，**不指定具体成员**（assignee 为空），广播给该角色所有成员；
2. 角色匹配的成员（或 lead）可 `task claim <id>` **抢单**——先到先得，抢到即写入 assignee；
3. 抢单后正常执行（claim → done → verified）；
4. 若抢到单的成员（或其用户）没时间做，可 `task retract <id>` **退单**——任务回到 `assigned` 开放池，**无黑名单**，任何成员（含刚退单的）都可再抢；
5. lead 可 `task re-bid <id>` 重新广播，让角色成员知道又可抢了。

> 退单权限：抢单者可退自己的单；**lead 可退任意单**。退单不是惩罚——只是"这单先不做"，不影响后续抢单。

### 广播模式（broadcast）

`task create --role reviewer --mode broadcast`：

- 为角色每个成员各创建一个独立实例（id 带 `@<member>` 后缀，如 `review@rev-1`）；
- 每个成员对自己的实例负责，各自独立走状态机、各自闭环；
- `task list --role <r>` 可聚合查看全员进度。

## 四、命令一览

```bash
# lead 派发任务（三种委派模式）
teamx task create "修复登录 bug" --assignee <member_id>              # direct
teamx task create "评审代码" --role reviewer                         # bid（默认）
teamx task create "全员评审" --role reviewer --mode broadcast        # broadcast
# 可选参数：--executor either|agent|human（默认 either）、--priority、--id、--detail

# 成员操作
teamx task ack <id>                 # 自动回执（plugin 通常自动完成）
teamx task claim <id>               # 抢单（bid 模式，先到先得）
teamx task retract <id>             # 退单（抢单者或 lead）
teamx task update <id> --progress "已完成 60%"
teamx task help <id> --reason "依赖第三方 API 文档"
teamx task done <id> --result "已修复并测试通过"

# lead 验收 / 重新广播
teamx task verify <id>              # 验收闭环
teamx task reject <id> --reason "缺少边界用例"
teamx task re-bid <id>              # 重新广播（任务已开放时）

# 查看
teamx task list [--mine] [--state <s>] [--executor either|agent|human]
teamx task log <id>                 # 完整审计历史
```

## 五、人 / 机任务区分

taskx 用 `executor` 字段区分任务由谁执行，有三种委派类型：

| executor | 语义 | 成员端行为 |
|---|---|---|
| **`either`**（默认） | 人或 agent 都可以做 | agent 自动执行，digest 显示 👤🤖，同时提醒用户"可随时接管" |
| **`agent`** | 必须由 AI 执行 | agent 自动执行，digest 显示 🤖 |
| **`human`** | 必须由人来执行（硬约束） | **不自动执行**，appendPrompt 提醒用户；agent 被明确告知【不得】代替用户执行，digest 显示 👤 |

这样 lead 可以明确区分"机器必须做的活"、"必须人来做的活"、"谁做都行"——默认 `either` 让 agent 高效推进，同时保留用户随时接管的可能；`human` 则硬性防止 AI 抢跑需要人工判断的任务。

### 命令示例

```bash
teamx task create "修复登录 bug" --assignee <member_id>            # 默认 either
teamx task create "训练模型" --assignee <member_id> --executor agent
teamx task create "签署合同" --assignee <member_id> --executor human
```

## 六、自动回执

当 `doc.created`（taskx）事件到达成员且 `assignee_member_id` 是当前成员时，opencode 插件**自动调用 `teamx task ack`** 回执，无需用户操作。回执写入 ledger（`doc.acknowledged`），lead 可以看到任务已被成员接收。

## 七、完成与验收

1. 成员 `teamx task done <id> --result "..."` → 任务进入 `done`，事件 `doc.done` 通过 reactions 定向通知 lead；
2. lead 在 digest / 通知中看到"任务已完成待验收"；
3. lead 检查任务文档 + git 产物 → `teamx task verify <id>` → 闭环；
4. 若不合格，lead `teamx task reject <id> --reason "..."` 打回，任务回到 `assigned`。

## 八、求助与协作

执行中受阻时，成员可 `teamx task help <id> --reason "..."`：
- 写 `doc.help_requested` 事件（**状态不变**，任务仍在当前状态）；
- reactions 定向通知 lead；
- lead 查看任务文档和求助原因，通过 `task update` 或 `ask` 回应，或直接改派。

## 九、todo 提醒（nudge）

server 的 nudge 任务会定期扫描未完成任务：
- 对每个有未完成任务（`assigned/acked/claimed/in_progress`）的 assignee，发送 `task.nudge` 定向事件；
- 成员的 opencode 会话收到后：digest 显示"我的任务"，appendPrompt 唤醒（agent 任务继续做，human 任务提醒用户）。

这样即使成员会话中途停下，server 也会提醒它继续推进未完成的任务。

## 十、git 集成

每次任务事件（创建/认领/进展/完成/验收…）后，teamx 默认**自动 `git commit + push`** 任务文档变更；用 `--no-push` 可关闭自动提交（改由成员手动 `git commit`）。任务文档在 git 里的历史就是任务的完整内容演化。

## 十一、团队视角

- **全员透明**：任务、状态、assignee、executor 都在团队 ledger 和 git 中，谁都能查；
- **审计可追溯**：`task log <id>` 展示每一次状态流转（谁、何时、什么事件）；
- **可统计**：`task list` 可按状态/executor/assignee 过滤，配合 nudge 事件可用于团队进度管理。

## 快速上手

```bash
# 1. lead 派发（假设 owner 就是执行者）
teamx task create "写周报" --assignee <member_id> --executor agent --session <s>

# 2. 成员侧（plugin 自动回执后）
teamx task list --mine --session <s>      # 看我的待办
teamx task update <id> --progress "..."   # 进展
teamx task done <id> --result "已完成"    # 完成

# 3. lead 验收
teamx task verify <id> --session <s>
```
