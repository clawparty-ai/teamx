# teamx 任务委派模式：需求分析、设计方案与测试

> 本文档是 taskx（内置任务文档类型）委派能力的设计依据。配套使用手册见 [27-taskx.cn.md](27-taskx.cn.md)。
> 状态：**已实现**（direct / bid / broadcast 三模式）

---

## 一、需求分析

### 1.1 背景

teamx 用 `taskx` 文档承载团队任务。早期只有"指定单一成员"（direct）一种委派方式。实际协作中 team lead 面临三种不同的任务分发诉求：

1. **明确责任人**——"这个 bug 就交给张三"；
2. **谁合适谁上**——"评审这活儿，团队里谁有空谁做"，成员自组织；
3. **全员行动**——"所有 tester 都要跑一遍回归"，每个人对自己的结果负责。

仅靠"指定单一成员"无法表达后两种诉求。

### 1.2 需求陈述

| 编号 | 需求 | 期望行为 |
|---|---|---|
| R1 | 指定成员委派 | lead 指定某成员，任务明确归属该成员 |
| R2 | 角色抢单委派 | lead 按角色派发，该角色所有成员可见；成员可抢单，先到先得 |
| R3 | 退单 | 抢到单的成员（或其用户）因故不做，可退单；退单后任务重新开放，无黑名单 |
| R4 | lead 重新广播 | lead 可对开放任务重新广播，通知角色成员"又可抢了" |
| R5 | 全员广播委派 | lead 按角色派发，该角色每个成员各得一份独立任务，各自闭环 |
| R6 | 人/机执行标记 | 任务标注 executor（either/agent/human），决定"谁执行"（与"谁接单"解耦） |

### 1.3 关键概念界定

- **接单（claim）≠ 执行（executor）**：
  - 接单：决定"这个任务归谁"（bid 模式通过抢单确定）；
  - 执行：决定"实际干活的是人还是 AI"（executor 字段）。
  - 无论 executor 是什么，bid 任务都由 agent 完成抢单（agent 代表用户 session 接单）；`executor=human` 时 agent 抢单只是为用户预订，用户执行，没时间可退单。

- **退单 ≠ 黑名单**：
  - 退单是"这单先不做"，任务回到开放池；
  - 任何成员（含刚退单的）都可再次抢单，无惩罚语义。

- **角色匹配**：只有角色匹配 `assignee_role` 的成员（或 lead）能抢单，避免跨角色抢单。

---

## 二、设计方案

### 2.1 数据模型（DocMeta 扩展）

```rust
pub struct DocMeta {
    // ...现有字段（doc/id/state/owner/history/assignee/executor/priority）...
    pub assign_mode: Option<String>,    // "direct" | "bid" | "broadcast"
    pub assignee_role: Option<String>,  // bid/broadcast 的目标角色
}
```

- `assign_mode=direct`：`assignee` 固定为指定成员；
- `assign_mode=bid`：`assignee` 创建时为空，claim 时写入抢单者；
- `assign_mode=broadcast`：每成员一个实例，`assignee` 各自固定。

### 2.2 状态机

```
assigned → acked → claimed → in_progress → done → verified
             │        │
             ├─(小任务可跳过 claimed)→ in_progress
             └→ help_requested（通知型，状态不变）
claimed → retracted → assigned（退单，重新开放）
done → rejected → assigned（打回）
```

`doc.retracted` 归入 **Backward** 事件（doc_flow.rs），`claimed → assigned` 合法。

### 2.3 命令设计

| 命令 | 底层事件 | 权限 |
|---|---|---|
| `task create --assignee <m>` | `doc.created`（direct） | lead |
| `task create --role <r>` | `doc.created`（bid，assignee 空） | lead |
| `task create --role <r> --mode broadcast` | `doc.created` × N（每成员实例） | lead |
| `task claim <id>` | `doc.claimed`（assignee=抢单者） | 角色匹配成员或 lead |
| `task retract <id>` | `doc.retracted`（assignee 清空）+ 重新广播 | 抢单者本人或 lead |
| `task re-bid <id>` | `doc.rebid` 广播 | lead |
| `task done/verify/reject/...` | 复用现有 | 见 27-taskx |

### 2.4 核心实现逻辑

**创建（cmd_task::Create）**：
- 根据 `--assignee`/`--role`/`--mode` 推导 `assign_mode`；
- `direct`：1 实例，assignee 锁定；
- `bid`：1 实例，assignee 空，`assignee_role` 记录角色；
- `broadcast`：枚举角色活跃成员，每人生成 `taskx/<id>@<member-prefix>.meta.json`。

**抢单（task_claim）**：
- 校验：bid 模式、state=assigned、assignee 为空、角色匹配（或 lead）；
- 写 `doc.claimed`，payload 带 `assignee_member_id=抢单者`；
- 原子性：`db::with_write` 事务内读-判-写，并发抢单只有一个成功。

**退单（task_retract）**：
- 权限：抢单者本人 或 lead；
- 写 `doc.retracted`（backward，claimed→assigned），payload 空 assignee；
- 重新广播 `doc.rebid` 给角色成员。

**角色推进授权（cmd_publish_doc::effective_role）**：
- assignee、角色匹配成员、lead 三者任一即获 advance 授权（绕过 `can_advance` 的角色限制）。

### 2.5 plugin 行为

