# Changelog

## 0.2.3 — 2026-09-15

### IM 双向对话 + Member Console「File」文件浏览器

- **IM 持久连接网关（方案 1 sidecar）**：新增 `teamx im-gateway run`，用
  `lark-channel` 建立飞书长连接（出站 WebSocket，无需公网域名/证书/回调地址），
  归一化入站事件并经 mTLS 转发给 `serve`。固定命令走快路径，其余任意文本转发给
  同机 `opencode serve`（`--agent <url>`），AI 回复推回飞书；双向落台账审计
  （`im.chat.in` / `im.chat.out`）。`im.bind` 加入服务白名单（唯一的「绑定前」
  动作，一次性 code 即授权）。设计见 `docs/TODO-im-agent.cn.md`。
- **Member Console「File」一级菜单**：工作区文件浏览 + 预览。浏览根锚定会话 cwd；
  复用 `fs.rs` 的同一套路径校验（`PathPolicy{Service,Console}`）；`.teamx` 必拒、
  `.git` 可浏览但折叠 `objects/hooks/logs/lfs`、密钥文件仅显示元数据；预览支持
  文本（行号）/markdown（`mdRender` 白名单）/图片内联，SVG 与二进制仅下载；
  repo 快捷过滤（cwd 深度 3、显示当前分支）。设计见
  `docs/design/console-file-browser.md`。
- **修复**：目标关闭后 `teams.goal_id` 被置 NULL，导致 console / `team status`
  显示「该团队尚未设置目标」——展示路径回退到该团队最近的 goal 行。
- **修复**：`teamx worker --opencode-url` 在多工作区节点上极易 attach 到错误的
  opencode 实例（默认 4199）；部署时务必显式指定。

## 0.2.2 — 2026-09-13

### teamx 空间改为项目级（`<cwd>/.teamx`），修复多会话证书混乱

- **根因**：旧设计把邀请函/mTLS 证书放在全局 `~/.teamx/letters/`，插件用模块级常量
  解析服务器地址；同服务器多份邀请函时证书选择不确定（readdir 顺序而非最新），
  且 `discoverServerUrl` 与 `letterMtls` 启发式不一致导致 URL/证书错配。
  详见 `docs/29-teamx-project-scoped-issue.cn.md`。
- **修复**：Rust `teamx_home()` 默认 `<cwd>/.teamx`（`TEAMX_HOME` 覆盖）；插件
  `TEAMX_SERVER_URL`/`TEAMX_HOME` 改为按 cwd 解析；全部工具传 `context.directory`；
  `team import`/`git setup` 强制本地落盘项目 `letters/`；session→directory 映射支撑后台任务。
- **行为变化**：每个 workspace（opencode 启动目录）拥有独立的 `.teamx/` 空间，
  一项目一身份；本地 DB/CA/instance/serve/tunnels 均随项目。
- 服务器部署固定 `TEAMX_HOME` 保留 CA；本地与云服务器二进制已更新并端到端验证。

## 0.4.1 — 2026-09-01

### grill-doc 允许早于团队创建运行（去除 owner 前置条件）

grill-doc 是收集需求过程，现在**可以先于团队存在而运行**（pre-team 模式）：

- `protocols/grill-with-docs.md`：启动不再要求「已入队且为 team owner」——
  `teamx_sync` 改为尽力而为；未入队时进入 pre-team 设计会话（记录写入 Git，
  不发布 ledger 事件），已入队且为 owner 时按团队内会话继续。
- pre-team 模式下跳过团队 ledger 能力：Fact Request 委派与决策广播仅在团队
  存在时使用，否则回退到本地调查或直接询问 owner。
- 记录 schema 与 ledger payload 结构不变，`protocol_version` 保持 1（向后兼容，
  已有 v1 记录可正常 resume）。
- 重新生成 `/team-grill` 命令资产与 DSH skill 适配器；`/team start` 同步更新。
- 更新 `docs/22/23/27` 文档与协议资产测试。

验证：`node scripts/generate-grill-protocol.mjs --check` 无 drift；
`node tests/protocol-assets.test.mjs` 通过；dsh-plugin typecheck 通过。

## 0.4.0 — 2026-08-30

### 合并 grill-with-docs（owner 主导设计会话）到企业版

