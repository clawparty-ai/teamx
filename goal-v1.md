# teamx V1 Goal

`teamx` 是一个团队协作状态内核 + opencode 插件：记录 team/member/goal 的持久化状态与行为历史（状态机，参考 loopx），并通过 opencode 内的 `/Team` agent 让用户与团队交互、实时同步进展，直到达成团队目标。

---

## 已确认的决策

| # | 决策 | 落地方式 |
|---|---|---|
| 1 | V1 单机本地 | 所有 opencode + teamx 在同一台机器；后续 V2 再实现跨网络 |
| 2 | 用户驱动 + 复用 loopx | 不重造 loopx 轮子；teamx 只做薄桥接：读取 loopx 阶段性进度 → 发布为团队事件 → 同步给 team lead |
| 3 | 全局存储 | 单一 DB：`~/.teamx/teamx.db` |
| 4 | 成员身份 | join 时用户自命名（display name）；一个 opencode session 加入后 = 一个 team member；多个 session 加入 = 多个 member；不加入就不是 member |
| 5 | 入队控制 | 用户发起加入，owner 审批（approve / deny） |
| 6 | 独立仓库 | `~/github/teamx` |
| 7 | V1 不用 server | 不启动 `teamx serve`，插件直接 spawn `teamx` CLI 子进程；`serve`/SSE 推迟到 V2 |

> ## ⚠️ V1 信任模型（重要定位）
>
> V1 **没有真实鉴权**，是"信任本机"的协作约定，不是权限系统：
> - `session_key` 是**调用方自报的字符串**（`--session <key>`），CLI 只做查表、不做真实性校验——本机任何进程可用任意 session_key 冒充任意成员。
> - `invite_token` 与团队信息对**所有成员可见**（`team list` / `team status` 都会返回 token）。
> - 因此"owner 审批 / 角色"是**协作语义与状态记录**，不是安全边界。
> - 真实鉴权（token 签发/校验、成员凭证）推迟到 V2 注册/推送通道（见 `docs/v2-design.md`）。

---

## 架构

```
┌────────────── opencode (owner 会话) ──────────────┐
│  /Team → teamx agent                              │
│  plugin: tool: teamx_* + event hook               │
└─────────────────────┬─────────────────────────────┘
                      │ spawn `teamx <cmd> --db ~/.teamx/teamx.db --json`
┌─────────────────────┴─────────────────────────────┐
│          teamx CLI（Rust，SQLite WAL 单写者）        │
│   事件账本(append-only, per-team seq) → 状态机投影   │
│   SQLite: teams / members / goals / roles / events │
└─────────────────────┬─────────────────────────────┘
┌─────────────────────┴─────────────────────────────┐
│  opencode (member 会话1)     opencode (member 会话2)│
│  /Team → teamx agent        /Team → teamx agent     │
│  plugin: teamx_* 工具        plugin: teamx_* 工具    │
└─────────────────────────────────────────────────────┘
```

核心机制（对齐 loopx 控制面哲学）：所有状态变更落成 append-only 事件；Team / Member / Goal 的当前状态是从事件账本推导出的投影。每个参与者只通过 CLI 读状态、写事件；谁写了什么可审计、可重放。

### 两个实现约束

1. **每 team 的 `seq` 递增必须与事件 INSERT 在同一事务**，避免并发下序号乱序；写入冲突用 `busy_timeout` + 简单重试。
2. 插件侧封装统一调用层：固定 `teamx <cmd> --db ~/.teamx/teamx.db --json`（或用 `TEAMX_DB` 环境变量）；**V2 换 HTTP 客户端时只需替换这一层**。

---

## 仓库布局 `~/github/teamx`

