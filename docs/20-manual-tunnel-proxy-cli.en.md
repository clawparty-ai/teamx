# teamx Tunnel and Proxy Manual Testing Guide (pure CLI, no opencode/dsh dependency)

> This document walks you through manually verifying the three tunnel modes of the
> networking features (frp / local / proxy) and the SOCKS5 outbound proxy's full lifecycle **using only the `teamx` command line**.
>
> Each scenario maps one-to-one to the automated comprehensive test `tests/tunnel-proxy-comprehensive.ts` (43 assertions); once the automated test passes, this document lets you reproduce the same behavior step by step in a real terminal.
>
> For the plugin edition (inside opencode, `/team tunnel …`) see the manual `docs/17-manual-tunnel.md`; for a network-mode primer on three-person collaboration see `docs/16-manual-network.md`.

## 0. Prerequisites

1. Binaries built:

   ```bash
   cargo build          # artifact: target/debug/teamx (abbreviated as $TEAMX below)
   export TEAMX="$PWD/target/debug/teamx"
   ```

2. Tools: `curl` (HTTP verification + SOCKS5 verification), `python3` (to run sample services). Tunnels support any TCP protocol; HTTP services are used uniformly here for easy observation.

3. The OS allows binding ports on 127.0.0.1 (examples use 8080 / 1080 / 9100-9999 / 18080).

## 1. The Three Modes in One Picture

```
frp (server exposes public port)      local (default, zero server exposure)     proxy (SOCKS5 outbound proxy)

member-b :8080                        member-b :8080                            target service (any host:port)
   │ expose --mode frp                   │ expose (defaults to local)               ▲ member-b dials the dynamic target
   ▼ WS(mTLS)                            ▼ WS(mTLS)                                │ WS(mTLS)
┌──────────────┐                      ┌──────────────┐                         ┌──────────────┐
│ teamx serve  │                      │ teamx serve  │                         │ teamx serve  │
│ public port 9100+│                  │ bridges only, no open ports│                  │ bridges only, no open ports│
└──────▲───────┘                      └──────▲───────┘                         └──────▲───────┘
       │ direct TCP connection                │ forward local mapping                    │ local SOCKS5 :1080
   member-a curl                       member-a :18080                          member-a curl --socks5-hostname
```

| Mode | Server-side port after registration | How consumers access | Typical use |
|---|---|---|---|
| `frp` | Allocates 9100-9999 | Direct `tcp://<server>:<port>` | Give the whole team a directly reachable entry point temporarily |
| `local` (default) | **No port used** (registration returns port=0) | `tunnel forward` maps to a local port | SSH `-L` experience, safest |
| `proxy` | No port used (port=0), no fixed target | Local SOCKS5 port, target specified dynamically by CONNECT | Let teammates' egress IP become yours |

## 2. Environment Setup: Simulating "Server + Member A + Member B" on One Machine

Use three terminals + three `TEAMX_HOME`s to simulate the three parties. Member B obtains mTLS material via an invitation letter, after which all tunnel commands **auto-discover** the server address and certificates (from `$TEAMX_HOME/letters/<id>/`); nothing else needs passing.

### 2.1 Directory Layout

```bash
LAB=/tmp/teamx-lab && rm -rf "$LAB" && mkdir -p "$LAB/server" "$LAB/a" "$LAB/b"
```

### 2.2 Terminal ① — Server + owner

```bash
export LAB=/tmp/teamx-lab
export TEAMX=/path/to/target/debug/teamx
export TEAMX_HOME=$LAB/server TEAMX_DB=$LAB/server/teamx.db

$TEAMX init                                   # generates the instance CA ($TEAMX_HOME/ca/)
$TEAMX serve --addr 127.0.0.1 --port 5781     # runs in foreground, leave it alone
```

### 2.3 Terminal ② — Owner ops commands (create team / invite / approve)