将社区版（main 分支）的 grill-with-docs 设计会话功能合并进 enterprise 分支
（cherry-pick `d868b09` + `7858b62`，本地-only，未 push）：

- **新增 `protocols/grill-with-docs.md`** — host-neutral 设计会话协议源
  （owner 主导、多轮访谈、证据收集、ADR 沉淀）。
- **新增 `scripts/generate-grill-protocol.mjs`** — 从协议源确定性生成各端
  适配产物（`--check` 可校验产物与源是否一致）。
- **opencode 端**：新增 `/team-grill` 斜杠命令（`assets/command/team-grill.md`
  + `.en.md`），支持 `--doc` / `--resume`；`install.sh` 同步安装。
- **DSH 端**：新增运行时 skill `teamx-grill-with-docs`
  （`src/grill-with-docs.generated.ts`），`src/index.ts` 注册。
- **文档**：`docs/22-manual-grill-with-docs.md`（手动测试）、
  `docs/23-manual-grill-with-docs-usage.md`（使用指南）、
  `docs/design/grill-with-docs.md`、ADR 0001/0002、`CONTEXT.md`。
- **测试**：`tests/protocol-assets.test.mjs` 校验生成产物一致性；
  `tests/run-all.sh` plugin 步骤插入 `generate-grill-protocol --check`。

验证：`cargo test -p teamx` 回归全量 117 passed（本次合并不含 Rust 改动，117 为
enterprise 全量基线）；opencode-plugin typecheck + build 通过；
协议生成/校验脚本通过。

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

- CLI/DB schema 迁移到 **v13**（enterprise 合并后编号：v11 presence、v12 is_lead、v13 users，新增 `users` 表、`members.user_id`、`invitations.user_id`）。现有数据库首次运行自动升级。
- 隧道访问语义对 *用户绑定* 设备有变化：同用户设备可互访对方隧道；其他用户拒绝。旧（未绑定）成员保持原有团队级访问，不影响现有团队。

## 0.3.2 — 2026-08-24

### 透明代理 DNS 方案（本地 DNS 代理 + 出口解析）+ 控制面板 card 改造

**透明代理无感知 DNS**（替代放弃的 fake-ip DNS）：
- macOS 的 `mDNSResponder` 不接受 `198.18.0.0/15` fake-ip 应答，且 fake-ip DNS
  会劫持 tun0 自身到 server 的解析导致 bridge 超时 → 放弃 fake-dns（`--fake-dns`
  改为显式参数，默认关闭）。
- 公共 DNS（8.8.8.8 / 1.1.1.1 / DoH）在墙内均被污染或阻断；只有海外出口
  （`egress2`，AWS 东京）能解析 Google 真实 IP 并直连。
- 新方案：本地 DNS 代理监听 `127.0.0.1:53`，系统 DNS 指向它。命中路由规则的
  域名经 teamx mTLS 通道让出口解析（真实 IP），并加入 tun 的主机路由后应答；
  其余域名转发上游系统 DNS。应用无需任何配置即可访问被代理域名。
- 新增 `dns_proxy.rs`（本地 DNS 服务器）；server 端 `team.resolve_dns` RPC +
  tunnel registry `resolve`/`complete_resolve`；exit 端 `resolve` 指令；
  `teamx dns list` / `teamx dns resolve <domain>` CLI。
- 路由表显式 CIDR 段规则直接加网络路由（覆盖 Google 等大 CDN 段），域名规则
  定期解析为单 IP 主机路由兜底。

**控制面板 card 改造**（Swift App）：
- 上下布局：底部 term 风格日志区；顶部 `NSSegmentedControl` 切换 8 个 card
  （连接状态 / 虚拟网卡 / SOCKS5 代理 / 默认出口 / 隧道 / tun0 路由规则 /
  路由表 / DNS）。
- 新增「路由表」card：显示默认路由 + 对给定 IP/域名执行 `traceroute`。
- 新增「DNS」card：显示默认 DNS + 对给定域名执行 `teamx dns resolve`（经出口，
  无污染）。

**其他修复**：
- 点击托盘图标菜单导致键盘/UI 卡死：`menuNeedsUpdate` 同步执行 `tunnel list`
  （server 往返）阻塞主线程 → 默认出口/出口列表改为读缓存，后台定时刷新。
