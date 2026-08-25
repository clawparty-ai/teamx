# teamx V1 Spec

teamx 是一个团队协作状态内核（Rust CLI + SQLite）+ opencode 插件。V1 面向单机本地：所有 opencode 会话与 teamx 在同一台机器上，无网络服务。

## 架构

- **CLI-only**：不启动 server。插件每次调用 spawn `teamx <cmd> --session <key> --json` 子进程。
- **全局存储**：单一 SQLite 库 `~/.teamx/teamx.db`（可用 `TEAMX_DB` 覆盖）。WAL 模式 + `busy_timeout` 5s + 单写者事务，支持多进程并发读、串行写。
- **事件账本**：所有状态变更写入 `events`（append-only），每 team 内 `seq` 单调递增且与 INSERT 同事务，杜绝并发乱序。Team/Member/Goal 当前状态是对账本的投影。
- **session_key** = `<实例UUID>:<opencode sessionID>`；实例 UUID 持久化在 `~/.teamx/instance.json`。

## 状态机

| 对象 | 状态 |
|---|---|
| Team | `forming → active → blocked → completed → archived` |
| Member | `pending → active → waiting → idle → left`（另有 `denied` 终止态） |
| Goal | `proposed → shared → refining → in_progress → blocked → achieved → closed` |

> `paused` 态已移除（与 `blocked` 语义冗余且不可达）；暂停工作请用 `publish blocked`。

合法转换在 `src/state.rs` 中以 (from, action) → to 显式表驱动；非法转换直接报错，不写入任何事件。

## Schema（SQLite）

```
teams(id, name, owner_member_id, goal_id, state, invite_token, created_at, updated_at)
members(id, team_id, session_key, display_name, role, state, loopx_project, last_seen_at, joined_at, left_at)
    └─ UNIQUE(team_id, session_key)：一个 session 每团队仅一行成员（leave/deny 后重入复用该行）
goals(id, team_id, title, body, state, created_at, updated_at)
    └─ UNIQUE(team_id)：一团队仅一个目标
roles(id, team_id, key, label, description, permissions_json, state, proposed_by)   -- state: proposed/approved（默认 approved）；自定义角色由成员 propose、owner 审批
events(id, team_id, member_id, seq, type, payload_json, created_at)  -- 账本
questions(id, team_id, asker_member_id, target_member_id, question, answer, state, created_at, answered_at)
sync_cursors(session_key, team_id, last_seq)                    -- 每会话增量游标（单调推进）
```

> `sessions` 冗余表已在 v3 迁移中移除（`members(session_key, team_id)` 已覆盖其全部信息且此前只写不读）。

## 事件类型

`team.created` `team.state_changed` `team.completed` `membership.pending` `membership.approved` `membership.denied` `member.role_set` `member.state_changed` `member.left` `goal.set` `goal.updated` `goal.shared` `goal.state_changed` `goal.achieved` `progress.published` `clarification.asked` `clarification.responded` `loopx.progress` `decision.broadcast` `role.proposed` `role.approved` `role.denied` `role.updated`

## CLI 命令

```
teamx init
teamx team create <name> --session <key> [--goal-title T] [--goal-body B]
teamx team join <token> --name <name> --session <key> [--loopx-project <dir>]
teamx team approve <member_id> --session <key>          # owner
teamx team deny <member_id> --session <key>             # owner
teamx team list --session <key>
teamx team status [--team <id>] [--session <key>]
teamx team leave --session <key> [--team <id>]     # owner 不可离开（无转移机制）
teamx team archive --session <key> [--team <id>]  # owner；completed → archived
teamx goal set <title> [--body B] --session <key>       # owner
teamx goal share --session <key>                        # owner
teamx goal close --session <key>                        # owner
teamx member set-state <idle|active> --session <key> [--member <id>]  # 自服务；owner 可代设
teamx role list [--team <id>]
teamx role set <role> --session <key> [--member <id>]   # owner 可代指定；仅 approved 角色可用
teamx role propose <key> <label> [desc] --session <key>  # 成员提议自定义角色
teamx role approve <key> --session <key>                 # owner 审批（自动授予提议者）
teamx role deny <key> --session <key>                    # owner 拒绝（移除提议）
teamx role update <key> [--label L] [--description D] --session <key>  # owner 修改角色名/描述
teamx publish <type> [--data <json>] --session <key>
teamx ask <member_id> --question <q> --session <key>
teamx respond <ask_id> --answer <a> --session <key>
teamx events --team <id> [--after <seq>]
teamx sync --session <key> [--no-advance]
teamx loopx report <project> --session <key>
```

