# tun0 Virtual Network Device — Detailed Design (Implementation Blueprint)

- Document type: design (finalized, entering implementation)
- Related: `09-design-tun0.md` (feasibility study), `06-design-proxy.md`, `08-design-proxy-routes.md`
- Date: 2026-08-22 (design) · 2026-08-23 (implementation complete)
- Target version: teamx 0.3.0
- Platforms: Linux + macOS (cross-platform TUN)
- Status: ✅ Implemented (see §11 implementation record)

## 1. Design Decisions (Confirmed)

| # | Decision Point | Conclusion |
|---|---|---|
| D1 | Implementation path | **Path A** (tun2socks style): a local user-space TCP/IP stack reassembles IP packets into TCP streams, reusing the existing proxy→server→egress channel |
| D2 | Domain routing | **fake-ip DNS interception** (done in v1): local DNS → fake IP → reverse mapping back to domain, egress dials by domain |
| D3 | UDP | **v1 supports local UDP DNS handling** (UDP/53 hijacked for fake-ip resolution); other UDP is dropped; full UDP over proxy deferred |
| D4 | Platforms | **Both Linux + macOS** (`tun` crate provides unified wrapping; see §5) |
| D5 | Privileges | **Requires root** (creating tun + injecting routes); detect at startup and fail with a clear error |
| D6 | Concurrency | Pre-allocated socket slots (smoltcp has no accept/backlog); v1 defaults to 64 connections |
| D7 | Reuse | Route matching (routes.rs), proxy client connection logic, server/egress fully reused |

## 2. Overall Architecture

```
                ┌─────────────── Local (requires root) ────────────────┐
 Application    │                                                      │
  │ system      │   ┌────────┐   ┌──────────────┐                      │
  ▼ routing     │   │  tun0   │──▶│  smoltcp     │                      │
 IP packets ───▶│   │ device │   │ user-space   │                      │
  ▲             │   └────────┘   │ TCP/IP stack │                      │
  │             │                │ (reassemble/ │                      │
  │             │                │  handshake)  │                      │
  │             │                └──────┬───────┘                      │
  │             │                       │ TCP stream + remote endpoint │
  │             │                       ▼                              │
  │             │                ┌──────────────┐                      │
  │             │                │ tun_socks    │  ← new module        │
  │             │                │ bridge       │                      │
  │             │                │ (reuses      │                      │
  │             │                │  proxy)      │                      │
  │             │                └──────┬───────┘                      │
  │             │                       │ one WS per connection        │
  └─────────────┼───────────────────────┼──────────────────────────────┘
                │                       │ WS (mTLS)
                │                       ▼
                │              teamx server (zero changes)
                │                       │
                │                       ▼
                │              egress (zero changes, dynamic dialing)
                ▼
         fake-ip DNS (local port-53 hijack, fake_ip→domain mapping)
```

### Key Data Flow (TCP)

```
1. Application connects to an IP:port in the fake-ip range (system routes point to tun0)
2. tun0 receives SYN → smoltcp completes the handshake (Listen→SynReceived→Established)
3. tun_socks detects the new Established connection:
   remote_endpoint = (fake_ip, port)
   → look up fake_ip in the mapping table → on hit, obtain domain
   → otherwise use the IP directly
   → establish a WS /tunnel/forward connection to the teamx server, send {"type":"connect","name":"<exit>","target":"<domain|ip>:<port>"}
   → pump bytes both ways (tun-side socket recv_slice/send_slice ↔ WS reads/writes)
4. egress receives target and dials dynamically (existing logic, zero changes)
```

### Key Data Flow (DNS, fake-ip)

```
1. Application resolves example.com → UDP packet enters tun0 (destination DNS server IP)
2. tun_socks UDP handling: recognizes this as a DNS query (port 53) → hands it to the fake-ip resolver
3. Resolver: queries the system DNS or upstream DNS to get the real IP, allocates a fake IP
   (from 198.18.0.0/15), records the fake_ip→domain mapping, and returns the fake IP as an
   A-record answer to the application
4. The application subsequently connects to the fake IP → follows the TCP flow → domain restored → egress dials by domain
```

## 3. Module Breakdown (crates/teamx/src/)

