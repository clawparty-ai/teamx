# teamx Reverse Tunnel Manual Test: member-a Reaching member-b's Local Service Through the Server

> Scenario: **member-b (developer)** runs a service locally (e.g. an HTTP service) and exposes it to the team through the `teamx` reverse tunnel; **member-a (tester)** accesses this service from another machine (or another session on the same machine) over the network — even when the two networks can't reach each other directly, traffic relays through `teamx serve`; direct connection is preferred when on the same subnet.
>
> **Two modes (chosen at expose time)**:
> - **local (default)**: server exposes no ports; member-a uses `/team tunnel forward` to map a local port for access (safer, an SSH `-L` experience).
> - **frp**: server exposes a public port (tcp://server:9100) that member-a connects to directly.
>
> Prerequisites: network mode is available (mTLS + the `/team tunnel` commands are installed), the owner has created a team and run `serve start`, and both members have joined via invitation letters and been approved.

## 0. Prerequisites

1. `./install.sh` has been run and opencode restarted (the `/team tunnel` subcommands are available).
2. A network-mode team exists (owner has run `serve start`, members have imported their letter + been approved; see `docs/16-manual-network.md`).
3. A running local service exists on member-b's machine (this example uses HTTP, but tunnels support any TCP protocol: SSH / databases / custom protocols).

## 1. Data Flow

**frp mode (`expose --mode frp`)**

```
┌─ member-b (developer) ────┐       ┌──────────────┐       ┌─ member-a (tester) ─────┐
│ Local service :8080        │       │ teamx serve  │       │ curl / browser          │
│ /team tunnel expose        │──────▶│ relay :9100+ │◀──────│ tcp://<server>:9100     │
└────────────────────────────┘  WS   └──────────────┘  TCP  └──────────────────────────┘
```

**local mode (default, `expose` without --mode)**

```
┌─ member-b (developer) ────┐       ┌──────────────┐       ┌─ member-a (tester) ─────────┐
│ Local service :8080        │       │ teamx serve  │       │ /team tunnel forward demo   │
│ /team tunnel expose        │──────▶│ bridge       │◀──────│ local listen 127.0.0.1:8080 │
└────────────────────────────┘  WS   │ (no exposure)│  WS   └───── curl http://127.0.0.1:8080/
```

- **member-b**: `/team tunnel expose` opens a persistent mTLS WebSocket to serve's `/tunnel`, registering local `:8080`.
- **serve**:
  - frp mode: allocates a public port (`9100-9999`); upon receiving member-a's TCP connection, relays bytes over that WS to member-b.
  - local mode: exposes no ports; member-a's `forward` WS (`/tunnel/forward`) bridges through the server to member-b's tunnel WS.
- **member-a**: in frp, connect to the public port; in local, use `/team tunnel forward` to map a local port — visiting `http://127.0.0.1:<local>/` reaches member-b's service.
- **Same-subnet direct connection**: `/team tunnel status <name>` returns `same_subnet`; if true, member-a may access `direct_addr` (member-b's `lan_ip:target_port`) directly.

## 2. Manual Test Steps

### 2.1 member-b —— Prepare a Local Service

First start a simple HTTP service locally (Python used in this example):

```bash
cd /tmp && mkdir -p svc && cd svc
echo "hello from member-b's service" > index.html
python3 -m http.server 8080 --bind 127.0.0.1
# Verify: curl http://127.0.0.1:8080/index.html → hello from member-b's service
```

### 2.2 member-b —— Expose the Service

In member-b's opencode window (**local mode, default**):

```
/team tunnel expose --name demo --port 8080
```

Expected: returns `mode: local` (server exposes no port), hints teammates to use `forward`, and the tunnel is persisted.

**frp mode (optional)**:

```
/team tunnel expose --name demo --port 8080 --mode frp
```

Expected: returns `public_port` (e.g. 9100); the server exposes `tcp://<server>:9100`.

> Manual equivalent (CLI):
> ```bash
> TEAMX_SERVER_URL=https://<server>:5781 \
> TEAMX_MTLS_CERT=~/.teamx/letters/<id>/client.crt \
> TEAMX_MTLS_KEY=~/.teamx/letters/<id>/client.key \
> TEAMX_MTLS_CA=~/.teamx/letters/<id>/ca.crt \
> teamx tunnel expose demo --port 8080            # local (default)
> teamx tunnel expose demo --port 8080 --mode frp # frp
> ```

### 2.3 Any Member (member-a) —— View/Access

In member-a's opencode window:

```
/team tunnel list
```

Expected: lists `demo`, with its mode (local/frp), public port (frp only), and the provider's LAN IP.

**local mode —— local forwarding**:

```
/team tunnel forward --name demo
```

Expected: listens locally at `127.0.0.1:8080` (default = provider target port; on conflict it returns random candidate ports needing confirmation), hinting "access as if it were a local service".

```bash
curl http://127.0.0.1:8080/index.html
# → hello from member-b's service
```

**frp mode —— connect to the server port directly**:

```bash
curl http://<server>:9100/index.html
# → hello from member-b's service
```

```
/team tunnel status demo
```

Expected:
```json
{
  "name": "demo",
  "port": 9100,
  "target_port": 8080,
  "lan_ip": "192.168.1.5",
  "same_subnet": true|false,
  "direct_addr": "192.168.1.5:8080",
  "relay_addr": "tcp://<server>:9100"
}
```

**Access via relay (always works, even without direct network reachability)**:

```bash
curl http://<server>:9100/index.html
# → hello from member-b's service
```

**Same-subnet direct connection (when same_subnet=true)**:

```bash
curl http://<direct_addr>/index.html     # e.g. http://192.168.1.5:8080/index.html
```

Or let the plugin pick the best address for you:

```
/team tunnel direct demo
# → same_subnet=true → direct_addr (direct); otherwise relay_addr (relay)
```

### 2.4 Close the Tunnel

member-b (or any member):

```
/team tunnel close demo
```

Expected: returns `closed: true`; the public port is released (`curl` to it fails immediately); the persisted record is removed (no auto-rebuild after restart).

## 3. Acceptance Checklist

- [ ] After member-b's `expose`, member-a can reach member-b's local service via `http://<server>:9100/...`
- [ ] `tunnel list` shows the tunnel (name/public port/LAN IP)
- [ ] `tunnel status` returns all three fields: `same_subnet` / `direct_addr` / `relay_addr`
- [ ] On the same subnet, `tunnel direct` returns `direct_addr` and direct access succeeds
- [ ] After closing, the public port is unreachable and the persisted record is cleared
- [ ] After restarting member-b's opencode, persisted tunnels are automatically rebuilt (`serve` log shows `restored reverse tunnel`)

## 4. Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `expose` errors "requires network mode" | `TEAMX_SERVER_URL` not set | set `TEAMX_SERVER_URL=<server url>` and retry |
| `expose` errors "tunnel `x` already exists" | a same-named tunnel exists (or wasn't closed last time) | `tunnel close x` first or use a different name |
| Public port gives `connection refused` | tunnel not registered/closed | confirm with `tunnel list`; retry `expose` |
| Access times out | serve not started/port blocked | check `serve status`; cross-machine, verify firewall allows the port |
| Direct connection fails but `same_subnet=true` | member-b's local firewall blocks the target port | allow member-b's target port; or use the relay address instead |
| Tunnel not rebuilt after restart | persistence file cleared (was closed) or server_url mismatch | re-run `expose`; ensure `TEAMX_SERVER_URL` matches what was used at expose time |

## 5. Notes

- Tunnels are at the **TCP level** (frp-style) and support any protocol (HTTP/HTTPS/SSH/databases/custom) — not limited to HTTP.
- One WS connection per service; if the provider disconnects (network jitter), it reconnects and re-registers automatically.
- Public port range `9100-9999` (allocated when `teamx serve` starts).
- Security: consistent with other team features — mTLS identity + same-team authorization; `tunnel list/status/close` are available only to members of the team.
- UDP tunnels are a future plan (currently TCP only).