全局参数：`--db <path>`、`--json`。默认输出可读文本；`--json` 输出机器可读 JSON（插件统一追加 `--json`）。

publish 类型与状态影响：

| type | 事件 | Goal | Team |
|---|---|---|---|
| start | goal.state_changed | → in_progress | - |
| progress | progress.published | → in_progress（若 shared/refining） | - |
| activity | progress.published | - | - |
| decision | decision.broadcast | - | - |
| update | decision.broadcast | - | - |
| blocked | goal.state_changed | → blocked | → blocked |
| resumed | goal.state_changed | → in_progress | → active |
| achieved | goal.achieved | → achieved | - |
| refine | goal.state_changed | → refining | - |

## 角色目录（默认）

`owner / observer / supervisor / contributor / subtask-implementer / reviewer`，每个 team 创建时 seed。V1 权限仅建议性（`permissions_json` 保留 `{}`），不做强制。

自定义角色：任意成员可用 `role propose` 提议自己的 job role（key 不与内置角色冲突，state=proposed）；owner `role approve` 后角色进入目录并自动授予提议者，`role deny` 则移除提议；owner 可用 `role update` 修改任意角色名/描述。`role set` 仅允许 approved 角色。

## opencode 插件

三件套（由 `install.sh` 安装到 `~/.config/opencode/`）：

- `agent/teamx.md`：`mode: all`，权限 `"teamx_*": allow`，内嵌"先 sync 再行动、有进展就汇报、owner 汇总后广播"协议。
- `command/Team.md`：`agent: teamx`，提供 `/Team` 路由。
- `plugins/teamx.js`：注册 21 个 `teamx_*` 工具 + `event` hook（`session.idle` → 自动发布 activity 事件，成员身份缓存）。

工具集：`teamx_create_team teamx_set_goal teamx_share_goal teamx_close_goal teamx_archive teamx_join teamx_approve teamx_deny teamx_set_role teamx_set_state teamx_list_teams teamx_status teamx_sync teamx_publish teamx_ask teamx_respond teamx_role_propose teamx_role_approve teamx_role_deny teamx_role_update teamx_loopx_report`

客户端层 `opencode-plugin/src/client.ts` 是 V2 换 HTTP 的唯一接缝。

## 安全边界（V1）

- **无鉴权**：`session_key` 由调用方自报（`--session`），CLI 不校验调用方身份；`invite_token` 对全队成员可见（`team list`/`team status` 均返回）。
- **定位**：V1 是"信任本机"的协作约定；"owner 审批/角色"是协作语义，不是安全边界。
- **owner 保护**：owner 不能 `team leave`（无所有权转移机制，防止团队变孤儿）；一个 session 至多作为**一个**非 `archived` 团队的 owner（`team create` 拒绝第二团队，幂等同名复用除外）；`team` 处于 `completed/archived` 后不可再入队。
- **真实鉴权**在 V2（token 签发/校验、成员凭证，见 `docs/02-design-v2-architecture.md`）。

## 并发与一致性

- `db::with_write`：`BEGIN IMMEDIATE` + 最多 20 次 busy 重试（每次 50ms），之后报超时。
- seq 计算与 INSERT 同事务 → 每 team 时间线严格有序。
- 同步游标单调推进：`set_cursor` 用 `MAX(last_seq, excluded.last_seq)`，并发写不会让游标回退（杜绝重复投递）。
- 合法状态转换校验先于任何写入 → 账本与投影永远一致。
- 成员/目标唯一约束（`members(team_id,session_key)`、`goals(team_id)`）由数据库强制，应用层不再依赖猜测。

## 目录

```
crates/teamx/src/{main,cli,db,state,events,commands,loopx}.rs
opencode-plugin/{src/{index,tools,client}.ts, assets/{agent/teamx.md, command/Team.md}}
install.sh
tests/smoke.sh
```

## V2 方向（本计划不含）

`teamx serve`(HTTP+SSE)、跨网络鉴权、TUI toast、SSE→system prompt 注入、角色权限强制、只读 Web 面板。
