# Proxy (SOCKS5 Outbound Proxy) Design Document

## 1. Background & Goals

The tunnel solves "inbound exposure": member-b opens a local port, and member-a maps it locally through the tunnel for access; the target is a **fixed port**.

The proxy solves the "outbound proxy": member-a starts a local **SOCKS5 proxy port**; applications on member-a (curl, firefox, etc.) configure that proxy, and traffic flows `member-a --ws--> team server --ws--> member-b`, where **member-b dials dynamically** to the target address specified in the SOCKS5 request — achieving "borrow member-b's network egress to reach any target".

Core differences:

| Dimension | tunnel (local forward) | proxy |
|------|------------------------|-------|
| Entry | member-a local TCP port (byte pass-through) | member-a local **SOCKS5** port (parses target address) |
| Target | Fixed (target_port specified at provider registration) | **Dynamic** (host:port from the SOCKS5 CONNECT request) |
| Egress | member-b dials the fixed target_port | member-b dials the target address from the SOCKS5 request |
| Use case | Access member-b's local services | Borrow member-b's network egress to reach any target |

## 2. Overall Architecture

```
+--------+   SOCKS5   +----------+   mTLS WS    +-----------+   mTLS WS    +----------+
|  curl/ | --1080-->  | member-a | --connect--> | teamx     | --open_    | member-b |
| firefox|            | (proxy   |  target=H:P | server    |   stream   | (exit)   |
|        |            |  start)  | <----------- | (relay)   | <----------|          |
+--------+            +----------+   [4B sid]   +-----------+   [4B sid]  +----------+
                                                                |
                                                                | dial H:P
                                                                v
                                                         +---------------+
                                                         | Target service H:P |
                                                         +---------------+
```

The data plane fully reuses tunnel's stream relay mechanism (`[4-byte BE stream_id][payload]` binary frames + per-WS single-stream bidirectional bridging); the **control plane adds target-address passing**:

- consumer (member-a) → server: `{"type":"connect","name":"<exit>","target":"host:port"}`
- server → provider (member-b): `{"type":"open_stream","stream_id":N,"target":"host:port"}`

When the provider receives an open_stream carrying `target`, it **dials the target** (rather than the fixed port registered at signup).

## 3. Protocol Design

### 3.1 Extending TunnelMode

Add a variant to `tunnel.rs`'s `TunnelMode`:

```rust
pub enum TunnelMode {
    Local,   // default: server binds no port; consumer accesses via forward
    Frp,     // server binds a public port
    Proxy,   // new: outbound proxy egress; provider dials SOCKS5 targets dynamically
}
```

- `--mode proxy` (or the `proxy` subcommand) registers a proxy egress.
- Proxy mode: `target_port = 0` (no fixed target), no server port bound (same as Local).

### 3.2 Registration (provider → server)

```json
{"type":"register","name":"egress","port":0,"mode":"proxy"}
```

- Server validation: proxy mode allows `port == 0` (Local/Frp still require port != 0).
- Ack is unchanged from today: `{"type":"registered","name":"egress","port":0,"mode":"proxy"}`.

### 3.3 Connect (consumer → server)

```json
{"type":"connect","name":"egress","target":"example.com:80"}
```

- `target` is the `host:port` parsed from the SOCKS5 CONNECT request.
- Compatibility: without `target`, behave as today (dial the fixed port registered).

### 3.4 Open Stream (server → provider)

```json
{"type":"open_stream","stream_id":12,"target":"example.com:80"}
```

- With `target`: provider dials that address.
- Without `target`: provider dials the registered fixed port (compatible with tunnel forward).

### 3.5 Data Plane

Unchanged: `[4B BE stream_id][payload]` bidirectional binary frames; server routes by stream_id.

## 4. Member-Side Implementation

### 4.1 member-b: Proxy Egress (provider / exit)

CLI: `teamx proxy exit --name egress`

Reuses `tunnel_client::run_expose`'s WS loop with modified open_stream handling:

```text
received open_stream (with target)    → TcpStream::connect(target)
                     (without target) → TcpStream::connect(127.0.0.1:fixed port)
```

### 4.2 member-a: Local SOCKS5 Proxy (consumer)

CLI: `teamx proxy start --port 1080 --exit egress`

