# Design 15 — Transparent Proxy (Local DNS Proxy + Exit Resolution)

> Status: implemented & verified on macOS (client) + Linux (server/exit).
> Date: 2026-08-24

## 1. Background / Problem

`tun0` (a root TUN device) intercepts matching traffic and tunnels it through a
teamx **proxy exit** (e.g. `egress2` on an overseas node). For a "transparent
proxy" (apps need no proxy configuration), the tricky part is DNS:

- **Fake-IP DNS was abandoned.** macOS `mDNSResponder` does not reliably accept
  responses from the `198.18.0.0/15` reserved range, so system queries either
  fell back to a censored resolver or timed out. It also hijacked tun0's *own*
  connection to the teamx server (the server hostname was resolved to a fake IP,
  breaking bridges).
- **IP-only routing is not enough.** Google/YouTube CDN IPs are huge, dynamic and
  geo-distributed. Pre-resolving a few domains misses most IPs, and the system
  resolver returns **GFW-polluted** addresses (observed: `185.45.5.35`,
  `174.132.167.252`, `104.244.42.197`) that are not Google IPs at all.
- **Public DNS is unusable from the censored network:** `8.8.8.8`, `1.1.1.1`
  UDP, and DoH endpoints (`dns.google`, `cloudflare-dns.com`) are all hijacked
  or blocked. Even the server node (`hub03`, Aliyun, inside the GFW) resolves
  Google to a Facebook IP.

**Only an overseas exit (e.g. `egress2` on AWS Tokyo, `35.79.166.197`) has an
uncensored resolver and can reach Google directly** (verified: resolves
`142.251.x`, `curl -I https://www.google.com` → 204).

## 2. Chosen Design

Run a **local DNS proxy on `127.0.0.1:53`** (loopback, so `mDNSResponder` talks
to it normally) and point system DNS at it. The proxy decides per domain:

- **Intercepted domains** (match the route table's domain rules) → resolved
  **through the teamx channel to the exit** (`egress2`'s uncensored resolver);
  the returned real IPs are added as host routes on the tun device and answered
  to the client. The app then connects to the real IP, the host route sends it
  into tun0, and tun0 proxies it out the exit.
- **Everything else** → forwarded unchanged to the upstream system DNS
  (normal/censored-but-fine for domestic sites).

This gives apps transparent, uncensored access to proxied domains with **no
DNS hijacking** and **no fake IPs**.

## 3. Component Map

### 3.1 Client side (macOS, runs in the `teamx tun0 start` process)

| File | Role |
|---|---|
| `crates/teamx/src/dns_proxy.rs` (new) | Local DNS server on `127.0.0.1:53` (a dedicated blocking thread). For intercepted domains calls `tunnel_client::resolve_dns`, routes each real IP via `tun_dev::add_ip_route`, records `ip -> domain` in the shared `ip_map`, and answers with `build_a_response`. Non-intercepted queries are forwarded to the upstream DNS. |
| `crates/teamx/src/tun_dns.rs` | New helpers: `parse_dns_query` (QNAME + qtype + question-end offset) and `build_a_response` (A-record answer; note **TTL is u32**, not u16). Also keeps the fake-IP responder for the optional `--fake-dns` mode. |
| `crates/teamx/src/tun_socks.rs` | `run_tun_proxy` starts the DNS proxy, calls `tun_dev::set_system_dns_single("127.0.0.1")`, and keeps `ip_route_loop` (periodic domain→IP host routes as a fallback) + CIDR network routes for large CDN ranges. `resolve_target` consults the `ip_map` so tun0 dials by hostname (preserving TLS SNI) when possible. |
| `crates/teamx/src/tun_dev.rs` | New: `set_system_dns_single` (one DNS, no fallback; saves a backup), `system_dns_servers`, `add_ip_route`/`del_ip_route`. Existing `set_system_dns`/`restore_system_dns` (fake-IP mode) keep original DNS as fallback and back up to `~/.teamx/dns-backup.json`. |
| `crates/teamx/src/tunnel_client.rs` | New `resolve_dns(server_url, exit, name)` (calls the server RPC). The **exit side** gained a `resolve` instruction handler that resolves with the exit's resolver and replies `resolve_result`. |
| `crates/teamx/src/cli.rs` / `commands.rs` | New `teamx dns` command: `dns list` (default system DNS) and `dns resolve <domain>` (via exit, uncensored). Also `Tun0Cmd::Start --fake-dns` (default off). |

### 3.2 Server side (`teamx serve`, hub03)

`crates/teamx/src/serve.rs` + `crates/teamx/src/tunnel.rs`:

- `TunnelRegistry::resolve` sends a `resolve` frame to the named proxy exit and
  registers a oneshot waiter (`resolve_waiters`).
- `team.resolve_dns` RPC: finds the caller's team, forwards to the exit,
  waits up to 6 s for `resolve_result`, returns the IP list.
- The provider `/tunnel` handler recognizes `resolve_result` and completes the
  waiter.

### 3.3 Exit side (`teamx proxy exit`, egress2)

`crates/teamx/src/tunnel_client.rs` `expose_once` gained a `resolve` instruction
handler (resolves via the exit's system resolver — uncensored).

## 4. `teamx dns` CLI

```
teamx dns list                # default system DNS (scutil --dns on macOS)
teamx dns resolve <domain>    # resolve via the default exit (uncensored)
teamx dns resolve <domain> --exit <name>
```

## 5. Control-Panel Redesign (Swift App)

`app/Sources/TeamxApp/ControlPanelController.swift` was restructured into a
**top/bottom layout**:

- **Bottom**: terminal-style log (monospaced `NSTextView`, fixed height) with
  copy/clear actions.
- **Top**: an `NSSegmentedControl` tab bar switching between 8 cards:
  1. 连接状态 (server/member presence + metrics table)
  2. 虚拟网卡 (tun0 start/stop/restart + status)
  3. SOCKS5 代理 (proxy start/stop + status)
  4. 默认出口 (default exit picker)
  5. 隧道 (tunnel table, read-only)
  6. tun0 路由规则 (route table)
  7. **路由表** (default route + **traceroute** query box → `traceroute <host>`)
  8. **DNS** (default DNS + **domain resolution** query box → `teamx dns resolve`)

New card helpers: `buildCards`, `buildRouteTableCard`, `buildDNSCard`,
`makeTermScroll`, `showCard`, `tabChanged`.

## 6. Other Changes Along The Way

- **Menu freeze fix**: clicking the tray icon opened the menu and
  `menuNeedsUpdate` synchronously ran `tunnel list` (a server round-trip) on the
  main thread, freezing input. `defaultExit()`/`listExits()` now read a cache
  refreshed in the background (`TeamxCore.refreshExitCache`, every 5 s +
  on launch).
- **Auto-login off**: removed `~/Library/LaunchAgents/io.flomesh.teamx.plist`;
  `build-teamx-app.sh` only installs it with `--install-agent`.
- **tun0 core fixes** (needed for the whole chain to work):
  - smoltcp `TxToken` now actually writes packets back to the tun fd (before,
    replies were silently dropped).
  - TUN fd set non-blocking; `rx_buf` no longer truncated (truncating shrank
    the read buffer and truncated large SYN packets).
  - macOS utun truncates TCP SYN by 4 bytes but keeps `total_len` → pad to
    `total_len` before parsing.
  - Checksums: skip RX verification, **compute on TX** (`Checksum::Tx`);
    a TCP checksum of 0 is rejected by the host stack.
  - `take_new_connection` only fires on `State::Established`; a handshake-in-
    progress socket is no longer reset as EOF.
  - `open_tunnel_bridge`: the `stream_id` read during the `stream_open` wait was
    consumed by the caller and never seen by the spawned pump → `sid` was always
    `None`, so all outbound data was buffered forever. Now the caller captures
    and passes it in.
  - The tun poll loop sleeps asynchronously (a synchronous `phy::wait` starved
    the current-thread runtime and bridge tasks never ran).

## 7. Deployment Notes

```
# client (macOS) — build & copy into the app bundle
cargo build -p teamx
./scripts/build-teamx-app.sh            # no --install-agent → no auto-login

# server (hub03, x86_64) + exit (egress2, aarch64)
rsync -az --delete --exclude target --exclude dist --exclude app \
  --exclude .git --exclude docs --exclude node_modules --exclude '*.db' \
  ./ ubuntu@<host>:~/teamx/
ssh <host> 'export PATH=/home/ubuntu/.cargo/bin:$PATH; cd ~/teamx && cargo build --release'
ssh <host> 'sudo systemctl stop <svc>; cp ~/teamx/target/release/teamx ~/.local/bin/teamx; sudo systemctl start <svc>'
```
- hub03: `teamx-serve` (+ `teamx-proxy-exit`), port 8888.
- egress2: `teamx-proxy-exit2` → `start-exit2.sh`.

## 8. Verification (2026-08-24)

With `tun0` running and system DNS = `127.0.0.1`:

```
networksetup -getdnsservers Wi-Fi        → 127.0.0.1
dig www.google.com A                     → ANSWER: 8 (142.251.x, real Google IPs)
curl -skI https://www.google.com/generate_204 → 204
curl -sI https://www.baidu.com           → 200 (non-intercepted, direct)
teamx dns list                           → 192.168.31.1
teamx dns resolve www.google.com         → 142.251.150.119 …
```

## 9. Known Limitations

- If the tun0 process dies, system DNS stays at `127.0.0.1` and the network is
  unreachable until `teamx tun0 stop` (restores DNS) or a manual reset. A
  watchdog / auto-restore is future work.
- `dns resolve` answers only A records; AAAA is forwarded upstream.
- IP host routes are refreshed every 5 minutes (CDN rotation); in practice the
  DNS proxy re-adds routes on every query, so coverage is near-real-time.
- traceroute runs on the client (not through the exit); it shows the client's
  path, which may be partially blocked.
