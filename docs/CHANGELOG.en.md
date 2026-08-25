# Changelog

## 0.3.2 — 2026-08-24

### Transparent-proxy DNS approach (local DNS proxy + egress-side resolution) + control panel card rework

**Seamless DNS for the transparent proxy** (replacing the abandoned fake-ip DNS):
- macOS `mDNSResponder` does not accept `198.18.0.0/15` fake-ip answers, and fake-ip DNS
  hijacks tun0's own resolution of the server, causing bridge timeouts → fake-dns was
  dropped (`--fake-dns` becomes an explicit flag, off by default).
- Public DNS (8.8.8.8 / 1.1.1.1 / DoH) is all poisoned or blocked behind the GFW; only the
  overseas egress (`egress2`, AWS Tokyo) can resolve Google's real IPs and connect directly.
- New scheme: a local DNS proxy listens on `127.0.0.1:53` with the system DNS pointing at
  it. Domains matching route rules are resolved through the teamx mTLS channel by the
  egress (real IP), added as host routes on the tun interface, then answered; other
  domains are forwarded upstream to the system DNS. Apps access proxied domains with no
  configuration whatsoever.
- Added `dns_proxy.rs` (local DNS server); server-side `team.resolve_dns` RPC +
  tunnel registry `resolve`/`complete_resolve`; exit-side `resolve` command;
  `teamx dns list` / `teamx dns resolve <domain>` CLI.
- Explicit CIDR-block rules in the route table directly add network routes (covering large
  CDN ranges like Google); domain rules are periodically resolved into single-IP host
  routes as fallback.

**Control panel card rework** (Swift App):
- Vertical layout: term-style log area at the bottom; an `NSSegmentedControl` at the top
  switching between 8 cards (connection status / virtual NIC / SOCKS5 proxy / default
  egress / tunnels / tun0 route rules / route table / DNS).
- New "Route Table" card: shows the default route + runs `traceroute` to a given IP/domain.
- New "DNS" card: shows default DNS + runs `teamx dns resolve` for a given domain
  (via the egress, unpolluted).

**Other fixes**:
- Clicking the tray icon menu froze keyboard/UI: `menuNeedsUpdate` synchronously ran
  `tunnel list` (a server round trip) blocking the main thread → default egress/egress list
  now read from cache, refreshed periodically in the background.
- tun0 core fixes: smoltcp Tx token now actually writes back to the tun fd; TUN fd set
  non-blocking + rx_buf no longer truncated; truncated TCP SYN from macOS utun padded out
  to total_len; checksums computed on TX / ignored on RX; new connections accepted only in
  `Established`; fixed stream_id passing bug in `open_tunnel_bridge` (otherwise data sat in
  a buffer forever, never sent); main loop switched to async sleep to avoid starving
  spawned tasks.
- Launch-at-login now defaults to off (LaunchAgent removed; package ships without
  `--install-agent`).

## 0.3.1 — 2026-08-23

### External rule-config compatibility + L1 desktop tray

**External rule-config compatibility mode** (`tun0 start --rules-config <path>`, importing
other proxy clients' YAML rules): parses their `rules`, mapping them onto the teamx route
table (DOMAIN-SUFFIX→wildcard, DOMAIN→exact, IP-CIDR→CIDR, MATCH→default).
`proxies`/`proxy-groups` are ignored (exits are chosen from teamx's egress set);
DIRECT/REJECT rules are skipped in v1.
Rule-parsing module + 8 unit tests.

**L1 desktop tray** (`teamx gui`): tray-icon + tao (pure Rust, cross-platform macOS menu
bar / Linux appindicator). Menu starts/stops tun0 / SOCKS5 proxy, shows status, quits.
Subprocesses are managed by `teamx gui` (spawning `teamx tun0 start` /
`teamx proxy start`). L2/L3 (settings window, rule visualization, full Tauri GUI) are on
the enterprise branch TODO.

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