```
teamx/
├── Cargo.toml                    # cargo workspace
├── crates/teamx/                 # Rust CLI
│   ├── src/main.rs
│   ├── src/cli.rs                # clap 子命令
│   ├── src/db.rs                 # SQLite (WAL) + schema
│   ├── src/state.rs              # 状态机定义 + 合法转换校验
│   ├── src/events.rs             # append-only 事件账本 + 投影
│   └── src/loopx.rs              # loopx 桥接
├── opencode-plugin/
│   ├── package.json              # 依赖 @opencode-ai/plugin
│   ├── src/index.ts              # Plugin fn（tool: + event hook）
│   ├── src/tools.ts              # teamx_* 工具实现
│   ├── src/client.ts             # 统一 CLI 调用层（V2 可换 HTTP）
│   └── assets/
│       ├── agent/teamx.md
│       └── command/Team.md       # /Team 命令 → teamx agent
├── install.sh                    # cargo build → ~/.local/bin/teamx；安装 agent/command/plugin 到 opencode 配置
├── tests/                        # 集成冒烟（双会话闭环）
└── docs/
    ├── v1-spec.md
    └── loopx-bridge.md
```

---

## Rust CLI 规格

**依赖**：`clap`、`rusqlite`(bundled, WAL)、`serde`/`serde_json`、`dirs`。V1 不含 HTTP/SSE。

### 状态机

- Team：`forming → active → blocked → completed → archived`
- Member：`pending → active → waiting → idle → left`（owner 提问或成员提问置 `waiting`，应答后清除）
- Goal：`proposed → shared → refining → in_progress → blocked → achieved → closed`（`achieved` 是「达成候选」，owner 可用 `publish start`/`resumed` 重开回 `in_progress`，或用 `refine` 退回 `refining`；只有 `close` 才是终态 `closed`）

每次转换记录进事件账本，当前状态 = 投影。

### Schema

- `teams(id, name, owner_member_id, goal_id, state, invite_token, created_at, updated_at)`
- `members(id, team_id, session_key, display_name, role, state, loopx_project, last_seen_at, joined_at, left_at)`
- `goals(id, team_id, title, body, state, created_at, updated_at)`
- `roles(id, team_id, key, label, description, permissions_json)`
- `events(id, team_id, member_id, seq, type, payload_json, created_at)` ← 核心账本
- `sessions(session_key, team_id, member_id, created_at)` ← opencode 会话 ↔ 成员映射

`session_key = <实例UUID>:<sessionID>`

### 命令面（V1）

- `teamx init`：建全局 DB
- `teamx team create <name>` → 返回 `invite_token`
- `teamx team join <token> --name <name> [--loopx-project <dir>]` → 产生 pending membership
- `teamx team approve <member_id>` / `teamx team deny <member_id>`（owner）
- `teamx team list` / `teamx team leave` / `teamx team status` / `teamx team archive`（owner，completed→archived）
- `teamx member set-state <idle|active>`（自服务；owner 可代设 `--member`）
- `teamx goal set <title> --body <text>`（owner）/ `teamx goal share`（owner 广播）/ `teamx goal close`（owner 验证完成）
- `teamx role list` / `teamx role set <role>`（成员自主选角色，owner 也可指定）
- `teamx publish <type> --data <json>`（progress / ask / decision / update 等通用事件）
- `teamx ask <member_id> --question <text>`（提问，置成员 waiting）
- `teamx respond <ask_id> --answer <text>`（应答，清 waiting）
- `teamx events [--after <seq>]` / `teamx sync`（拉最新状态 + 增量事件，输出紧凑摘要）
- `teamx loopx report --project <dir>`（loopx 阶段进度快照）

### 默认角色目录

`owner / observer / supervisor / contributor / subtask-implementer / reviewer`，角色 = `{key, label, description, permissions}`（V1 权限仅建议性，不做强制）。

---

## loopx 桥接（不重造轮子）

