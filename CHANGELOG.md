# Changelog

## Unreleased

### 网络模式 N3：插件事件驱动 + 轮询降级

- **WS 时零轮询**：`TEAMX_SERVER_URL` 设置且 WS 连接在线时，M2 轮询器进入空闲（`wsConnected` 门控）；WS 断开后自动恢复轮询。
- **事件驱动**：WS `event` 帧 → 去抖（200ms 合并突发）后刷新 digest；`onStatus` 在连接/断开切换时立即补一次刷新，跨切换不丢事件。
- **断线回退**：`connectWs` 指数退避重连（1s→60s）+ 抖动；重连成功即切回推送通道，轮询作为兜底（per-session seq 水位防重复 toast）。
- **测试**：新增 `tests/plugin-unit/ws.test.ts`（`wsUrl` 映射、连接、事件回调、断开报告、重启后重连），接入 `run-all.sh`。

### 网络模式 I2：吊销强制 + approve 流程补全

- **吊销检查（连接层）**：RPC 与 WS 连接时，若成员的邀请函已被吊销（`invitations.revoked_at`），证书虽仍能完成 mTLS 握手，但会被立即拒绝（RPC 返回 `member has been revoked`；WS 返回 `error: revoked`）。
- **吊销后主动断连**：`team invite-revoke` 成功后，服务器通过 `broadcast::Hub::disconnect_member` 向该成员的在线 WS 连接推送 `close` 帧并断开；`invite-revoke` 返回 `member_id` 供路由。
- **证书 = "能连"，approve = "能干活"**：证书允许建立连接但 pending 成员受状态机限制（不能 approve 等）；吊销后连连接都进不来，补全 I2 的"证书泄露不能直接工作 / revoke 后立即断连"闭环。
- **测试**：`tests/ws-test.ts` 增加吊销强制三段（revoke 后 live WS 被主动断开、RPC 被拒、WS 重连被拒）。

### 网络模式 N1：WebSocket 推送

- **`GET /ws` 端点**（mTLS）：成员连接后按客户端证书 CN 识别身份，订阅其所属团队的实时事件流；收到 `registered`（含 member_id + teams）后即开始推送。心跳每 30s 发 `ping`（`TEAMX_WS_HEARTBEAT_SECS` 可调），断连自动清理订阅。
- **实时事件广播**：RPC 写账本后，按 `team_id` 把新增事件（`events_after`，全局自增 id 检测）fan-out 到该团队所有在线连接（`broadcast::Hub`：team→member→sender 注册表）；推送尽力而为，账本仍是唯一事实源，离线靠 `sync` 补齐。`/health` 增加 `connections` 在线数。
- **插件 `runWs`**：`connectWs`（mTLS + register + `event` 回调 + 心跳 `pong` + 指数退避重连 1s→60s）；`TEAMX_SERVER_URL` 设置时 index.ts 建立推送连接、实时刷新 digest，M2 轮询作为断线降级（seq 水位防重复 toast）。
- **测试**：新增 `tests/ws-test.ts`（双 mTLS WS 客户端 register、owner 广播实时推给 member、心跳 ping、/health 连接数），接入 `run-all.sh`。

### 网络模式 I1：邀请函（Invitation Letter）+ mTLS 身份