- tun0 核心修复：smoltcp Tx token 真正写回 tun fd；TUN fd 非阻塞 + rx_buf 不再
  truncate；macOS utun 截断的 TCP SYN 补齐到 total_len；checksum 改为 TX 计算
  /RX 忽略；仅在 `Established` 后接新连接；`open_tunnel_bridge` 的 stream_id
  传递 bug（否则数据被缓存永不发送）；主循环改异步 sleep 避免饿死 spawn 任务。
- 开机自启默认关闭（移除 LaunchAgent，打包不带 `--install-agent`）。

## 0.3.1 — 2026-08-23

### External rule-config compatibility + L1 desktop tray

**外部规则配置兼容模式**（`tun0 start --rules-config <path>`，导入其他代理
客户端的 YAML 规则）：解析其 `rules`，映射到 teamx 路由表（DOMAIN-SUFFIX→
通配、DOMAIN→精确、IP-CIDR→CIDR、MATCH→default）。`proxies`/`proxy-groups`
忽略（出口从 teamx 的 egress 集选择）；DIRECT/REJECT 规则 v1 跳过。
规则解析模块 + 8 个单元测试。

**L1 桌面托盘**（`teamx gui`）：tray-icon + tao（纯 Rust，跨平台 macOS 菜单栏 /
Linux appindicator）。菜单启停 tun0 / SOCKS5 proxy、状态显示、退出。子进程由
`teamx gui` 管理（spawn `teamx tun0 start` / `teamx proxy start`）。L2/L3
（设置窗口、规则可视化、Tauri 完整 GUI）列入 enterprise 分支 TODO。

## 0.3.0 — 2026-08-23

### tun0 virtual NIC (transparent proxy, needs root)

A TUN device routes matching traffic through teamx proxy exits **without**
configuring apps — apps keep talking to the network normally; traffic to the
fake-ip range is transparently captured, TCP is reassembled in user space
(smoltcp) and forwarded through the chosen exit.

- **`teamx tun0 start`** (requires root): creates the tun device, injects the
  fake-ip route, runs a fake-ip DNS responder and bridges connections.
  - `--exit <name>` / `-f <routes.json>` to pick the default / routed exit.
  - `--ip` (gateway, default 198.18.0.1), `--net/--net-prefix` (fake-ip net,
    default 198.18.0.0/15), `--max-conns`.
- **`teamx tun0 stop|status`**.
- **fake-ip DNS** (`tun_dns.rs`): answers A queries with 198.18.x.x fake IPs,
  keeps a fake_ip↔domain map so the exit dials by hostname.
- **smoltcp user-space stack** (`tun_stack.rs`) with a local patch
  (`vendor/smoltcp`, `[patch.crates-io]`) enabling `listen(0)` as an any-port
  wildcard — required for transparent interception of arbitrary target ports.
- **Bridge** (`tun_socks.rs`): per-connection `open_tunnel_bridge` (extracted
  from `run_socks5_proxy`) reuses the existing WS→server→egress channel;
  **server and egress are unchanged**.
- Cross-platform: Linux `/dev/net/tun` + `ip route`, macOS `utun` + `route`.
- Reuses `routes.rs` for per-target exit selection (IP/CIDR + fake-ip domain).
- Tests: 64 unit tests (fake-ip alloc/lookup/DNS round-trip, routes, …).
  Linux live verification on hub03: tun0 up, fake-ip route, DNS returns fake
  A, example.com content fetched through the tunnel via egress2, .com flows
  confirmed dialing through the egress2 exit. macOS: `tests/tun0-macos-test.sh`.
- Docs: `docs/09-design-tun0.md` (detailed design).

## 0.1.2 — 2026-08-22

### Proxy per-target egress routing

A single local SOCKS5 port can now pick which `proxy exit` each CONNECT target uses, based on the target domain/IP — so multiple exits (e.g. different cloud hosts) can be combined behind one proxy.

