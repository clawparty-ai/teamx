# Changelog

## 0.2.0 — 2026-08-30

Teamx 0.2.0 从「共享目标状态机」成长为一个完整的网络协作平台：基于 mTLS 的 Git 代码托管、tun0 透明代理、owner 主导的设计评审工作流、桌面 GUI，以及按用户隔离的隧道访问。

### 亮点

- **基于 mTLS 的 Git 代码托管** — 原生 `git clone/pull/push` 直通团队 server；建队自动建 repo / 批准成员自动授权 / 导入邀请自动 clone。
- **tun0 透明代理**（Linux + macOS）——带本地 DNS 代理、按目标出口路由、watchdog 自愈、事件驱动 I/O（无忙轮询）。
- **用户身份 + 按用户隧道 ACL** — 一个人可拥有多台设备；同一用户的设备零配置互访对方隧道，其他用户拒绝，owner/lead 保留监察。
- **grill-with-docs** — owner 主导的多轮设计讨论工作流（设计树 + 事实请求 + 持久化 ADR），单协议 + 生成式适配器，主机无关交付。
- **桌面 GUI** — 托盘应用 + 原生控制面板、实时日志、root 权限 tun0 管理。
- **`@teamx-ai` 插件** 发布到 npm（opencode + dsh）。

### 新增内容

#### Git 托管（网络模式）

- 团队仓库经 teamx server 管理，标准 **Git Smart HTTP over mTLS** — 原生 `git clone` / `pull` / `push` 开箱即用。
- **团队自动化**：建队自动初始化仓库；批准成员自动授权读；导入邀请自动 clone。
- 仓库级权限（read / write / admin），`teamx git` CLI 支持 clone/pull/push/commit/grant。

#### 网络与隧道

- **隧道自愈**：WS keep-alive 心跳 + 断线自动重连，附生产 runbook。
- **SOCKS5 代理按目标出口路由**：域名/IP 路由到指定代理出口。
- **tun0 透明代理**（虚拟网卡）Linux + macOS：无需逐应用配置。
- **本地 DNS 代理**，保留原 DNS 兜底（含 AAAA 投毒防护）。
- tun0 工程化：watchdog phase-1、异步 bridge 搭建、AsyncFd 事件驱动（去除 2ms 忙轮询）。

#### 团队协作

- **多团队 lead** — owner 可提升 backup lead / co-lead（`is_lead`）。
- **用户身份 + 按用户隧道 ACL**（见亮点）。设备邀请证书 CN 携带 `user_id`；`teamx user list` 审计每个 person 的设备。

#### 设计工作流

- **doc-flow**：TEAM.md `## Documents` 章节、建队时生成文档契约快照、声明式文档生命周期引擎（权限 + 动态状态机校验）、`doc.*` 事件接入 publish 闭环。
- **grill-with-docs**：owner 主导多轮设计会话，依赖感知设计树、稳定 `DQ-`/`FR-` 标识、事实报告、ADR、`CONTEXT.md` 术语表 — 以 `/team-grill`（opencode）与 DSH runtime skill 交付，由单份主机无关协议生成。

#### 桌面 GUI（macOS）

- 托盘应用 → 原生 egui 控制面板（非浏览器）、实时日志面板、tun0 root 授权弹窗、LaunchAgent 安装模式、可双击 `Teamx.app`。

#### 插件与发布

- `@teamx-ai/opencode`、`@teamx-ai/dsh` 及 installable bundle 发布到 npm；dsh 插件注册 teamx runtime skill。

#### 工程与文档

- 全量代码审计（security / correctness / usability）+ 两轮 review 修复（DNS 缓存、waiter leak、pipe 死锁、AAAA 防护、UX）。
- **双语文档**：所有设计/手册/测试/审查文档补齐 cn/en 对照；新增可行性分析、纯 CLI 隧道/代理手册、综合 E2E 套件（43 项隧道/代理检查）。
- 本地流量抓包可行性分析（L1 HTTP / L2 TCP）— 企业版路线图前瞻。

### 说明

- CLI/DB schema 迁移到 **v11**（新增 `users` 表、`members.user_id`、`invitations.user_id`）。现有数据库首次运行自动升级。
- 隧道访问语义对 *用户绑定* 设备有变化：同用户设备可互访对方隧道；其他用户拒绝。旧（未绑定）成员保持原有团队级访问，不影响现有团队。

## 0.1.0 — 2026-08-20

首个版本：Rust CLI（SQLite 事件台账 + 状态机 + mTLS server）+ opencode 插件（30+ 工具 + `/team` 命令 + agent 路由）+ 网络模式 + 多人协作。

### 网络模式（N0—N4）

团队协作不再局限于单机。成员通过 mTLS 身份与实时 WebSocket 推送，在局域网内连接共享 server。

