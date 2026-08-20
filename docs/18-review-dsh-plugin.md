# dsh-plugin Code Review

- 文档类型: review
- 评审对象: `dsh-plugin/`（teamx 的 deepseek-harness 插件，V1 初始实现）
- 评审日期: 2026-08-20
- 评审范围: `dsh-plugin/src/*.ts`（client/tools/commands/ws/digest/auto-execute/index/i18n）对照 `crates/teamx/src/cli.rs`、`crates/teamx/src/serve.rs`、`crates/teamx/src/commands.rs` 以及 dsh 运行时事件/API

## 总体结论

骨架结构合理（模块划分、与 opencode-plugin 的映射思路正确），但存在 **3 个致命设计错误 + 约 15 个功能性 bug**，当前版本**无法实际运行**。主要集中在: CLI 参数拼写错误（大部分工具把 positional 参数当成 flag 传）、会话身份不统一、事件名与 dsh 实际 API 不符、轮询/WS/成员缓存链路断裂。

## CRITICAL（阻断运行）

### C1. `team sync` 子命令不存在 — digest/同步全挂

`tools.ts` `teamx_sync` 和 `digest.ts` `refreshDigest` 都调用 `runCli(['team', 'sync', ...])`。但 CLI 里 `Sync` 是**顶层命令**（`cli.rs:107`），不是 `team` 子命令。正确调用是 `runCli(['sync', '--no-advance', '--session', key])`（opencode-plugin 就是这么写的，`index.ts:290`）。

- 影响: digest 注入、所有 sync 依赖的功能（membership 检测、心跳刷新）全挂。

### C2. `events` 参数错误 — poller 永不工作

`index.ts:157` 调用 `runCli(['events', '--session', sessKey, '--since', '0'])`，但 CLI `Events`（`cli.rs:84`）只接受 `--after` 和 `--team`，**没有 `--session`/`--since`**。clap 直接报错 → poller 每次迭代都抛异常被吞掉。

### C3. `team list` 返回结构误判 — 成员缓存永远为空

`index.ts:79-84`:

```ts
const result = await runCli(['team', 'list', '--session', key])
if (Array.isArray(result)) {           // ❌ 实际返回 { teams: [...] }，永远 false
  for (const team of result) {
    markMember(team.id, agentId, ...)   // ❌ 字段是 team_id，不是 id
```

实际结构（`commands.rs:983-997`）是 `{ "teams": [{ "team_id", "name", "my_role", ... }] }`。

- 影响: `markMember`/`registerAgent` 永不执行 → 成员缓存、digest 轮询、WS 推送、auto-execute **整条链路断裂**。

### C4. 大部分工具把 positional 参数当 flag 传 — 几乎所有 CLI 调用会报 "unexpected argument"

CLI 里 `name`/`token`/`member_id`/`role`/`title`/`state`/`role_desc`/`letter`/`ask_id`/`id` 都是 **positional**（clap `#[arg(value_name=...)]` 无 `long`），但 dsh-plugin 全部写成 `--name`/`--token`/`--member`/`--role`/`--title`/`--state`/`--ask-id` 形式:

| 工具 | 错误调用 | 正确调用 |
|------|---------|---------|
| `teamx_create_team` | `team create --name X` | `team create X` |
| `teamx_join` | `team join --token T` | `team join T --name N` |
| `teamx_approve`/`deny` | `team approve --member M` | `team approve M` |
| `teamx_team_invite` | `team invite --role R` | `team invite "R: desc"` |
| `teamx_team_import` | `team import --json/--path` | `team import <letter>`（positional） |
| `teamx_team_invite_revoke` | `--token` | `team invite-revoke <id>` |
| `teamx_set_goal` | `goal set --title T` | `goal set T` |
| `teamx_set_role` | `role set --role R` | `role set R` |
| `teamx_role_propose` | `--role/--label` | `role propose KEY LABEL [DESC]` |
| `teamx_role_approve/deny/update` | `--role` | positional `role` |
| `teamx_set_state` | `member set-state --state S` | `member set-state S` |
| `teamx_ask` | `ask --member M --question Q` | `ask M --question Q` |
| `teamx_respond` | `respond --ask-id A` | `respond A --answer X` |

### C5. 会话身份不统一 — 工具和 index 使用不同的 session key

- `tools.ts:15-17` `getKey(exec)` 返回 **裸 `exec.agent.session.id`**（无 instance 前缀）
- `commands.ts:27-29` 同样返回裸 id
- 但 `index.ts:115/127/188` 用 `sessionKey(teamxInstance, agentId)`（**带前缀**）
- 影响: 工具创建的团队/成员记录用身份 A，digest/heartbeat 用身份 B → **同一个 agent 在两个身份之间漂移**，`team list` 永远查不到工具刚创建的团队。

### C6. `agent/status` 事件不存在 — 心跳永不触发