- **New `routes.rs`**: ordered first-match rules — exact domain, suffix wildcard (`*.cn`), IPv4/IPv6 CIDR, exact IP. Pure-function matcher + unit tests.
- **CLI**: `proxy start -f/--routes <file>` (ephemeral JSON override), `proxy start` reads the **SQLite route table** by default; `--exit` remains the legacy fixed-exit fallback.
- **SQLite persistence** (db migrate v6): `proxy_routes` (seq/match/exit) + `proxy_settings` (`default_exit`). Managed via `teamx proxy routes list|add|remove|set-default|import|clear`.
- `tunnel_client::run_socks5_proxy` resolves the exit per CONNECT target; server/exit side unchanged.
- Tests: routes.rs unit (matching + DB round-trip + upsert), `tests/proxy-routes-test.ts` end-to-end (file routing + SQLite routing + fixed-exit regression) — all pass.
- Docs: `docs/08-design-proxy-routes.md` (design + usage), `docs/20-manual-tunnel-proxy-cli.md` §5.5 (multi-exit routing runbook).

## 0.1.1 — 2026-08-22

修复全部高/中优先级发现（见 `docs/review/code-review-codex-0817.md`）：

### Tunnel WS keep-alive + auto-reconnect

Long-idle tunnel WebSockets (`/tunnel` provider + `/tunnel/forward` consumer) were silently dropped by NAT/middleboxes, leaving stale registered tunnels and half-open connections — proxy flows then failed with `SOCKS5 (5) Connection refused` / `curl: (97)` until both processes were manually restarted.

- **Server heartbeat** (`serve.rs`): both tunnel WS handlers now send a 30s application-level `{"type":"ping"}` (mirroring the `/ws` channel) and reply `pong` to client pings.
- **Client pong** (`tunnel_client.rs`): `proxy exit` / `tunnel expose` / `tunnel forward` / `proxy start` reply `pong` to server pings.
- **Provider auto-reconnect** (`run_expose`): `proxy exit` / `tunnel expose` automatically reconnect with exponential backoff (1s→30s) when the WS drops and re-register the tunnel, so consumers are never stranded.
- `handle_tunnel_forward` merged two socket/provider tasks into one `select!` loop (single sink owner).
- Docs: `docs/20-manual-tunnel-proxy-cli.md` §11 documents keep-alive + self-healing + systemd/while-loop production runbook.

Verified: 43 unit tests, `tests/tunnel-proxy-comprehensive.ts` (43 asserts), `tests/proxy-test.ts`, `tests/cross-network.sh`; live kill/restart test on a cloud host confirms the provider reconnects and re-registers within seconds, and `systemd Restart=always` recovers a SIGKILLed server.

## 0.1.0 — 2026-08-17

First release: Rust CLI (SQLite event ledger + state machine + mTLS server) + opencode plugin (30+ tools + `/team` commands + agent routing) + network mode + multi-member collaboration.

### Network Mode (N0—N4)

Team collaboration is no longer limited to a single machine. Members connect to a shared server over LAN with mTLS identity and real-time WebSocket push.

- **mTLS server** (N0): `teamx serve` runs an axum + tokio-rustls server with mandatory mutual TLS. RPC handlers derive member identity from client certificate CN (replacing self-reported session keys). `team.import` binds certificate identity to a pre-allocated seat via a dedicated path. Supports `--san <ip>` for LAN IP in server certificate SAN; plugin `serve start` auto-detects and passes the local IP.
- **WebSocket push** (N1): `GET /ws` endpoint registers subscribers by client certificate CN. Events are fanned out per team via `broadcast::Hub` (team→member→sender registry). 30s heartbeat, automatic cleanup on disconnect.
- **Revocation enforcement** (I2): `team invite-revoke` triggers active WebSocket disconnect for the revoked member. Certificates are rejected at connect/RPC time. Certificate = "can connect", approve = "can work"; revocation cuts off both.
- **Plugin event-driven** (N3): When WebSocket is connected, the poller sleeps (zero polling). On disconnect, exponential backoff reconnect (1s→60s) with polling fallback. Event frames are debounced (200ms) to batch bursts.
- **Cross-network verification** (N4): `tests/cross-network.sh` validates the full mTLS chain over a non-loopback IP. `docs/n4-cross-network.md` provides a two-machine runbook.

### Invitation Letters (I0—I1)

Members join via one-time invitation letters containing mTLS client certificates, replacing shared session keys.