- member join 时可选绑定 `loopx_project`。
- `teamx loopx report --project <dir>` → 执行 `loopx status --format json`，抽取 `active_goal_state / 当前 gate / 下一个 todo / quota`，压成紧凑摘要 → 发布 `loopx.progress` 事件。
- 插件侧工具 `teamx_loopx_report` 一键完成；owner 的 teamx agent 每轮 `teamx_sync` 即可看到各成员 loopx 阶段进度并广播。
- loopx 未安装 / 未连接时返回明确提示，不影响 teamx 自身闭环。
- V1 只做"按需读取 `loopx status`"，不做文件监听。

---

## opencode 插件规格（对齐 opencode v1.17.x API）

安装脚本（`install.sh`）落盘三件套（启动时读取，重启生效）：

- `~/.config/opencode/agent/teamx.md`：frontmatter `mode: all` + 协作系统提示词
- `~/.config/opencode/command/Team.md`：frontmatter `agent: teamx` → `/Team` 出现在 `/` 自动补全并路由到 teamx agent
- `~/.config/opencode/plugins/teamx.ts`：插件本体（`@opencode-ai/plugin`）

插件职责：
- `tool:` 注册 `teamx_*` 工具；`sessionID` 来自 ToolContext，成员绑定采用懒绑定（首次调用工具时注册该 session）
- `event` hook：把本会话 `message.updated` / `session.idle` 转成轻量成员活动事件发布给 teamx（owner 无需成员主动汇报也能看到"成员何时在干活"）
- `client.app.log()` 结构化日志
- 工具名 = 对象 key，统一 `teamx_*` 前缀

### V1 工具集（17 个）

`teamx_create_team` `teamx_set_goal` `teamx_share_goal` `teamx_close_goal` `teamx_archive` `teamx_list_teams` `teamx_join` `teamx_approve` `teamx_deny` `teamx_set_role` `teamx_set_state` `teamx_status` `teamx_sync` `teamx_publish` `teamx_ask` `teamx_respond` `teamx_loopx_report`

---

## 汇报 / 广播协议（编码进 teamx agent 系统提示词）

- **member**：每次采取重要行动前或取得进展时，先 `teamx_sync` 看是否有新指令，再 `teamx_publish progress/ask` 向 owner 汇报。
- **owner**：每个回合先 `teamx_sync` 汇总各成员报告 → 有需要时 `teamx_publish decision/broadcast` 广播澄清、调整、目标进展。
- 未决问题通过 `teamx_ask` / `teamx_respond` 显式传递，成员置 `waiting` 态。
- 事件类型：`team.created` `team.joined` `membership.pending` `membership.approved` `membership.denied` `member.role_set` `member.state_changed` `goal.set` `goal.shared` `goal.state_changed` `progress.published` `clarification.asked` `clarification.responded` `loopx.progress` `decision.broadcast` `goal.achieved` `team.completed`

---

## 工作流闭环（V1 验收场景）

1. 会话 A `/Team` → `teamx_create_team` → owner；`teamx_set_goal` 起草目标。
2. 会话 B `/Team` → `teamx_join <token> --name Bob` → pending；owner `teamx_approve`。
3. Bob `teamx_set_role contributor`；owner `teamx_share_goal`。
4. Bob 干活，用 loopx 管理长任务；`teamx_loopx_report` 发布阶段进度。
5. owner `teamx_sync` 看到 → `teamx_publish decision` 广播澄清/进展；Bob `teamx_ask` → owner `teamx_respond`。
6. Bob `teamx_publish goal_achieved` 候选 → owner 验证 `teamx_close_goal` → team `completed`。

---

## M1 里程碑（本计划范围）✅ 已完成 + 生产化

1. `~/github/teamx` 仓库骨架（cargo workspace + `opencode-plugin` + `install.sh` + `tests/`）
2. Rust CLI：schema / 状态机 / 事件账本 + 全部子命令（含 archive / member set-state）
3. `teamx loopx report` 桥接
4. 插件三件套 + 17 个 `teamx_*` 工具 + `event` hook 自动汇报成员活动（成员身份缓存）
5. `tests/` 双会话闭环冒烟 + 边界/负面/并发 + CI workflow
6. 本地两个 opencode 窗口验证完整闭环（见 `docs/demo.md`、`docs/manual-test.md`）