- **`team invite "<角色>: <描述>"`**（owner）：为预分配的 member_id 签发 mTLS 客户端证书（CN=`member:<id>:<role>`，显式随机 serial），生成自包含 invitation letter（CA + 客户端证书/私钥 + server 地址 + 角色），单行 `teamx-inv:v1:<base64>` 输出；角色同时落进团队角色目录（approved）。新增 `invitations` 表（v5 迁移，含 member_id/role/cert_serial/cert_cn/used_by/revoked_at）。
- **`team import <letter>`**（成员）：解包 letter、存 `~/.teamx/letters/<invitation_id>/`（0600）并认领预分配席位（pending，自动带角色）；本地无该邀请（跨机）时仅落盘并提示连服务器完成注册。letter 一次性使用；重导入被拒。
- **`team invite-list` / `team invite-revoke <id>`**（owner）：列出/吊销邀请函；吊销后其证书在 connect 时即失效。
- **mTLS 身份（核心）**：`teamx serve` 强制双向 TLS（CA 签发 server + 客户端证书）；RPC handler 从客户端证书 CN 解析 `member_id` 作为 actor 身份，**替代自报 `session`**（`team.import` 走专用 `import_with_cert` 路径绑定证书身份到预分配席位）。证书 = "能连上"，approve = "能干活"（证书可连接但 owner 审批后才 active）。
- **server 证书 SAN**：`teamx serve --san <ip>` 把局域网 IP 加进 server 证书 SAN（插件 `serve start` 自动探测并传入）。
- **修复 CA 密钥用途**：CA 证书补 `keyCertSign`/`cRLSign`（此前缺导致 openssl 校验失败）。
- **插件**：`runRpc` 增加 mTLS 传输（`fetch` 带 `tls:{cert,key,ca,serverName}`，材料来自 env `TEAMX_MTLS_CERT/KEY/CA` 或 `~/.teamx/letters/*/` 自动发现）；`serve start` 传 `--san` + https URL + mTLS 健康探测；新增 `teamx_team_invite`/`teamx_team_import`/`teamx_team_invite_list`/`teamx_team_invite_revoke` 工具。
- **测试**：`cli-test.sh` 新增邀请函全流程（invite→import→approve→revoke→吊销后拒导入）；新增 `tests/mtls-test.sh`（serve 强制 mTLS、无证书被拒、证书身份解析、`team.import` 绑定证书 member_id、pending 成员不能 approve），并接入 `run-all.sh`。

### 自定义角色（Custom Roles）

- **成员可提议自定义角色**：`role propose <key> <label> [desc]`，写入团队角色目录（state=proposed），key 不得与内置角色（owner/observer/supervisor/contributor/subtask-implementer/reviewer）冲突。
- **owner 审批/拒绝**：`role approve <key>` 将角色置为 approved 并**自动授予提议者**；`role deny <key>` 移除提议。非 owner 调用均被拒绝。
- **owner 修改角色**：`role update <key> [--label] [--description]` 可改任意角色（含内置）的名称/描述，未传字段保持原样。
- **使用约束**：`role set` 只允许 approved 角色（内置 + 已批准自定义）；pending 角色使用时报错并提示等待审批。
- **事件**：新增 `role.proposed` / `role.approved` / `role.denied` / `role.updated`；插件 toast/digest 摘要已覆盖。
- **数据**：`roles` 表新增 `state`（默认 approved）与 `proposed_by`（v4 迁移，幂等补列）。
- **插件**：新增工具 `teamx_role_propose` / `teamx_role_approve` / `teamx_role_deny` / `teamx_role_update`；扁平别名 `/team-role-propose` `/team-role-approve` `/team-role-deny` `/team-role-update`。
- **测试**：cli-test 新增自定义角色全流程（propose→不可用→审批→自动授予→可用→update→deny）。

### 命令简化 + owner 唯一约束

- **通知风暴修复**：M2 轮询器（`sync --no-advance`）新增 per-session 已通知 seq 水位，同一批事件只 toast 一次，不再每 15s 重复轰炸；首次接入记录水位不提示存量。
- **owner 唯一约束**：一个 session 至多作为一个非 `archived` 团队的 owner；`team create` 创建第二团队（不同名）时报错，归档后才可再创建（幂等同名复用不受影响）。
- **命令简化**：
  - `/Team` 升级为 `/team <子命令>` 子命令路由（`create/join/status/sync/goal/approve/deny/role/state/ask/respond/publish/archive/help`），`agent/teamx.md` 增加路由表。
  - 全部子命令均有**扁平别名**命令（`/team-create`、`/team-join`、`/team-status`、`/team-sync`、`/team-goal-set`、`/team-goal-share`、`/team-goal-close`、`/team-approve`、`/team-deny`、`/team-role`、`/team-state`、`/team-ask`、`/team-respond`、`/team-publish`、`/team-archive`、`/team-help`），利用 opencode 命令列表前缀过滤实现 tab 补齐。
  - 命令文件安装位置统一到标准 `commands/`（复数）目录，保留 `command/` 单数向后兼容。