- **`team invite "<role>: <desc>"`** (owner): Issues an mTLS client certificate (CN=`member:<id>:<role>`) and generates a self-contained invitation letter (`teamx-inv:v1:<base64>`). Role is automatically added to the team catalog.
- **`team import <letter>`** (member): Unpacks the letter, stores certs, and claims the pre-allocated seat (pending, auto-roled). Cross-machine: stores locally and prompts to connect for registration.
- **`team invite-list` / `team invite-revoke <id>`** (owner): List/revoke invitation letters; revoked certificates are rejected at connect.
- Plugin tools: `teamx_team_invite`, `teamx_team_import`, `teamx_team_invite_list`, `teamx_team_invite_revoke`.
- Slash commands: `/team invite`, `/team import`, `/team invite-list`, `/team invite-revoke`.

### Custom Roles

- Members can propose custom roles (`role propose`); owner approval automatically grants the role to the proposer.
- Owner can update any role's label/description (`role update`).
- `role set` only accepts approved roles (built-in + approved custom); pending roles trigger an error with a reminder to wait for approval.

### Command System

- **`/team <subcommand>`** routing: `create`, `join`, `status`, `sync`, `goal`, `approve`, `deny`, `role`, `state`, `ask`, `respond`, `publish`, `archive`, `help`. All subcommands have flat aliases (`/team-create`, `/team-invite`, ...) for tab completion.
- `teamx log` audit replay (resolves member names, supports `--team`, `--session`, `--limit`, `--after`).
- Owner uniqueness constraint: one session can own at most one non-archived team; archive before creating another.

### Three-Person Collaboration Demo

- `docs/demo-3p.md`: owner + contributor + reviewer workflow.
- `tests/three-member.sh`: automated end-to-end test (multi-member approval, parallel roles, Q&A, broadcast, close+archive).

### Production Hardening

- **State machine completeness**: removed unreachable `paused` state; added `team archive` (completed→archived) and `member set-state idle|active`; `achieved` can be reopened by the owner (start/resume→in_progress, refine→refining).
- **Data model**: removed redundant `sessions` table; added `UNIQUE(team_id, session_key)` on `members` and `UNIQUE(team_id)` on `goals`; member re-entry reuses the same row; sync cursor advances monotonically.
- **Authorization/robustness**: owner cannot `team leave`; `team approve/deny` supports `--team` disambiguation; `team create` is idempotent (same name reuses); `publish --data` falls back to `{"message": s}` for non-JSON.
- **Notification storm fix**: per-session notified-seq watermark; same batch of events only toasts once.
- **M2 polling + agent injection**: works without a server; polling refreshes digest + `experimental.chat.system.transform` injects team state into agent context.

### Code Review Fixes

Fixed all high/medium priority findings (see `docs/code-review-codex-0817.md`):

- **Cross-team read bypass** (security): non-members can no longer read invite tokens, members, roles, or events of arbitrary teams.
- **Pending members cannot publish** (authorization).
- **Non-object payload panic** (robustness): `publish --data '[]'` no longer panics.
- **Invitation letter path traversal** (security): `invitation_id` must be a UUID.
- **PKI partial-rebuild correctness**: `server.key` loss no longer invalidates issued member certificates.
- **Auto-execute seq watermark** (correctness): `shouldAutoExecute` uses `e.seq > lastExecutedSeq` for repeat triggers.
- **Directed task type matching** (correctness): `assignedToMe` matches any event with `assignee_member_id`.
- **Non-owner cannot `role set owner`** (authorization).

### Security Model

V1 has no real authentication (`session_key` self-reported, `invite_token` visible to all members). This is a "trust this machine" collaboration convention. Documented in `docs/goal-v1.md` and `v1-spec.md`.

### Testing

`tests/run-all.sh` runs a 9-step automated suite: CLI edge cases, mTLS identity + revocation, WebSocket push + disconnect + reconnect, cross-network LAN verification, and plugin unit tests (auto-execute, WebSocket, state machine). `tests/acceptance.sh` runs real-model acceptance tests (headless `opencode run --agent teamx`).

### Tech Stack

Rust CLI (axum + tokio-rustls + rusqlite + rcgen) · TypeScript plugin (opencode plugin API) · SQLite WAL · mTLS (ring + x509-parser + base64) · WebSocket (axum ws)
