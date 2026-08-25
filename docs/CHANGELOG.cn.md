# Changelog

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
