# Proxy (SOCKS5 Outbound Proxy) Test Cases

Numbering scheme: `PR-<module>-<index>`. PR = SOCKS5 protocol, PE = exit egress, PC = consumer proxy, PI = integration.

## Unit Tests (cargo test)

### PR-001 SOCKS5 handshake: NO AUTH negotiation
- Input: `05 01 00` (VER=5, 1 method, no authentication)
- Expected: returns `0x00` (NO AUTH)

### PR-002 SOCKS5 handshake: multiple methods, NO AUTH selected
- Input: `05 03 00 01 02`
- Expected: returns `0x00`

### PR-003 SOCKS5 handshake: client demands an authentication method
- Input: `05 01 02`
- Expected: error (v1 does not support authentication)

### PR-004 SOCKS5 CONNECT: IPv4 target
- Input: `05 01 00 01 7F 00 00 01 1F 90` (CONNECT, IPv4 127.0.0.1:8080)
- Expected: `host=127.0.0.1, port=8080`, consumed=10

### PR-005 SOCKS5 CONNECT: domain target
- Input: `05 01 00 03 09 65 78 61 6d 70 6c 65 2e 63 6f 6d 00 50` (example.com:80)
- Expected: `host=example.com, port=80`, consumed=6+11+2=19

### PR-006 SOCKS5 CONNECT: IPv6 target
- Input: `05 01 00 04 <16B addr> 00 50`
- Expected: host is an IPv6 string, port=80

### PR-007 SOCKS5 CONNECT: non-CONNECT command rejected
- Input: `05 02 00 01 7F 00 00 01 00 50` (BIND)
- Expected: error

### PR-008 SOCKS5 CONNECT: truncated input
- Input: `05 01 00 01 7F` (insufficient)
- Expected: error (or a needs-more-bytes response)

### PE-001 TunnelMode::Proxy parsing
- `TunnelMode::parse("proxy")` == Proxy; `as_str()` == "proxy"

### PE-002 Proxy-mode registration allows port=0
- `register(team, member, "egress", 0, None, tx, Proxy)` succeeds, port returned as 0

### PE-003 Proxy mode binds no server port
- After Local/Proxy registration `port == 0`; list reports mode=proxy

### PE-004 open_stream passes the target through
- `open_stream(team, name, tx, Some("example.com:80"))` → provider receives
  `{"type":"open_stream","stream_id":N,"target":"example.com:80"}`

### PE-005 open_stream compatible without a target
- `open_stream(team, name, tx, None)` → provider receives open_stream (no target field)

### PE-006 Duplicate registration of a same-named proxy exit rejected
- Same team, same name registered twice → second call Err

## Integration Tests (bun tests/proxy-test.ts)

### PI-001 exit registration (mode=proxy)
- Member B connects to `/tunnel` and registers `{"name":"egress","port":0,"mode":"proxy"}`
- Expected: receives `registered`, port=0, mode=proxy

### PI-002 SOCKS5 end-to-end: curl reaching member-b's egress service through the proxy
- member-a starts `teamx proxy start --port 1080 --exit egress`
- member-b runs a local HTTP service on `127.0.0.1:19099` (returns a fixed body)
- `curl --socks5-hostname 127.0.0.1:1080 http://127.0.0.1:19099/`
- Expected: the member-b service body comes back (bytes round-trip a→server→b identically)

### PI-003 Multiple concurrent SOCKS5 connections
- Fire 3 curls simultaneously (different streams)
- Expected: all 3 succeed, stream_ids do not collide

### PI-004 tunnel.list reports the proxy exit
- `tunnel.list` returns the mode=proxy egress

### PI-005 Non-team-members cannot use the exit
- Connecting to `/tunnel/forward` requesting egress without certificate/membership → rejected

### PI-006 Disconnect cleanup
- The exit WS disconnects → tunnel.list no longer contains egress

## Manual Acceptance (optional)

### PI-H1 Firefox/Chrome SOCKS5 configuration
- Browser configured with SOCKS5 127.0.0.1:1080 → can reach targets reachable from member-b's side

### PI-H2 Domain resolution happens on the member-b side
- curl --socks5-hostname against a domain only member-b can resolve → success