```
src/
├── tun_dev.rs      # Cross-platform TUN device wrapper (macOS utun / Linux tun), wraps the tun crate
├── tun_stack.rs    # smoltcp integration: Device impl + Interface config + poll loop
├── tun_socks.rs    # Connection bridge: Established connections → proxy WS; byte pumping; UDP/DNS handling
├── tun_dns.rs      # fake-ip DNS: local port-53 hijack + fake_ip↔domain mapping table
├── tun_cli.rs      # `teamx tun0 start/stop` commands (privilege check, route injection, process management)
├── routes.rs       # [Reused] route matching (IP/CIDR + domains, see 08-design)
└── tunnel_client.rs# [Reused] WS connection setup / socks5_proxy core logic
```

### 3.1 `tun_dev.rs` — Cross-platform TUN Wrapper

```rust
pub struct TunDevice {
    pub dev: tun::Device,          // underlying tun crate device
    pub name: String,              // actual device name (utunN / tunN)
}

impl TunDevice {
    /// Create and configure the TUN device. Requires root.
    /// - macOS:  name=utunN (auto-assigned), the tun crate runs route add automatically
    /// - Linux:  name=tunN, set IP/netmask via ioctl (ensure_root_privileges)
    pub fn create(name: Option<&str>, ip: Ipv4Addr, netmask: Ipv4Addr, mtu: u16) -> Result<TunDevice, String>;

    /// Read one IP packet (non-blocking, EWOULDBLOCK → None)
    pub fn read_packet(&mut self, buf: &mut [u8]) -> Option<usize>;

    /// Write one IP packet (sent into tun, i.e. toward the application-side network stack)
    pub fn write_packet(&mut self, packet: &[u8]) -> Result<(), String>;

    pub fn as_raw_fd(&self) -> i32;   // for smoltcp phy::wait polling
}
```

macOS/Linux differences (`#[cfg(target_os)]`):
- **macOS**: `tun::Configuration` sets `tun_name("utunN")` + address/netmask/destination/up; `tun::create` internally invokes `ifconfig` and `route` commands (root required).
- **Linux**: `platform_config(|c| c.ensure_root_privileges(true))`; `tun::create` uses ioctl. Requires `/dev/net/tun` to exist and root.
- **Route injection**: routes pointing the fake-ip range (198.18.0.0/15) at tun0 must be added on both platforms:
  - macOS: `sudo route -n add -net 198.18.0.0/15 <tun_ip>`
  - Linux: `sudo ip route add 198.18.0.0/15 dev tunN`

### 3.2 `tun_stack.rs` — smoltcp Integration

```rust
pub struct TunStack {
    device: TunDevice,
    iface: smoltcp::iface::Interface,        // Config::new(HardwareAddress::Ip)
    sockets: smoltcp::iface::SocketSet,      // pre-allocated TCP sockets
    // bridge state corresponding to each socket
    bridges: Vec<Option<TcpBridge>>,
}

pub struct TcpBridge {
    pub remote: IpEndpoint,       // destination (ip:port) or fake-ip
    pub state: BridgeState,       // Connecting | Established | Closing | Closed
    // WS write half/read half to teamx (reuses tunnel_client's channels)
}

pub struct StackConfig {
    pub tun_ip: Ipv4Addr,          // tun0 interface IP (e.g. 10.0.0.1)
    pub netmask: Ipv4Addr,
    pub max_conns: usize,          // pre-allocated socket slots (default 64)
    pub fake_ip_net: (Ipv4Addr, u8), // 198.18.0.0/15
}
```

Core logic:
```rust
impl TunStack {
    /// Main loop: poll-driven + new connection handling + byte pumping
    pub async fn run(mut self, routes: Arc<RouteTable>, conn_maker: Arc<dyn Fn(&str,u16)->Conn>) -> Result<(),String> {
        loop {
            let now = Instant::now();
            self.iface.poll(now, &mut self.device, &mut self.sockets);
            // 1. find newly Established sockets → open WS to egress
            // 2. bytes read via recv_slice → write to WS
            // 3. bytes read from WS → send_slice
            // 4. remote FIN / WS closed → close() / abort()
            // 5. UDP packets → DNS hijack or drop
            // 6. idle-timeout connection cleanup
        }
    }
}
```