### M2（无 server 轮询版）+ 模型级验收

- 新增 `teamx log` 命令（审计回放，解析成员名，`--team`/`--session`/`--limit`/`--after`）。
- 插件 M2（不依赖 server，轮询实现）：
  - 每会话团队 digest 缓存 + `experimental.chat.system.transform` 注入（成员 agent 每轮请求可见最新团队状态）。
  - 轮询器（`TEAMX_POLL_INTERVAL` 毫秒，默认 15000，0 关闭）刷新 digest；有新事件时 `client.tui.showToast` 通知，有 `clarification.asked` 时 `appendPrompt` 唤醒提示。
  - `dispose` hook 清理定时器。
- 新增 `tests/acceptance.sh`（真实模型级验收，headless `opencode run --agent teamx`，消耗 token，不并入默认套件）；已实测通过（模型经插件调用 `teamx_create_team`/`teamx_set_goal`，账本落 team.created/goal.set，event hook 自动发布 activity）。

### 三人协作 Demo

- 新增 `docs/demo-3p.md`（owner + contributor + reviewer 三人协作方案文档）。
- `demo/start.sh` 支持多窗口（`./demo/start.sh 3`）。
- 新增 `tests/three-member.sh`（三人闭环自动化测试：多成员审批、并行角色、澄清问答、广播、关闭+归档），并接入 `tests/run-all.sh`。
- 更新 `test-plan.md` / `test-cases.md`（TC-401、TM-04）。

### 生产化（production hardening）

- **状态机完整性**：移除不可达的 `paused` 态；新增 `teamx team archive`（owner，completed→archived）与 `teamx member set-state idle|active`（自服务/owner 代设），补齐 `MemberIdle`/`MemberActive`/`ArchiveTeam` 动作的可达命令。
- **数据模型完整性**：
  - 移除冗余 `sessions` 表（`members(session_key, team_id)` 已覆盖其全部信息，此前只写不读）。
  - `members` 加 `UNIQUE(team_id, session_key)`、`goals` 加 `UNIQUE(team_id)`（v3 迁移，含旧数据去重）。
  - 成员 leave/deny 后重入复用同一成员行（不再产生同名 left 残留行）。
  - 同步游标单调推进（`MAX`），并发写不回退。
- **授权/健壮性**（承上轮 review）：
  - 禁止 owner `team leave`（无所有权转移前防团队孤儿）。
  - `team approve/deny` 增加 `--team` 消歧（多团队 owner）。
  - `team create` 同名复用（模型重试幂等）。
  - `publish --data` 非 JSON 字符串 fallback 为 `{"message": s}`。
  - 插件 `event` hook 缓存成员身份，非成员会话不再每次 idle 都 spawn 子进程。
  - 插件 `runCli` 加 30s 超时。
- **安全定位**：明确 V1 无真实鉴权（`session_key` 自报、`invite_token` 全员可见，仅信任本机），写入 `goal-v1.md` 与 `v1-spec.md`。
- **打包/安装**：插件 `package.json` 可发布（非 private、`exports`/`files`/`license`/`repository`）；`install.sh` 幂等、设置 `~/.teamx` 0700 / db 0600 权限、支持 `--uninstall`。
- **CI**：新增 `.github/workflows/ci.yml`（cargo build+clippy+test、CLI 集成测试、插件 typecheck+build）。
- **测试**：新增回归（archive、member set-state、重入复用、owner 离开拒绝、多团队 owner 审批、bare data fallback、游标单调）。

## 0.1.0

初始版本：Rust CLI（SQLite 事件账本 + 状态机）+ opencode 插件（15 工具 + `/Team` agent）+ loopx 桥接 + 双会话 demo 与测试。