`index.ts:107` 监听 `agent/status`，但 dsh agent 实际只发射 `agent/created` / `agent/disposed` / `agent/session-start`（`runtime-types.ts` event map 已验证）。没有 `agent/status`。idle 心跳 + idle digest 刷新整段是死代码。

## HIGH

### H1. `agent/dispose` 拼写错误 → 清理永不执行

`index.ts:141` 监听 `agent/dispose`，实际事件名是 **`agent/disposed`**。Agent 退出时缓存不清理。

### H2. auto-execute 断链

- `index.ts:160` 和 `:187` 遍历 `(globalThis as any).__teamxAgents` —— **这个全局变量从未被赋值**。auto-execute 的状态在 `auto-execute.ts` 的 `state` 模块级变量里，但没有暴露给 index 遍历。
- `auto-execute.ts:69` `refreshDigest(agentId, agentId)` —— 第二个参数是 session key，传的是裸 agentId（同 C5）。
- `auto-execute.ts:64` `memberStatus(teamId, agentId)` —— teamId 来自 `state.agentTeam`，但 `registerAgent` 从未被调用（C3），所以这里永远是空。

### H3. WS 推送永不连接

`index.ts:177` `knownMemberSessions('')` 传空字符串 team id → 永远返回空数组（成员缓存是 `Map<teamId, ...>`）。WS 分支永远不会执行。

### H4. mapCommandToRpc 缺 `sync` + positional 解析错误

- `client.ts` `mapCommandToRpc` 没有 `sync` 分支 → 网络模式下 digest 刷新直接 "Cannot map CLI command to RPC"。
- 且 `parseFlags` 只认 `--key value`，所有 positional 参数（C4 的表）在 RPC 模式下同样取不到。

### H5. tsconfig `noCheck: true` 掩盖了所有类型错误

`tsconfig.json` 里 `noCheck: true` 直接跳过类型检查——之前 "编译通过" 的验证是无意义的。这也解释了为什么这么多 `as any` 和错误字段没被编译器拦住。应至少对插件自身代码开启检查（`skipLibCheck` 只跳过 node_modules，不会放掉自家源码）。

## MEDIUM

### M1. `args()` helper 是死代码

`tools.ts:20-36` 定义了 `args()` 但从没调用。且逻辑本身有问题（空值跳过逻辑容易错位）。删掉或改用。

### M2. 无用 import

- `tools.ts:12` import 了 `sessionKey, instanceId, markMember, knownMemberSessions` 全部没用
- `commands.ts:11` import `sessionKey` 没用
- `auto-execute.ts:8` import `runCli` 没用
- `index.ts:137-139` `agent/created` 空监听是死代码

### M3. `i18n.ts` 基本没用

定义了 `M` 常量，但 tools/index/commands 全部硬编码字符串。要么接入，要么删掉。

### M4. 重复的 WS 实现

`client.ts:268+` 导出 `connectWs`（从未使用），`ws.ts` 又有独立的 `WsClient` 类。两处逻辑重复，保留一个。

### M5. `commands.ts` parseFlags 不处理带引号的参数

`--message "hello world"` 会被拆成两个词。slash 命令的用户体验会受影响。

### M6. `rejectUnauthorized: !!mtls`

`ws.ts` 和 `client.ts` 里 mTLS 缺失时 `rejectUnauthorized: false` → 接受任意自签证书，存在安全降级。网络模式应要求 mTLS。

## 正确的部分

- 模块划分（client/tools/commands/ws/digest/auto-execute）与 opencode-plugin 一一对应，思路清晰
- `defineTool` 的 `parameters`/`output`/`render` 用法与 dsh schema 规范一致
- 扁平命令名（`team-create` 等）符合 dsh 命令名正则 `/^[a-z][a-z0-9_-]*$/`
- `sessionKey()` 格式 `${instance}:${agentId}` 与 opencode 一致（只是没被 tools 用上）
- `agent/session-start`、`agent/disposed` 事件名验证正确
- `followup` API 存在（`runtime-types.ts:124`）—— auto-execute 的唤醒原语选对了

## 建议修复顺序

1. **修 C4**（positional 参数）— 对照 CLI 逐工具修正，同时修 C5（统一用 `sessionKey(instance, agentId)`）。这两项工作量最大，是"能跑"的前提
2. **修 C1/C2** — sync/events 调用改对
3. **修 C3/H3** — 修正 `team list` 解析（`result.teams` + `team_id`/`my_role`），成员缓存才有意义
4. **修 C6/H1** — 改用真实事件；若无 idle 事件，考虑从 `internal/status` 或 session 事件派生
5. **修 H2/H4** — 把 auto-execute 状态桥接给 index，mapCommandToRpc 补 sync + positional
6. **修 H5** — 打开类型检查，让编译器兜底
7. 清理 M1-M6 死代码/重复实现