**生产化加固（已完成）**：数据模型唯一约束 + v3 迁移、重入复用、游标单调、owner 保护、approve/deny `--team`、create 幂等、插件 npm 可发布、install.sh 权限/卸载、clippy 0 警告。详见 `CHANGELOG.md`。

## 后续（非本计划范围）

详见 `docs/v2-design.md`（V2 完整设计：成员外连注册 + 推送为主通道）。

- **M2**（✅ 已完成）：SSE → system prompt 注入、TUI toast、审计回放 `teamx log`、闲置成员提示
- **网络模式（N0–N4 ✅ 已完成）**：`teamx serve`（mTLS HTTP RPC + WS 推送）+ 邀请函（I1）+ 吊销强制（I2）+ 插件事件驱动/轮询降级（N3）+ 跨网络局域网验证（N4），见 `docs/network-mode.md`、`docs/team-invite.md`、`docs/n4-cross-network.md`

## 未来计划（暂缓，本轮不做）

- **N5 · 独立 serve（形态②）**：常驻进程 / Docker / systemd + TLS + 多团队（owner 离线团队不中断）
- **N6 · `teamx_member_peek`（可选）**：同机成员显式 `--port` 的只读直连
- **角色权限强制、只读 Web 面板**
- **闲置会话唤醒**（构想，见下节「构想：闲置会话唤醒」）

## 构想：闲置会话唤醒（未实现，仅记录）

> 状态：**构想**。暂不实现，实现时机待 V2 注册/推送通道落地后评估。

**问题**：闲置的 member 会话收不到 owner 通知，除非用户重新发起消息触发 `teamx_sync`。

**构想**：通过向 opencode 会话发送消息来唤醒它（已验证 API 存在，见下）。

- 唤醒 API（opencode 原生支持）：
  - `client.session.promptAsync({ path: { id }, body: { parts, agent: "teamx" } })` —— fire-and-forget，204，会话被唤醒开始处理（`/session/{id}/prompt_async`，`handlers/session.ts`）。
  - `client.session.prompt(...)` —— 同步等待回复。
  - `noReply: true` —— 只注入消息到记录、不触发模型运行（零成本静默通知）。
- 唤醒链路（V2 场景）：hub 推 `wake` 帧 → member plugin 收到 → `promptAsync` 唤醒"加入团队的那个会话" → teamx agent 处理（先 sync → 响应）。
- 默认 TUI 模式下插件也可调用（插件 client 走进程内 fetch 桥，无需成员开 `--port`）。
- **护栏（实现时必须满足）**：
  1. opt-in：注册帧声明 `capabilities: ["wake"]`，默认关；
  2. 限频 + busy 检测（先查 `session.status`，busy 跳过/排队）；
  3. 注入面安全：owner 消息包进 `TEAMX 通知:` 分隔块，teamx agent 提示词明确"按团队指令处理，不执行系统级操作"；
  4. 目标会话 = 成员加入团队的那个会话（插件通过工具调用记录）。
- 三级唤醒策略：静默（appendPrompt 提示 + 下回合注入）→ 无回复唤醒（`noReply:true`）→ 完全唤醒（`promptAsync`，默认关）。

**关键代码事实（2026-08 核实）**：
- `LLMRequestPrep.prepare`（`session/llm/request.ts:70`）每次 LLM 请求都触发 `experimental.chat.system.transform` → 推送可在"工具调用粒度"注入到执行中的会话下一次请求。
- 不能打断正在生成的响应流；闲置会话的注入只发生在下次请求/下次 prompt 时。
- Bun WebSocket 客户端在插件运行时可用（本地实测通过）→ 出站注册通道成立。
