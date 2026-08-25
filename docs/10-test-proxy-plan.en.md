# Proxy (SOCKS5 Outbound Proxy) Test Plan

## 1. Scope

Covers the `teamx proxy` features:

- `teamx proxy exit` (member-b's outbound proxy exit, provider side)
- `teamx proxy start` (member-a's local SOCKS5 proxy, consumer side)
- SOCKS5 protocol parsing (handshake + CONNECT request)
- End-to-end data plane (curl reaching member-b's network egress through the SOCKS5 proxy)

## 2. Test Layers

| Layer | Tooling | Content |
|----|------|------|
| Unit tests | `cargo test` | socks5.rs parsing, tunnel.rs Proxy mode, open_stream target passthrough |
| Integration tests | `bun tests/proxy-test.ts` | server + exit member + proxy member + end-to-end curl --socks5 |
| Full regression | `./tests/run-all.sh` | adds the new proxy steps; confirms no regression in tunnel/ws/mtls etc. |

## 3. Test Environment

- Single-machine closed loop: `TEAMX_SERVER_URL=https://127.0.0.1:PORT` (serve ships with mTLS).
- Two members: owner (proxy consumer) + member B (exit provider).
- Target service: member-b starts a local HTTP service (e.g. `127.0.0.1:19099`)
  simulating "a service reachable from member-b's network egress"; member-a reaches it through SOCKS5.
- Use `curl --socks5-hostname 127.0.0.1:1080 http://127.0.0.1:19099/`.
  - Domain resolution happens on the member-b side (SOCKS5 forwards the target address verbatim to the exit).

## 4. Isolation and Cleanup

- Every test uses an isolated temporary `TEAMX_HOME` / `TEAMX_DB` / ports (server, socks5,
  target service, exit).
- `trap cleanup` removes temporary directories and processes.

## 5. Key Verification Points (mapped against the test cases)

- SOCKS5 handshake (NO AUTH) replies `05 00`.
- CONNECT requests: IPv4 / domain / IPv6 ATYP all parsed correctly; non-CONNECT commands rejected.
- exit registers successfully with mode=proxy, port=0, binding no server port.
- Consumer connects carrying a target → provider receives open_stream with the target → dials the target.
- Data-plane bytes identical in both directions (member-a's application receives the response from member-b's egress service).
- `tunnel.list` shows exits with mode=proxy.
- Connections rejected when member B is not on the team (mTLS authorization boundary).

## 6. Execution Order

1. `cargo test` (P1/P2 unit)
2. `cargo clippy --all-targets -- -D warnings`
3. `bun tests/proxy-test.ts` (P3-P5 integration)
4. `./tests/run-all.sh` (full regression)