New module `socks5.rs`, responsibilities:

1. Listen on `127.0.0.1:PORT`, accepting application connections.
2. SOCKS5 handshake (NO AUTH):
   - Read `VER NMETHODS METHODS...` (VER=0x05)
   - Reply `05 00` (choose no-auth)
3. Parse CONNECT request: `VER CMD RSV ATYP ADDR PORT`
   - `ATYP=0x01` IPv4 (4 bytes)
   - `ATYP=0x03` domain name (1-byte length + name)
   - `ATYP=0x04` IPv6 (16 bytes)
   - `CMD=0x01` CONNECT; other CMDs reply not-supported
4. Connect to `wss://server/tunnel/forward`, send `{"type":"connect","name":"egress","target":"host:port"}`
5. After receiving `stream_open`, reply SOCKS5 success `05 00 00 01 00 00 00 00 00 00`
6. Then bridge bytes as in tunnel forward (consumer-side logic fully reused)

### 4.3 SOCKS5 Protocol Parsing (pure functions, unit-testable)

`socks5.rs` provides side-effect-free parsing functions:

```rust
/// Parse SOCKS5 greeting: first 2 bytes + methods, return selected auth method
pub fn parse_greeting(buf: &[u8]) -> Result<u8, String>;   // returns 0x00 = NO AUTH

/// Parse CONNECT request, return (atyp, host, port)
pub struct SocksTarget { pub host: String, pub port: u16 }
pub fn parse_connect_request(buf: &[u8]) -> Result<(usize, SocksTarget), String>;
//                                        ^ consumed byte count
```

## 5. Server Changes (serve.rs / tunnel.rs)

| Location | Change |
|------|------|
| `tunnel.rs` `TunnelMode` | Add `Proxy` variant; `parse` accepts `"proxy"`; `as_str` returns `"proxy"` |
| `tunnel.rs` `register` | Proxy mode allows `target_port == 0` (skip non-zero check) |
| `tunnel.rs` `open_stream` | Add optional `target: Option<String>` parameter, passed through into the provider's open_stream message |
| `serve.rs` `handle_tunnel_ws` | register branch: Proxy mode doesn't require a port; pass target through |
| `serve.rs` `handle_tunnel_forward` | connect branch: parse optional `target` field, hand to `open_stream` |

## 6. CLI Design

```
teamx proxy exit    --name egress [--server URL]     # member-b: outbound proxy egress (long-running)
teamx proxy start   --port 1080 --exit egress [--server URL]  # member-a: local SOCKS5 (long-running)
```

- `exit`: provider side, long-running WS loop (reusing tunnel_client::run_expose, mode=proxy).
- `start`: consumer side, long-running SOCKS5 listener (new run_socks5_proxy).
- Without `--server`, resolve by existing rules (flag > env > letter > localhost).

## 7. Security Boundary

- Reuses existing mTLS: members must hold a valid certificate for this team to register/connect.
- A proxy exit is usable only by members of **the same team** (server validates by team_id, same as tunnel).
- SOCKS5 listens only on `127.0.0.1`, never exposed to the LAN.
- CONNECT only (HTTP/HTTPS/any TCP); UDP ASSOCIATE unsupported (out of scope for v1).
- Target addresses are resolved and dialed by member-b; member-a can request any host:port (same trust model as tunnel: team members are open to each other).

## 8. Milestones

| Step | Content | Verification |
|------|------|------|
| P1 | socks5.rs: SOCKS5 handshake + CONNECT parsing (pure functions) | Unit tests |
| P2 | tunnel.rs: TunnelMode::Proxy + relaxed register + open_stream target passthrough | Unit tests |
| P3 | serve.rs: register/connect support proxy + target | Build + integration tests |
| P4 | tunnel_client.rs: run_expose supports target dialing + run_socks5_proxy | Integration tests |
| P5 | cli.rs + commands.rs + main.rs: proxy exit/start commands | End-to-end curl --socks5 |

## 9. Non-Goals (out of scope for v1)

- UDP ASSOCIATE (SOCKS5 UDP relay)
- Authentication (username/password) — NO AUTH only
- Localized DNS resolution (on member-a side) — target domains resolved on member-b's side
- Access control/whitelisting for proxy exits