```bash
export LAB=/tmp/teamx-lab
export TEAMX=/path/to/target/debug/teamx
export TEAMX_HOME=$LAB/server TEAMX_DB=$LAB/server/teamx.db

# Create the team, get the owner member id, issue the client certificate
$TEAMX team create Lab --session s:a --json | tee create.json
OWNER_ID=$(python3 -c 'import json;print(json.load(open("create.json"))["owner_member_id"])')
$TEAMX cert issue "$OWNER_ID" owner --out "$LAB/a"

# The owner also needs mTLS material to call tunnel/proxy subcommands (they go over the network, not the local DB)
export TEAMX_MTLS_CERT=$LAB/a/member.crt \
       TEAMX_MTLS_KEY=$LAB/a/member.key \
       TEAMX_MTLS_CA=$LAB/server/ca/ca.crt

# Send an invitation letter to member B
$TEAMX team invite "contributor: provides local services" --session s:a --json | tee invite.json
MEMBER_B=$(python3 -c 'import json;print(json.load(open("invite.json"))["member_id"])')
```

### 2.4 Terminal ③ — Member B imports the invitation letter

```bash
export LAB=/tmp/teamx-lab
export TEAMX=/path/to/target/debug/teamx
export TEAMX_HOME=$LAB/b

cp "$LAB/server/invite.json" .   # or obtain the letter string from a messaging channel into letter.json
LETTER=$(python3 -c 'import json;print(json.load(open("invite.json"))["letter"])')

# Key step: import is a "write-to-database" operation; in this single-machine drill we temporarily borrow the server's DB to claim the seat;
# the mTLS material is written to $TEAMX_HOME/letters/<invitation-id>/ (in B's own HOME).
export TEAMX_DB=$LAB/server/teamx.db
$TEAMX team import "$LETTER" --name DevB --session s:b
unset TEAMX_DB

# From then on none of B's network commands need any configuration beyond environment variables:
# server URL ← auto-read from the https://127.0.0.1:5781 embedded in the letter
# client certificate ← auto-read from letters/<id>/client.crt|key, ca.crt
```

### 2.5 Back to Terminal ②: Approve member B

```bash
$TEAMX team approve "$MEMBER_B" --session s:a
$TEAMX team status --session s:a --json | python3 -m json.tool   # members should contain owner + DevB(active)
```

> **With two real machines**: add `--server-url https://<LAN_IP>:5781` to invite; change serve to `--addr 0.0.0.0 --san <LAN_IP>`; members cannot share the DB, so use RPC-based import with client certificates:
> ```bash
> LETTER='teamx-inv:v1:…'
> python3 - "$LETTER" <<'PY'      # extract the certificate trio into ./mtls/
> import sys,json,base64,pathlib
> d=json.loads(base64.b64decode(sys.argv[1][len('teamx-inv:v1:'):]))
> c=d['certificates']; p=pathlib.Path('mtls'); p.mkdir(exist_ok=True)
> (p/'client.crt').write_text(c['client_cert']); (p/'client.key').write_text(c['client_key'])
> (p/'ca.crt').write_text(c['ca_cert'])
> PY
> curl --cacert mtls/ca.crt --cert mtls/client.crt --key mtls/client.key \
>      -H 'Content-Type: application/json' \
>      -d "{\"method\":\"team.import\",\"args\":{\"letter\":\"$LETTER\",\"name\":\"DevB\"}}" \
>      https://<LAN_IP>:5781/rpc
> # afterwards export TEAMX_MTLS_CERT/KEY/CA for that terminal pointing at ./mtls/
> ```

## 3. Test One: FRP Tunnel (public-port relay)

**Corresponding automated assertions: Section 2 (9 items)**

### 3.1 Terminal ③ (B): start a local service and expose it

```bash
mkdir -p /tmp/www && echo "hello from member-b" > /tmp/www/index.html
python3 -m http.server 8080 --directory /tmp/www &

$TEAMX tunnel expose web --port 8080 --mode frp --session cli
# expected output:
# ok tunnel registered: name=web mode=frp port=9100
```

Keep the process in the foreground (this is the provider connection; Ctrl-C triggers disconnect cleanup, see §7).