- **mTLS server**（N0）：`teamx serve` 运行 axum + tokio-rustls server，强制双向 TLS。RPC handler 从客户端证书 CN 推导成员身份（取代自报 session key）。`team.import` 通过专用路径把证书身份绑定到预分配席位。支持 `--san <ip>` 把局域网 IP 加入 server 证书 SAN；插件 `serve start` 自动探测并传入本机 IP。
- **WebSocket 推送**（N1）：`GET /ws` 端点按客户端证书 CN 注册订阅者。事件经 `broadcast::Hub`（team→member→sender 注册表）按团队扇出。30s 心跳，断连自动清理。
- **吊销执行**（I2）：`team invite-revoke` 触发被吊销成员的活跃 WebSocket 断连。证书在连接/RPC 时被拒绝。证书 = "can connect"，批准 = "can work"；吊销两者皆断。
- **插件事件驱动**（N3）：WebSocket 连接期间轮询休眠（零轮询）。断连后指数退避重连（1s→60s）+ 轮询兜底。事件帧 200ms 去抖合并突发。
- **跨网络验证**（N4）：`tests/cross-network.sh` 在非 loopback IP 上验证完整 mTLS 链路。`docs/n4-cross-network.md` 提供双机 runbook。

### 邀请信（I0—I1）

成员通过含 mTLS 客户端证书的一次性邀请信加入，取代共享 session key。

- **`team invite "<role>: <desc>"`**（owner）：签发 mTLS 客户端证书（CN=`member:<id>:<role>`）并生成自包含邀请信（`teamx-inv:v1:<base64>`）。角色自动加入团队目录。
- **`team import <letter>`**（member）：解包邀请信、存储证书、认领预分配席位（pending，自动角色）。跨机：本地存储并提示连接以完成注册。
- **`team invite-list` / `team invite-revoke <id>`**（owner）：列出/吊销邀请信；被吊销证书在连接时被拒绝。
- 插件工具：`teamx_team_invite`、`teamx_team_import`、`teamx_team_invite_list`、`teamx_team_invite_revoke`。
- 斜杠命令：`/team invite`、`/team import`、`/team invite-list`、`/team invite-revoke`。

### 自定义角色

- 成员可提议自定义角色（`role propose`）；owner 批准后自动授予提议者。
- owner 可更新任何角色的名称/描述（`role update`）。
- `role set` 只接受已批准角色（内置 + 已批准自定义）；pending 角色报错并提醒等待审批。

### 命令系统

- **`/team <subcommand>`** 路由：`create`、`join`、`status`、`sync`、`goal`、`approve`、`deny`、`role`、`state`、`ask`、`respond`、`publish`、`archive`、`help`。所有子命令都有扁平别名（`/team-create`、`/team-invite`、…）便于 tab 补全。
- `teamx log` 审计回放（解析成员名，支持 `--team`、`--session`、`--limit`、`--after`）。
- owner 唯一性约束：一个 session 最多拥有一个未归档团队；需先 archive 再建新团队。

### 三人协作 Demo

- `docs/demo-3p.md`：owner + contributor + reviewer 工作流。
- `tests/three-member.sh`：自动化端到端测试（多成员审批、并行角色、Q&A、广播、close+archive）。

### 生产加固

- **状态机完备性**：移除不可达 `paused` 状态；新增 `team archive`（completed→archived）与 `member set-state idle|active`；`achieved` 可被 owner 重开（start/resume→in_progress，refine→refining）。
- **数据模型**：移除冗余 `sessions` 表；`members` 加 `UNIQUE(team_id, session_key)`、`goals` 加 `UNIQUE(team_id)`；成员重入复用同一行；sync cursor 单调推进。
- **鉴权/健壮性**：owner 不能 `team leave`；`team approve/deny` 支持 `--team` 消歧；`team create` 幂等（同名复用）；`publish --data` 非 JSON 时回退为 `{"message": s}`。
- **通知风暴修复**：按 session 的已通知 seq 水位线；同一批事件只 toast 一次。
- **M2 轮询 + agent 注入**：无 server 也能工作；轮询刷新 digest + `experimental.chat.system.transform` 把团队状态注入 agent 上下文。

### 代码审查修复

修复全部高/中优先级发现（见 `docs/review/code-review-codex-0817.md`）：

- **跨团队读取绕过**（安全）：非成员不再能读取任意团队的 invite token、成员、角色或事件。
- **pending 成员不能 publish**（鉴权）。
- **非对象 payload 崩溃**（健壮性）：`publish --data '[]'` 不再 panic。
- **邀请信路径穿越**（安全）：`invitation_id` 必须是 UUID。
- **PKI 部分重建正确性**：丢失 `server.key` 不再使已签发成员证书失效。
- **自动执行 seq 水位**（正确性）：`shouldAutoExecute` 用 `e.seq > lastExecutedSeq` 判断重复触发。
- **定向任务类型匹配**（正确性）：`assignedToMe` 匹配任何带 `assignee_member_id` 的事件。
- **非 owner 不能 `role set owner`**（鉴权）。

### 安全模型

V1 无真实认证（`session_key` 自报、`invite_token` 全员可见）。这是"信任本机"的协作约定。见 `docs/goal-v1.md` 与 `v1-spec.md`。

### 测试

`tests/run-all.sh` 运行 9 步自动化套件：CLI 边界用例、mTLS 身份 + 吊销、WebSocket 推送 + 断连 + 重连、跨网络局域网验证、插件单元测试（auto-execute、WebSocket、状态机）。`tests/acceptance.sh` 运行真实模型验收测试（headless `opencode run --agent teamx`）。

### 技术栈

Rust CLI（axum + tokio-rustls + rusqlite + rcgen）· TypeScript 插件（opencode plugin API）· SQLite WAL · mTLS（ring + x509-parser + base64）· WebSocket（axum ws）