### 3.3 `tun_socks.rs` — Connection Bridging (Core)

Bridges an Established smoltcp TCP socket to egress:

```rust
pub async fn bridge_tcp(
    stack: &mut TunStack, sock_handle: SocketHandle,
    exit_name: &str, target: &str,   // "domain:port" or "ip:port"
) {
    // Reuse the WS establishment logic from tunnel_client::run_socks5_proxy (extracted into a reusable function)
    // 1. mtls_for(server_url) → client_config → connect_async_tls_with_config
    // 2. send {"type":"connect","name":exit_name,"target":target}
    // 3. Pump both ways: socket.recv_slice ↔ ws.send(Binary); ws.next() ↔ socket.send_slice
}
```

**Reuse point**: everything after SOCKS5 CONNECT in the existing `run_socks5_proxy` (tunnel_client.rs:510)
(open WS → send connect → pump bytes) should be **extracted into `spawn_tunnel_bridge(server_url, exit_name, target) -> (SendRecvHandle)`**
so that `proxy start` and `tun0` share the same WS bridging code.

### 3.4 `tun_dns.rs` — fake-ip DNS

```rust
pub struct FakeIpDns {
    pub fake_net: (Ipv4Addr, u8),           // 198.18.0.0/15
    map: Mutex<HashMap<Ipv4Addr, String>>,  // fake_ip -> domain
    reverse: Mutex<HashMap<String, Ipv4Addr>>,
}

impl FakeIpDns {
    pub fn alloc(&self, domain: &str) -> Ipv4Addr;   // allocate/reuse a fake IP
    pub fn lookup(&self, ip: Ipv4Addr) -> Option<String>;  // restore the domain
    /// Resolve one DNS query packet (UDP payload), return the answer packet
    pub fn answer(&self, query: &[u8]) -> Option<Vec<u8>>;
    /// Local UDP listener (e.g. 198.18.0.1:53 or hijacking tun's port-53 traffic)
    pub async fn serve(&self, ...) -> Result<(),String>;
}
```

- Upstream resolution: use the system DNS or hardcoded servers (8.8.8.8/1.1.1.1); send a real DNS query to get the real IP, then allocate a fake IP.
- Only A/AAAA queries are answered; other types are forwarded upstream.
- Concurrency safety: `Mutex<HashMap>` + an atomic counter for allocation.

### 3.5 `tun_cli.rs` — CLI Commands

```bash
# Start tun0 (root required): create tun, inject routes, start fake-ip DNS, run the forwarding loop
sudo teamx tun0 start --routes routes.json [--exit default] [--port 1080]
                      [--ip 198.18.0.1] [--net 198.18.0.0/15] [--max-conns 64]
                      [--dev tun0]

# Stop (remove routes, close tun)
sudo teamx tun0 stop [--dev tun0]

# Show status
teamx tun0 status
```

Privilege detection (at startup):
```rust
fn check_privileges() -> Result<(), String> {
    #[cfg(unix)]
    if unsafe { libc::geteuid() } != 0 { return Err("tun0 requires root (sudo)".into()); }
    Ok(())
}
```

Route injection (`--net 198.18.0.0/15`):
```rust
#[cfg(target_os = "macos")]
Command::new("route").args(["-n","add","-net",net,"-interface",dev]).status();
#[cfg(target_os = "linux")]
Command::new("ip").args(["route","add",net,"dev",dev]).status();
```

## 4. Relationship to the Existing Proxy

| Dimension | proxy start | tun0 |
|---|---|---|
| Entry point | Applications explicitly configure SOCKS5 | System routes point to the virtual device (transparent to applications) |
| Protocol | SOCKS5 (L4) | IP packets (L3) → reassembled by smoltcp |
| Privileges | None (pure user space) | **Requires root** |
| Domain routing | Uses the SOCKS5 domain field directly | fake-ip DNS reverse mapping |
| Egress channel | Same (WS→server→egress) | Same |
| Coexistence | ✅ No interference | ✅ No interference |

**Core reuse**:
1. `routes.rs`: `RouteTable::resolve(host)` — tun0 uses it to pick the exit too (IP/CIDR matched directly; domains matched after fake-ip restoration).
2. `tunnel_client.rs`: the extracted `spawn_tunnel_bridge()` WS bridge.
3. server / egress: zero changes.