### 3.2 Terminal ② (A): access via the public port + query status

```bash
curl http://127.0.0.1:9100/
# → hello from member-b          (bytes reach B's 8080 through the serve relay)

$TEAMX tunnel list --session cli
# → the tunnels array contains {"name":"web","mode":"frp","port":9100,"target_port":8080,...}

$TEAMX tunnel status web --session cli
# In the single-machine drill A and B are both on loopback:
#   same_subnet=true, direct_addr="127.0.0.1:8080"   ← same /24 hinting direct connection
# Across subnets same_subnet=false and only the relay address is usable

$TEAMX tunnel close web --session cli
curl --max-time 2 http://127.0.0.1:9100/ || echo "✓ port released, connection refused"
```

## 4. Test Two: Local Tunnel + forward (zero server exposure)

**Corresponding automated assertions: Section 3 (5 items)**

### 4.1 Terminal ③ (B): expose with default local mode

```bash
$TEAMX tunnel expose web2 --port 8080 --session cli
# expected output:
# ok tunnel registered: name=web2 mode=local port=0     ← 0 = server opened no port at all
```

### 4.2 Terminal ② (A): confirm zero exposure, then map locally and access

```bash
$TEAMX tunnel status web2 --session cli | grep -E '"mode"|"port"'
# → "mode":"local", "port":0
ss -ltn | awk '$4 ~ /:91[0-9][0-9]$/'   # no 9100+ listeners (Linux); on macOS use lsof -iTCP -sTCP:LISTEN

$TEAMX tunnel forward web2 --local-port 18080 --session cli
# expected output:
# ok forward: name=web2 listening on 127.0.0.1:18080 (access like a local service)

curl http://127.0.0.1:18080/
# → hello from member-b      (A:18080 →WS→ serve →WS→ B:8080)
```

After verifying, Ctrl-C to end forward. In local mode the server only relays encrypted WS traffic the whole time and exposes no TCP port.

## 5. Test Three: SOCKS5 Outbound Proxy (proxy exit / proxy start)

**Corresponding automated assertions: Section 4 (4 items)**

Roles reversed: this time **B is the egress**, and A routes its own traffic out through B.

### 5.1 Terminal ③ (B): start the proxy exit

```bash
$TEAMX proxy exit egress
# keep the process running. Registers a special mode=proxy, port=0 tunnel; each stream's target is specified dynamically by the consumer
```

### 5.2 Terminal ② (A): start the local SOCKS5 port

```bash
$TEAMX proxy start --port 1080 --exit egress
# expected output:
# ok proxy: exit=egress SOCKS5 listening on 127.0.0.1:1080
#   (set curl --socks5-hostname or browser proxy)
```

### 5.3 Verify: A reaches services on B's network via B's exit

```bash
# Target service on the same machine (B's 8080, still running)
curl --socks5-hostname 127.0.0.1:1080 http://127.0.0.1:8080/
# → hello from member-b

# Domain-type targets also work (CONNECT ATYP=domain dialed by B's side)
curl --socks5-hostname 127.0.0.1:1080 https://example.com -o /dev/null -w '%{http_code}\n'

$TEAMX tunnel list --session cli | grep egress
# → {"name":"egress","mode":"proxy","port":0,...}
```

### 5.4 Take the exit down → registry auto-cleanup

```bash
# In terminal ③ Ctrl-C the proxy exit, wait about 1 second:
$TEAMX tunnel list --session cli   # egress is gone
curl --max-time 2 --socks5-hostname 127.0.0.1:1080 http://127.0.0.1:8080/ \
  || echo "✓ proxy unavailable after the exit goes offline"
```

### 5.5 Multi-exit routing (split by target domain/IP)

The same team can have multiple `proxy exit`s (unique names), and a single local **SOCKS5 port** can pick the exit automatically per target.

**Configure the route table (SQLite, default behavior)**:

```bash
$TEAMX proxy routes set-default egress           # default exit
$TEAMX proxy routes add '*.cn' egress2           # .cn domains go via egress2
$TEAMX proxy routes add '10.0.0.0/8' egress2     # internal IP ranges go via egress2
$TEAMX proxy routes add '192.168.1.5' egress     # specific IP goes via egress
$TEAMX proxy routes list                          # view them
```

Start (**no --exit / -f needed**, read from SQLite):

```bash
$TEAMX proxy start --port 1080
```

**Ad-hoc JSON file (-f, does not write the DB)**:

```json
{ "default": "egress", "rules": [ { "match": "*.cn", "exit": "egress2" } ] }
```

```bash
$TEAMX proxy start --port 1080 -f routes.json
```

**Verify the splitting**:

```bash
curl --socks5-hostname 127.0.0.1:1080 https://www.baidu.cn -o /dev/null -w '%{http_code}\n'  # → egress2
curl --socks5-hostname 127.0.0.1:1080 https://example.com  -o /dev/null -w '%{http_code}\n'  # → egress
# Compare egress IPs:
curl --socks5-hostname 127.0.0.1:1080 https://ifconfig.me   # hits a rule → egress2's IP
```

**Rule matching** (first-match, `routes.rs`):
| Form | Example | Notes |
|------|------|------|
| Exact domain | `example.com` | Does not match `api.example.com` |
| Wildcard suffix | `*.cn` | Matches `www.baidu.cn`, not `cn.com` |
| CIDR | `10.0.0.0/8`, `2001:db8::/32` | Matched by range when the target is an IP |
| Exact IP | `192.168.1.5` | Shorthand for CIDR /32 |

Management commands: `proxy routes list / add / remove / set-default / import <file> / clear`.

## 6. Test Four: Multiple Tunnels Coexisting and Selective Close

**Corresponding automated assertions: Section 5 (8 items)**

```bash
# Terminal ③ (B): start another HTTP service and expose two frp tunnels
echo svc-a > /tmp/www/a.html && echo svc-b > /tmp/www/b.html
python3 -m http.server 8081 --directory /tmp/www &
$TEAMX tunnel expose svc-a --port 8080 --mode frp --session cli   # → port=9100
$TEAMX tunnel expose svc-b --port 8081 --mode frp --session cli   # → port=9101 (another independent WS)

# Terminal ② (A): both reachable, independent of each other
curl http://127.0.0.1:9100/a.html   # → svc-a
curl http://127.0.0.1:9101/b.html   # → svc-b
$TEAMX tunnel list --session cli      # lists svc-a / svc-b simultaneously

# Close only svc-a:
$TEAMX tunnel close svc-a --session cli
curl --max-time 2 http://127.0.0.1:9100/ || echo "✓ svc-a closed"
curl http://127.0.0.1:9101/b.html    # → svc-b still fine
$TEAMX tunnel list --session cli      # only svc-b remains
```

Duplicate-name protection: have B run `$TEAMX tunnel expose svc-b --port 8081 --mode frp --session cli` again; it should fail with
`tunnel 'svc-b' already exists in this team`, and the original tunnel is unaffected.

## 7. Test Five: Provider Disconnect Auto-Cleanup + Port Pool Recycling

**Corresponding automated assertions: Section 7-8 (13 items)**

```bash
# Terminal ③ (B): open yet another frp tunnel
$TEAMX tunnel expose ghost --port 8080 --mode frp --session cli    # note the assigned port, e.g. 9102
```

```bash
# Terminal ② (A): confirm reachable, then have B Ctrl-C
$TEAMX tunnel list --session cli | grep ghost                      # present
sleep 1 && $TEAMX tunnel list --session cli                       # ghost gone (WS disconnect cleans up immediately)
curl --max-time 2 http://127.0.0.1:9102/ || echo "✓ public port closed along with the disconnect"

# Port pool: exposing multiple frp tunnels consecursively yields monotonically increasing assignments all within 9100-9999;
# after closing one and exposing again, the just-freed number can be reused (the automated test observed 9101 being reused).
```

Boundary behavior quick reference:

