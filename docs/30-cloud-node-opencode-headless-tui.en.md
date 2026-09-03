# Cloud opencode member node: headless server + ttyd TUI (attach to the same server) + teamx tunnel remote access

> Status: verified and deployed on 3 cloud hosts (Tokyo/Seoul/US)
> Audience: anyone who wants a cloud-hosted opencode work node that is interactive and reachable remotely by the team lead
> Prerequisites: teamx enterprise (reverse `/tunnel`, per-user tunnel ACL); nodes are Ubuntu 24.04 aarch64

---

## 0. Why this layout

On one worker node we want both:

1. an **interactive terminal** (the worker drives the opencode TUI from a browser via ttyd), and
2. **Web / headless** access (the lead or the worker watches the *same* working state in a browser / via API).

Key point: **both must attach to the same opencode server**. Running two independent processes means history is shared but real-time events are not. So we use opencode's "one server + multiple clients" model:

```
opencode serve  (the single headless server, bound to 127.0.0.1:<PORT>)
   ├── ttyd → opencode attach http://127.0.0.1:<PORT>    (TUI as a client)
   └── opencode Web / Headless API  (reached over the tunnel in a browser)
```

> Anti-pattern: running `opencode -c` (TUI with an embedded server) *and* a separate `opencode serve` means two server instances — shared history, but not shared live events.

---

## 1. Architecture

```
              teamx server (authoritative ledger/CA, mTLS :443)
                     ▲                ▲
        (tunnel expose)        (tunnel expose)
                     │                │
        ┌────────────┴──────┐  ┌─────┴──────────────────┐
        │ ttyd (768x)       │  │ opencode serve (4199)  │   ← two local services exposed
        └────────┬──────────┘  └────────────────────────┘
                 ▼
   opencode attach http://127.0.0.1:4199     (TUI client, same server)

  Local side (the worker, or a lead):
    teamx tunnel forward <ttyd tunnel>   → http://127.0.0.1:xxxxx/   (browser: TUI)
    teamx tunnel forward <serve tunnel>  → http://127.0.0.1:yyyyy/   (browser: Web / Headless API)
```

- Zero inbound ports: the node opens no public port; ttyd/serve both bind `127.0.0.1` and are reached only through the teamx reverse tunnel.
- Identity: ttyd/serve/tunnel run as **that worker's teamx member** (its invitation certificate), so the tunnels belong to that user.

---

## 2. What to install on each node

```bash
# 1) teamx binary (build on Ubuntu aarch64, or copy from another aarch64 host)
sudo install -m 0755 teamx /usr/local/bin/teamx

# 2) ttyd
sudo install -m 0755 <ttyd.aarch64> /usr/local/bin/ttyd   # https://github.com/tsl0922/ttyd/releases

# 3) opencode (aarch64)
curl -fsSL https://opencode.ai/install | bash    # installs to ~/.opencode/bin
sudo cp ~/.opencode/bin/opencode /usr/local/bin/opencode

# 4) worker system user + work dir
sudo useradd -m -s /bin/bash -U <user>           # e.g. caicai
sudo mkdir -p /home/<user>/work && chown <user> /home/<user>/work
```

---

## 3. Make the node a teamx member (user-bound → decides tunnel access)

Issue the worker invitation bound to a user (`--user-name <person>` puts the user into the certificate CN):

```bash
# owner (on the serve machine) issues the worker invitation
teamx team invite "kb-engineer: 知识库工程师 (worker)" \
  --name-hint <user>-worker --user-name <user> \
  --server-url https://<server> --session <owner> --team <team-id>
```

Node imports + claims (local import stores the certificate; claim the seat over mTLS RPC; owner approves):

```bash
cd /home/<user>/work
teamx team import "<worker letter>" --name <user>-worker --session <node>
# claim RPC: POST /rpc team.import with the letter's mTLS material
# owner: teamx team approve <member_id> --session <owner>
```

After that `.teamx/letters/<id>/` holds `ca.crt/client.crt/client.key`, used by tunnel/serve authentication.

---

## 4. systemd: single server + attach

**4.1 opencode headless server (the single server)** — `/etc/systemd/system/opencode-serve-<user>.service`:

```ini
[Unit]
Description=opencode headless server for <user> (single server)
After=network-online.target

[Service]
Type=simple
User=<user>
WorkingDirectory=/home/<user>/work
Environment=HOME=/home/<user>
Environment=PATH=/usr/local/bin:/usr/bin:/bin
# Optional password for the control plane. Kept off here because the tunnel is
# per-user ACL'd (only the same user + owner/lead reach it). Keep it if the
# node is shared or trust boundaries are wider (serve warns if unsecured).
# Environment=OPENCODE_SERVER_PASSWORD=<pw>
ExecStart=/usr/local/bin/opencode serve --hostname 127.0.0.1 --port <PORT>
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
```

**4.2 ttyd = opencode attach (TUI client — do NOT run `opencode -c`)** — `/etc/systemd/system/ttyd-<user>.service`:

```ini
[Unit]
Description=ttyd web terminal for <user> (opencode attach client)
After=network-online.target opencode-serve-<user>.service

[Service]
Type=simple
User=<user>
# Note: opencode attach <url>, not opencode -c
ExecStart=/usr/local/bin/ttyd -p 768x -W -T xterm-256color -t disableReconnect=true \
  /bin/bash -lc "cd /home/<user>/work && exec /usr/local/bin/opencode attach http://127.0.0.1:<PORT> -c"
Restart=always
RestartSec=3
WorkingDirectory=/home/<user>/work
Environment=HOME=/home/<user>
Environment=PATH=/usr/local/bin:/usr/bin:/bin

[Install]
WantedBy=multi-user.target
```

> - Start the server first; ttyd depends on it (`After=...`).
> - `-c/--continue` resumes the last session; add `--password` on attach when a password is set.
> - If you previously ran `opencode -c` (embedded server), stop it first to avoid port/data contention.

Start and verify:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now opencode-serve-<user> ttyd-<user>

ps -eo pid,ppid,cmd | grep opencode | grep -v grep
#   opencode serve --hostname 127.0.0.1 --port <PORT>                    <- the only server
#   bash -lc ... exec opencode attach http://127.0.0.1:<PORT> -c        <- TUI client
sudo ss -tlnp | grep <PORT>     # exactly one listener
```

---

## 5. Expose to "the same user" over a teamx tunnel

**5.1 Register two provider tunnels on the node** (one systemd per local port):

```ini
# /etc/systemd/system/tx-expose-<user>-ttyd.service
ExecStart=/usr/local/bin/teamx tunnel expose ttyd-<user> \
  --port 768x --mode local --server https://<server> --session <user>
```
```ini
# /etc/systemd/system/tx-expose-<user>-serve.service
ExecStart=/usr/local/bin/teamx tunnel expose opencode-<user> \
  --port <PORT> --mode local --server https://<server> --session <user>
```

(`User=<user>`, `WorkingDirectory=/home/<user>/work`, `Restart=always`.)

Because the tunnel is bound to the worker user, the per-user ACL lets only **that user's devices**, the owner, and team leads forward it.

**5.2 Local side (worker / lead):**

```bash
teamx tunnel forward ttyd-<user>     --local-port 19033 --session <me>
teamx tunnel forward opencode-<user> --local-port 19034 --session <me>
```

Browser:
- `http://127.0.0.1:19033/` → ttyd TUI
- `http://127.0.0.1:19034/` → opencode Web / Headless (first **Add project → `~/work`**, then you can create a session; JSON API under `/api/session`)

---

## 6. Identity / permission model (important)

- **worker member**: the "robot" identity on the cloud node; runs `opencode serve`, `ttyd`, and `tunnel expose`.
- **personal member**: the real person (zhongm/zhaoz/caicai) on their own laptop; used to `tunnel forward` into their node.
- Both are issued with `--user-name <person>` bound to the **same user** → per-user tunnel ACL allows "that person + owner/lead", denies other users.
- **owner/co-leads see everything**: fixed a `serve.rs` bug where the tunnel ACL called `is_team_owner` with swapped arguments `(team_id, member_id)` instead of `(member_id, team_id)`; previously owner/leads were always denied on user-bound tunnels.
- **No-password trade-off**: serve binds 127.0.0.1 + per-user ACL means the headless control plane is only reachable by the person / owner / lead → we omit `OPENCODE_SERVER_PASSWORD` for easy browser access. Keep a password if the node is shared or the trust boundary is wider.

---

## 7. Archive invitations in the serve machine's `.teamx`

Keep copies of all worker/personal letters under the serve machine's project `.teamx/members/` (0600 root) for distribution and audit:

```
/var/lib/teamx/projects/<proj>/.teamx/members/
  <user>-personal/invitation.letter     # the person imports on their laptop
  worker-<user>/invitation.letter       # the node imports (already used)
```

> Invitations are single-use: once claimed by a node they are spent; hand the personal letters to the people — do not test them yourself.

---

## 8. Troubleshooting

| Symptom | Cause / fix |
|---|---|
| Worker online, the human (e.g. caishu) shows offline | "Online" means an open row in `member_connections` (a long-lived WS). Workers are online because `tunnel expose` keeps a WS; a human is online when their local opencode plugin holds a `/ws` (start opencode from their own workspace directory). |
| Clicking "Create a session" in the opencode Web does nothing | Open the Web, then **Add project → pick `~/work`** first; after that creating/chatting works. |
| TUI and headless are not real-time consistent | Two server instances (ttyd running `opencode -c` plus a separate `serve`). Unify to one `serve` + `attach`. |
| `Exec format error` | A macOS-built binary was copied to a Linux node. Always build/distribute on Ubuntu aarch64. |
| Lead can forward but curl is empty | Old `is_team_owner` argument-order bug; upgrade to the fixed build. |

---

## 9. Common operations

```bash
sudo systemctl status opencode-serve-<user> ttyd-<user> tx-expose-<user>-*
sudo journalctl -u opencode-serve-<user> -f
sudo systemctl restart opencode-serve-<user> ttyd-<user>     # after config/password change
# session API (after local forward):
curl http://127.0.0.1:19034/api/session
```