## 5. Platform Adaptation Matrix

| Item | Linux | macOS |
|---|---|---|
| Device type | `/dev/net/tun` (tunN) | `utunN` |
| tun crate configuration | `platform_config(ensure_root_privileges)` + ioctl to set IP | automatic `ifconfig`+`route` |
| Root requirement | Yes (CAP_NET_ADMIN) | Yes |
| Route injection | `ip route add` | `route -n add` |
| Default MTU | 1500 → 1280 after encapsulation (recommended) | Same |
| fake-ip DNS | Same | Same |
| Testing approach | Cloud host hub03 (root) | Local machine (sudo required) |

**MTU note**: tun0 defaults to 1500, but IP packets are additionally encapsulated over WS(mTLS); tun0 MTU=1280 is recommended
to avoid fragmentation (§7 risk table).

## 6. CLI and Configuration Example

```bash
# 1. Prepare the routing table (reuse proxy routes)
sudo teamx proxy routes set-default egress
sudo teamx proxy routes add '*.cn' egress2

# 2. Start tun0 (root required)
sudo teamx tun0 start --ip 198.18.0.1 --net 198.18.0.0/15

# 3. Applications send traffic through the fake-ip range (DNS hijacked, TCP reassembled and forwarded)
#    Verify:
curl --interface utun0 https://example.com    # Linux: --interface tunN
dig @198.18.0.1 example.com                    # fake-ip DNS

# 4. Stop
sudo teamx tun0 stop
```

## 7. Risks and Mitigations

| Risk | Mitigation |
|---|---|
| root privilege requirement | Detect at startup + document clearly; `--dev` allows a custom name to avoid conflicts |
| smoltcp TCP stack limitations (no SACK/PLPMTU) | Sufficient for interactive scenarios; suggest SOCKS5 proxy when throughput matters |
| MTU/fragmentation | Set tun0 to 1280 |
| Concurrency limit (pre-allocated sockets) | Tunable via `--max-conns` (default 64) |
| UDP limited to DNS | State clearly in v1 docs: other UDP is dropped |
| fake-ip colliding with real IPs | Use 198.18.0.0/15 (RFC 5737 documentation range, not public) |
| macOS auto-assigned utunN names are unpredictable | `TunDevice::name` returns the actual name; inject routes using the actual name |
| DNS hijack reliability | Hijack only UDP port 53 destined to the fake-ip DNS server IP |

## 8. Test Plan

### 8.1 Unit Tests (Rust `#[cfg(test)]`)

| Module | Cases |
|---|---|
| tun_dev | Config generation (mac/linux branches), error handling |
| tun_dns | fake-ip allocation uniqueness, lookup restoration, DNS answer construction, mapping reuse |
| tun_socks | target composition (domain/IP), route-resolution wiring |
| routes | [Reused] already covered |

### 8.2 Integration Tests (root required; scripts + Bun TS)

**Linux (cloud host hub03) `tests/tun0-linux-test.sh`**:
1. Start serve + egress (reuse existing setup)
2. `sudo teamx tun0 start` (fake-ip range)
3. `curl --interface tunN https://example.com` → 200 (via egress)
4. `curl --interface tunN https://ifconfig.me` → egress exit IP
5. fake-ip DNS: `dig @198.18.0.1 example.com` → returns a fake IP
6. Domain routing: configure `*.com → egress2` in the route table and verify split routing
7. `sudo teamx tun0 stop` → routes removed, tun closed

**macOS (local machine) `tests/tun0-macos-test.sh`**:
1. Start serve + egress likewise
2. `sudo teamx tun0 start`
3. `curl --interface utunN https://example.com` → 200
4. Exit IP verification
5. `sudo teamx tun0 stop`

### 8.3 Test Matrix (scenario × platform)

| Scenario | Linux | macOS |
|---|---|---|
| tun0 start (root) | ✅ | ✅ |
| Non-root startup error | ✅ | ✅ |
| TCP forwarding (HTTP 200) | ✅ | ✅ |
| Correct exit IP | ✅ | ✅ |
| fake-ip DNS response | ✅ | ✅ |
| Domain-based split routing | ✅ | ✅ |
| IP/CIDR split routing | ✅ | ✅ |
| tun0 stop cleanup | ✅ | ✅ |
| Coexistence with proxy start | ✅ | ✅ |