| 事件 | plugin 动作 |
|---|---|
| `doc.created`（bid，角色匹配） | toast"新任务可认领"；executor≠human 时 agent 自动 claim |
| `doc.claimed`（他人） | digest 更新为"已被 X 认领"，不再显示可抢 |
| `doc.retracted` / `doc.rebid` | digest 显示"任务可认领"，agent 可再次 claim |
| `doc.created`（broadcast 实例） | assignee=我 → 自动 ack + 按 executor 执行 |

---

## 三、测试方案

### 3.1 单元测试（Rust）

| 用例 | 覆盖 |
|---|---|
| `taskx_retract_returns_claimed_to_assigned` | retracted 分类 + claimed→assigned 合法 + 非授权者被拒 |
| `builtin_taskx_spec_loads_without_disk_file` | 内置 spec fallback |
| `taskx_state_machine_advances_and_rejects` | 完整状态机 + backward |
| `nudge_open_tasks_*` | 任务 nudge 触发/去重/跳过已完成 |

### 3.2 集成测试（CLI 端到端）

使用隔离 `TEAMX_HOME`/`TEAMX_DB` + 临时 git 仓库，验证真实命令流。

**场景 A：bid 抢单闭环**
1. lead 创建团队 + 2 个 reviewer 成员；
2. `task create --role reviewer`（bid）；
3. 校验 meta：`assignee=None, assign_mode=bid, assignee_role=reviewer`；
4. reviewer A `task claim` → 成功，assignee=A；
5. reviewer B `task claim` → 被拒（已抢）；
6. A `task done` → owner `task verify` → verified；
7. 断言：状态机完整，事件台账可追溯。

**场景 B：退单与重新抢单**
1. A 抢单；
2. A `task retract` → assigned、assignee 清空；
3. B 再抢 → 成功（无黑名单）；
4. lead `task re-bid` → 返回广播成功。

**场景 C：broadcast 全员**
1. `task create --role reviewer --mode broadcast`；
2. 断言：生成 N 个实例（`@<member>` 后缀），每实例 assignee 各自；
3. 每个 reviewer 对自己的实例 `task done` → 各自 verified。

**场景 D：权限**
1. 非 reviewer 成员 claim bid 任务 → 被拒（角色不匹配）；
2. 非抢单者 retract → 被拒；
3. lead retract 任意任务 → 成功。

### 3.3 并发测试

两个 reviewer 同时 `task claim` 同一任务 → 断言恰好一个成功（原子性）。

### 3.4 回归

- 全量单测（105+）全绿；
- Windows target 编译通过；
- 现有 direct 任务流程不回归（`--assignee` 隐含 direct）。

---

## 四、测试案例（可执行）

```bash
# 环境准备（隔离）
export TEAMX_HOME=/tmp/tx-test/home TEAMX_DB=/tmp/tx-test/t.db
TEAMX=target/debug/teamx
git init -q . && git config user.email t@t && git config user.name t

# 建团队 + 2 个 reviewer
$TEAMX team create "抢单组" --session s:o --goal-title "g" >/dev/null
TID=$($TEAMX team status --session s:o --json | python3 -c \
  'import json,sys; print(json.load(sys.stdin)["teams"][0]["team"]["id"])')
sqlite3 t.db "INSERT INTO members(id,team_id,session_key,display_name,role,state,joined_at)
  VALUES('rev-1','$TID','s:rev1','评审A','reviewer','active','now'),
        ('rev-2','$TID','s:rev2','评审B','reviewer','active','now');"

# 案例 1：bid 创建（assignee 空）
$TEAMX task create "评审代码" --role reviewer --id r1 --no-push --session s:o
python3 -c "import json; m=json.load(open('.teamx/docs/taskx/r1.meta.json')); \
  assert m['assign_mode']=='bid' and m['assignee'] is None and m['assignee_role']=='reviewer'; print('C1 OK')"

# 案例 2：抢单 + 重复抢单被拒
$TEAMX task claim r1 --session s:rev1
$TEAMX task claim r1 --session s:rev2 2>&1 | grep -q "already claimed" && echo "C2 OK"

# 案例 3：退单 + 重新抢单（无黑名单）
$TEAMX task retract r1 --session s:rev1
$TEAMX task claim r1 --session s:rev2 && echo "C3 OK"

# 案例 4：broadcast 全员
$TEAMX task create "全员评审" --role reviewer --mode broadcast --id br --no-push --session s:o
ls .teamx/docs/taskx/br@*.meta.json | wc -l | grep -q 2 && echo "C4 OK"

# 案例 5：权限（非角色成员 claim 被拒）
$TEAMX task create "专属任务" --role tester --id t1 --no-push --session s:o
$TEAMX task claim t1 --session s:rev1 2>&1 | grep -q "may not claim" && echo "C5 OK"

# 清理
rm -rf /tmp/tx-test
```

---

## 五、设计取舍记录

| 决策 | 理由 |
|---|---|
| bid 默认（`--role` 即 bid） | 与"角色派发"直觉一致；direct 用 `--assignee` 显式表达 |
| broadcast 每成员独立实例 | 复用 DocMeta 单状态机，避免共享实例的并发冲突 |
| 退单无黑名单 | 退单是"不做"，非惩罚；黑名单会增加心智负担且难撤销 |
| claim 需角色匹配 | 防止跨角色抢单，保持分派意图 |
| 抢单由 agent 完成 | 与"agent 代表用户 session"的模型一致；human 通过退单表达意愿 |
| retract 权限（抢单者/lead） | 平衡"成员自决"与"lead 管控" |