| Operation | Expected result |
|---|---|
| `expose` duplicate name | Error `already exists in this team`; original tunnel unaffected |
| `close` a nonexistent name | Returns `{"closed":false,"freed_port":null}` (idempotent, not an error) |
| `status` a nonexistent name | Error (ok=false) |
| `list` with an empty team | Empty `tunnels` array |
| provider WS disconnect | Removed from the registry within ≤1 second, port released, relay closed |

## 8. Acceptance Checklist

- [ ] serve healthy: owner with mTLS `curl --cacert $TEAMX_HOME/ca/ca.crt --cert … --key … https://127.0.0.1:5781/health` → `"ok":true`
- [ ] frp: `curl http://<server>:<port>/` returns B's service content; after `close`, the same port refuses connections
- [ ] local: `status` shows `port=0`; after A's `forward`, `curl 127.0.0.1:<local port>` works; server has no new listeners
- [ ] proxy: on A, `curl --socks5-hostname` fetches content through B's exit; concurrent connections don't cross streams
- [ ] multi-tunnel: mutually unaffected; selective close correct
- [ ] disconnect: within ≤1s of provider Ctrl-C, registry cleaned up and port recycled
- [ ] duplicate-name / nonexistent-name close/status behavior matches the §7 table

## 9. Troubleshooting

| Symptom | Cause | Remedy |
|---|---|---|
| `no mTLS material: import an invitation letter or set TEAMX_MTLS_CERT/KEY/CA` | No certificate material under that HOME | Complete the §2.4 import; or explicitly export `TEAMX_MTLS_CERT/KEY/CA` |
| `connect …: invalid peer certificate` / TLS handshake failure | Server certificate SAN doesn't include the connected hostname | When accessing across machines, run serve with `--san <LAN_IP>` and connect using the same address embedded in the letter |
| expose reports `already exists in this team` | Same-name tunnel online | `close` first or rename |
| frp port unreachable but present in list | Provider process dead or blocked by firewall | Confirm the expose process is alive; check the server host firewall allows 9100-9999 |
| forward hangs without response | Provider-side bridge missing / wrong local target port | Confirm B's expose process is running and `--port` points at a real listening port |
| SOCKS5 returns `general SOCKS server failure`(05 05) | Exit offline or target unreachable | Use `tunnel list` to confirm egress exists; on B, verify reachability with a manual `curl <target>` |
| SOCKS5 returns `Connection refused`(05 05) / `Can't complete SOCKS5 connection`(97) | **Idle tunnel WS silently cut off by NAT/middlebox**, tunnel already gone from the registry, or provider's WS half-open | Make sure proxy exit / proxy start are upgraded to the version with **heartbeat + auto-reconnect** (`>= 0.2.1`, see §11); the two processes will automatically reconnect and re-register, **no manual restart needed**; if still on an old version, restarting both ends restores it temporarily |
| `tunnel port pool exhausted (9000-9999)` | 900 frp tunnels filled the pool | Clean up unused tunnels |
| Member reports `not a member of team …` | Wrong team identifier / not approved | RPC determines identity by **certificate CN**; confirm the member was approved |
| `proxy start` reports "no exit configured" | No SQLite route configured and no `--exit` / `-f` given | `proxy routes set-default <exit>`, or pass `--exit <name>`, or use `-f <routes.json>` |
| Routes don't split as expected (a target took the default exit) | Rule order / match form wrong | Check with `proxy routes list`; `*.cn` doesn't match `cn.com`; domain rules don't match IP targets (use CIDR) |
| `proxy routes` subcommand reports Unknown argument | Binary too old | Rebuild with `cargo build` (routes feature since 0.1.2+) |

## 10. Mapping to Automated Tests

| Manual section | Automated Section | Assertions |
|---|---|---|
| §3 frp | 2. FRP Tunnel | 9 |
| §4 local + forward | 3. Local Tunnel | 5 |
| §5 SOCKS5 proxy | 4. Proxy Tunnel | 4 |
| §6 multi-tunnel | 5. Multi-Tunnel | 8 |
| §7 boundary + disconnect + port pool | 6/7/8 | 17 |
| **Total** | | **43** |