## 9. Implementation Steps

1. Cargo dependencies: `tun = "0.8"`, `smoltcp = "0.14"` (features: proto-ipv4, socket-tcp, socket-udp, socket-dns, phy-tuntap_interface), `ipnet` (optional).
2. `tun_dev.rs`: cross-platform TUN creation/read/write (mac/linux branches).
3. `tun_stack.rs`: smoltcp Device impl + Interface + SocketSet + poll loop.
4. Extract `spawn_tunnel_bridge()` (pull the WS bridge out of run_socks5_proxy for reuse).
5. `tun_socks.rs`: TCP bridging (new connections→WS→pump) + UDP (DNS hijack/drop).
6. `tun_dns.rs`: fake-ip allocation + DNS answers.
7. `tun_cli.rs`: tun0 start/stop/status + privileges + route injection.
8. Wire up `cli.rs` + `commands.rs`.
9. Unit tests.
10. Linux integration tests (hub03) + macOS integration tests (local sudo).
11. Docs (this file + additions to 20-manual) + CHANGELOG + commit.

## 10. Effort and Milestones

| Milestone | Content | Estimate |
|---|---|---|
| M1 | tun_dev + tun_stack (able to create tun0 and get smoltcp running) | 1 day |
| M2 | TCP bridge to egress (curl works end to end) | 1 day |
| M3 | fake-ip DNS + domain routing | 1 day |
| M4 | CLI + privileges + route injection + dual-platform adaptation | 0.5 day |
| M5 | Tests (Linux + macOS) + docs | 1 day |
| **Total** | | **≈ 4.5 days** |

## 11. Implementation Record (2026-08-23)

Implemented and verified per this design; key deviations and decisions recorded:

### 11.1 smoltcp `listen(0)` Patch (Key Discovery for Transparent Proxying)

During research and implementation we found that **smoltcp 0.14's `listen()` rejects port=0** (`ListenError::Unaddressable`),
and listen sockets match **exactly** on `repr.dst_port == listen_endpoint.port` — making a transparent proxy
for "any destination port" impossible out of the box.

**Solution**: vendor smoltcp into `vendor/smoltcp`, point `[patch.crates-io]` at the local copy, and apply two patches:
- `listen()`: allow `port==0` (treated as a wildcard; no longer reports Unaddressable)
- `accepts()`: `self.listen_endpoint.port == 0 || repr.dst_port == listen_endpoint.port`
  (a listen socket with port=0 accepts any destination port)

With this, `socket.listen(0)` inside `TunStack::new` means "listen on all ports"; combined with `set_any_ip(true)`
and the fake-ip prefix route, transparent interception is achieved.

### 11.2 DNS Binding Strategy

- Initially binding `198.18.0.1:53` could fail right after the interface comes up; binding `0.0.0.0:53` collided
  with Ubuntu's systemd-resolved (127.0.0.53).
- **Final choice**: prefer binding the tun gateway IP (`198.18.0.1:53`); fall back to `0.0.0.0:53` on failure.

### 11.3 Verification Results (Linux, hub03)

| Item | Result |
|---|---|
| `tun0 start` (root) | ✅ dev=tun0 ip=198.18.0.1 mtu=1280 |
| fake-ip route | ✅ 198.18.0.0/15 → tun0 |
| fake-ip DNS | ✅ 198.18.0.1:53; example.com→198.18.0.1, www.google.com→198.18.0.2 |
| Full TCP chain | ✅ `curl -k --interface tun0 --resolve example.com:443:<fake>` → HTTP 200 |
| Domain split routing | ✅ With routes `*.com→egress2`, an ESTAB connection to example.com(20.85.130.105):443 appears on 197 |
| stop cleanup | ✅ Routes deleted, device released |

### 11.4 TODO (Future Iterations)

- Real-world sudo testing on local macOS (`tests/tun0-macos-test.sh`)
- fake-ip AAAA (IPv6) answers
- UDP forwarding (non-DNS)
- Configuration persistence (`teamx tun0 config`)
