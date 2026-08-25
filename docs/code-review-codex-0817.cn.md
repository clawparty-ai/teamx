# teamx 代码 Review — 2026-08-17

> **状态：已全部修复**（见 CHANGELOG「代码 Review 修复」）。高/中优先级 8 项 + 次要建议均已落地并补回归测试（`tests/cli-test.sh`、`tests/mtls-test.sh`、`tests/plugin-unit/auto-execute.test.ts`）。

## 范围

- 仓库：`/Users/caishu/github/teamx`
- 对象：Rust CLI（`crates/teamx`）、opencode 插件（`opencode-plugin`）、安装脚本
- 结果：本次 review 未修改源码

## 验证结果

- `cargo test --workspace`：14 个单测全部通过
- `cargo clippy --all-targets -- -D warnings`：通过
- `bunx tsc --noEmit`（`opencode-plugin`）：通过
- 额外手动复现出若干未覆盖问题，详见下文

## 高优先级问题

- 网络模式存在跨团队读取绕过。
  - `cmd_team_status` 在传入 `--team` 时完全跳过 session/membership 校验。
  - `cmd_role_list`、`cmd_events`、`cmd_log --team` 也不做归属校验。
  - RPC 层 `dispatch` 虽从 mTLS 证书解析身份，但这些命令仍可按任意 team id 读取状态、事件、角色和 `invite_token`。
  - 位置：`crates/teamx/src/commands.rs:735`、`crates/teamx/src/commands.rs:1403`、`crates/teamx/src/commands.rs:1907`、`crates/teamx/src/serve.rs:449`

- `pending` 成员可以在未审批前发布/改变团队状态。
  - `memberships_for_session` 只排除 `left/denied`。
  - `cmd_publish` 不校验 actor 的 `pending/waiting` 状态。
  - 已复现：未审批的 `inst:b` 可成功执行 `publish decision`。
  - 位置：`crates/teamx/src/commands.rs:229`、`crates/teamx/src/commands.rs:1722`

- `publish --data '[]' --assignee <id>` 会 panic。
  - 当 `data` 是合法但非对象的 JSON（数组/字符串/数字）时，`payload["assignee_member_id"] = ...` 会触发 `serde_json` panic。
  - CLI 退出码 101。
  - 位置：`crates/teamx/src/commands.rs:1758`

- 邀请函导入存在路径穿越。
  - `store_letter` 直接用未校验的 `invitation_id` 拼目录。
  - 已复现：`invitation_id: "../../teamx-escaped"` 会在 `/tmp/teamx-escaped/` 写入 `letter.json`、`client.crt`、`client.key`、`ca.crt`。
  - 位置：`crates/teamx/src/commands.rs:1218`

## 中优先级问题

- PKI 文件缺失时会误重建 CA。
  - `ensure_pki` 只要四个文件任一缺失就重新生成整套 CA + server cert。
  - 如果仅 `server.key` 丢失，会覆盖已有 CA，导致已签发的所有 member cert 全部失效。
  - 位置：`crates/teamx/src/pki.rs:77`

- 插件 auto-execute 只会触发一次。
  - `alreadyExecuted: autoExecutedSeq.has(sessionID)` 传的是“是否曾执行过”的布尔值，而不是当前 seq 水位。
  - 同一 session 之后的新定向任务不会再自动唤醒。
  - 位置：`opencode-plugin/src/index.ts:271`

- 定向任务类型匹配过窄。
  - `assignedToMe` 只认 `decision.broadcast` / `goal.shared`。
  - 但 Rust 侧任何 `publish` 类型带 `--assignee` 都会写入 `assignee_member_id`。
  - 因此 `start` / `progress` / `achieved` 等定向任务不会触发自动执行。
  - 位置：`opencode-plugin/src/index.ts:153`、`crates/teamx/src/commands.rs:1756`

- 非 owner 可把自己 role 设为 `owner`。
  - `ensure_owner` 仍使用 `owner_member_id`，因此没有直接越权。
  - 但插件 `isOwnerSession` 依赖 `my_role === "owner"`，成员可借此绕过自动执行或显示为 owner。
  - 位置：`crates/teamx/src/commands.rs:1430`、`opencode-plugin/src/index.ts:138`

## 次要建议

- `teamx serve` 绑定地址用 `format!("{}:{}")` 解析，不支持裸 IPv6 地址。
- `loopx::loopx_status` 调用 `loopx` 子进程没有超时，可能长时间挂起。
- `team_status_json` 和首次 `sync` 会全量读团队事件到内存，事件量大时偏重；建议在 SQL 层分页/限制。
- `serve` 和插件 `serveStart` 启动后没有把 `TEAMX_SERVER_URL` 同步给当前插件会话，后续工具仍可能走本地 CLI 路径。
- 插件 membership 缓存没有在 `leave` / `deny` 后失效，可能继续为已退出成员发布 activity。

## 结论

整体结构清晰，状态机与事件账本的测试基础较好。优先处理顺序建议：

1. 网络模式授权与团队归属校验
2. 邀请函路径校验
3. `publish` 非对象 payload panic
4. 插件 auto-execute 的水位逻辑与任务类型匹配