One-shot regression:

```bash
bun tests/tunnel-proxy-comprehensive.ts    # or step 13 of ./tests/run-all.sh
```

## 11. Connection Keepalive and Self-Healing (must-read for production)

### Background

After long idle periods, the tunnel WebSockets (the `/tunnel` provider channel and `/tunnel/forward` consumer channel) may be **silently cut off** by NAT, cloud firewalls, or middleboxes — neither end notices (no FIN, just drops), so:

- The registry still shows the tunnel as existing, but the real connection is dead (stale / half-open)
- Every new request from the consumer is lost on the link, surfacing as `curl: (97) Can't complete SOCKS5 connection` or SOCKS5 `Connection refused`(05 05)
- Old versions required **manually restarting processes on both ends** to recover

### Fix Content (code level)

Since `0.2.1` (commit `4167ab5`):

| Layer | Mechanism | Effect |
|---|---|---|
| Server side `serve.rs` | All tunnel WS channels (provider `/tunnel` + consumer `/tunnel/forward`) send an application-layer `{"type":"ping"}` heartbeat every **30s** | Actively probes the link so middleboxes keep seeing live traffic and won't reclaim it as idle |
| Client side `tunnel_client.rs` | Client replies `pong` upon receiving `ping`; `run_expose` (`proxy exit` / `tunnel expose`) **automatically reconnects with exponential backoff** after WS disconnect (1s → 2s → 4s … capped at 30s) and **automatically re-registers the tunnel** | Provider self-heals after disconnection; the newly registered tunnel makes the consumer usable again |

> Version check: `teamx --version`. If either the server or client is below 0.2.1, upgrade the binary (redeploy with `cargo build --release`).

### Operational Fallback (process level)

Even though heartbeat + reconnect covers the WS layer, **process-level supervision** is still recommended for long-running processes, to handle process crashes, machine reboots, etc.:

**Cloud VM (systemd)** — one unit each for `teamx serve` and `proxy exit`:

```ini
# /etc/systemd/system/teamx-serve.service
[Unit]
Description=teamx network-mode server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=ubuntu
Environment=TEAMX_HOME=/home/ubuntu/.teamx
Environment=TEAMX_DB=/home/ubuntu/.teamx/teamx.db
ExecStart=/usr/local/bin/teamx serve --addr 0.0.0.0 --port 8888 --san hub03.flomesh.io
Restart=always
RestartSec=3
NoNewPrivileges=true
ProtectSystem=full

[Install]
WantedBy=multi-user.target
```

```ini
# /etc/systemd/system/teamx-proxy-exit.service
[Unit]
Description=teamx proxy exit egress
After=teamx-serve.service
Wants=teamx-serve.service

[Service]
Type=simple
User=ubuntu
ExecStart=/home/ubuntu/start-exit.sh
Restart=always
RestartSec=5
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
```

> In `start-exit.sh`, export `TEAMX_HOME` / `TEAMX_DB` / `TEAMX_SERVER_URL` / `TEAMX_MTLS_CERT|KEY|CA` then `exec teamx proxy exit <name>`.

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now teamx-serve teamx-proxy-exit
```

**Local (macOS/Linux clients)** — supervise `proxy start` with a while loop:

```bash
while true; do
  teamx proxy start --port 1080 --exit egress
  sleep 5   # auto-restart after abnormal process exit
done
```

### Verification Checklist

- [ ] `teamx --version` >= 0.2.1 (both server and client sides)
- [ ] Long-idle test: after `proxy start` + `proxy exit` idle longer than a heartbeat period (e.g. 100s), `curl --socks5-hostname 127.0.0.1:1080 https://example.com` still works
- [ ] Self-healing test: after `sudo systemctl kill -s SIGKILL teamx-serve`, serve is brought back by systemd, and proxy exit automatically reconnects and re-registers egress (watch `journalctl -u teamx-proxy-exit` show `ok tunnel registered`); the proxy stays usable throughout
