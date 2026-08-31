# Running teamx server and teamx members on cloud hosts (best practice)

> Status: **verified in practice** (hub03 = serve + owner; 197 = ttyd + opencode + member)
> Audience: deployers who want to upgrade teamx from a single-machine demo to cloud multi-party collaboration
> Prereq: `teamx` binary v0.2.0+ (with `team invite --as-lead / --auto-approve`, schema v14)
> Related: `docs/03-design-network.en.md`, `docs/20-manual-tunnel-proxy-cli.en.md`, `docs/04-design-team-invite.en.md`

---

## 0. Architecture overview

```
                 ┌─────────────────────────────────────────────┐
                 │   Cloud host A (hub03, public)              │
                 │   systemd: teamx serve :8888 (mTLS)         │
                 │   - authoritative SQLite ledger (TEAMX_DB)  │
                 │   - CA / server cert (~/.teamx/ca)          │
                 │   - team created here: owner identity on A  │
                 └──────────────┬──────────────────────────────┘
                                │ mTLS (cert CN = member:<id>:<role>[:<user>])
        ┌───────────────────────┼────────────────────────────┐
        ▼                       ▼                             ▼
 ┌───────────────┐      ┌───────────────┐            ┌───────────────┐
 │ Local admin   │      │ Cloud host B  │            │ Other members │
 │ machine       │      │ (197)         │            │ teamx CLI/    │
 │ import lead   │      │ ttyd+opencode │            │ plugin        │
 │ letter then   │      │ teamx plugin  │            │               │
 │ manages team  │      │ tunnel expose │            │ tunnel forward│
 │ remotely      │      │ ttyd :7681    │            │ → local port  │
 └───────────────┘      └───────────────┘            └───────────────┘
```

**Core idea**: one cloud host runs `teamx serve` (authoritative ledger + CA); the team is **created locally on that host** (owner identity naturally lives on the serve host); every other machine (local admin, second cloud host, ordinary members) is a member/lead connecting outbound over mTLS — **zero inbound ports, zero exposure**.

---

## 1. Prepare the teamx binary

All three places need the teamx binary (local admin / cloud host A / cloud host B):

- Local: `cargo build --release -p teamx` then `install -m 0755 target/release/teamx ~/.local/bin/teamx`
- Cloud hosts: copy the source tarball to the target host, build **on that architecture** (ARM/x86 compile separately), then install to the system path:
  ```bash
  sudo install -m 0755 ~/teamx/target/release/teamx /usr/local/bin/teamx
  ```
- On cloud hosts you may also keep `~/.local/bin/teamx` (the systemd unit can point at either path).

---

## 2. Cloud host A: start teamx serve with systemd

Use a **dedicated system user** (e.g. `teamx`) to isolate file permissions:

```bash
sudo useradd -r -m -s /usr/sbin/nologin teamx
```

Create `/etc/systemd/system/teamx-serve.service`:

```ini
[Unit]
Description=teamx network-mode server (HTTP RPC + WS push + tunnels)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=teamx
Environment=TEAMX_HOME=/home/teamx/.teamx
Environment=TEAMX_DB=/home/teamx/.teamx/teamx.db
ExecStart=/usr/local/bin/teamx serve --addr 0.0.0.0 --port 8888 --san <cloud-host-domain-or-public-ip>
Restart=always
RestartSec=3
NoNewPrivileges=true
ProtectSystem=full
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

> Notes:
> - `--port 8888`: serve's mTLS HTTPS port (shared by RPC + WS + tunnels). The security group must open it.
> - `--san`: repeatable; give both the domain and the public IP (e.g. `--san hub03.example.com --san 1.2.3.4`), otherwise client SNI verification fails.
> - On first start the instance CA (`$TEAMX_HOME/ca/`) and server certificate are generated automatically.
> - **Do not treat `0.0.0.0:8888` as a public service for non-members**: mTLS already requires a client certificate; connections without one are rejected at the TLS layer.

Start and verify:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now teamx-serve
sudo systemctl status teamx-serve        # active (running)
journalctl -u teamx-serve -n 20          # expect "teamx serve listening on https://0.0.0.0:8888 (mtls, ...)"
```

---

## 3. Create the team locally on cloud host A (owner on the serve host)

> **Why the first team must be created on the serve host**: in network mode `team.create` requires the caller to already hold a member seat (a certificate), and the first team has no invitation that could bootstrap one. So the first team is created locally via CLI on the host that holds the CA (a bootstrap constraint).

```bash
# Run as the teamx user (or any shell with write access to the DB):
sudo -u teamx bash
export TEAMX_HOME=/home/teamx/.teamx TEAMX_DB=/home/teamx/.teamx/teamx.db

teamx team create "cloud-proxy-lab" --session s:cloud \
  --goal-title "cloud proxy lab team"
# Expected: ok, team with id / invite_token (invite_token is only for local join; network mode uses letters)
```

Record the returned **team id** (used later for `--team <id>` in invite / tunnel commands).

> Tip: the owner session `--session` value (e.g. `s:cloud`) is only meaningful in the serve host's local CLI context; in network mode the server always identifies by the **certificate CN** and ignores `--session`.

### 3.1 Create a teamx git repository (also on the server)

Right after the team is created, create the code repository on the serve host (bare repos live under `~/.teamx/repos/<team_id>/<name>.git`; members access them over mTLS — no self-hosted git server needed):

```bash
# Cloud host A (team lead/owner):
teamx git create <repo-name> --description "team code repository" \
  --session s:cloud --team <team-id>
# Expected: ok repo=<repo-name> (bare repo initialized, default branch main)

# List the team's repositories (members can also run this)
teamx git list --session s:cloud --team <team-id>
```

> Notes:
> - `teamx git create` is **team-lead only** (owner or co-lead); `serve.rs`'s `git.create` checks `is_team_owner`.
> - Repo permissions: `read` (clone/pull) / `write` (push) / `admin`; **approval auto-grants read** (`team approve` and auto-approve both do this), so members imported via a lead letter (`--auto-approve`) can clone immediately.
> - To let more members write, the owner explicitly runs `teamx git grant <repo> <member_id> --permission write --session s:cloud --team <id>`.
> - Repos are team assets; remove with `teamx git delete <repo> --session s:cloud --team <id>` (owner/admin).

---

## 4. Issue the first invitation: mark as team lead + auto-approve

> Requires v0.2.0+ (schema v14) `team invite --as-lead --auto-approve`. If your binary lacks these flags, see the "Compatibility" section at the end.

```bash
# Cloud host A (owner):
teamx team invite "管理员: 远程管理团队" \
  --name-hint "admin" \
  --server-url https://<cloud-host-A>:8888 \
  --as-lead --auto-approve \
  --session s:cloud --team <team-id>
# Expected: ok + letter (teamx-inv:v1:<base64> ...)
```

Notes:
- `--as-lead`: the member becomes a **co-lead immediately on import** (`is_lead=1`), able to exercise owner-level team authorization (invite/approve/goal/role etc.), except "granting the owner role to others / demoting the true owner".
- `--auto-approve`: the member becomes **active immediately on import** (skips the owner's manual approval) and is auto-granted read access to all team git repositories.
- `--server-url`: **must be an address members can reach** (cloud host domain or public IP), never `127.0.0.1`.
- `--as-lead` may only be issued by the **primary owner** (a co-lead cannot mint new leads).

Send the returned single-line `letter` to the target admin.

---

## 5. Admin imports the letter on the local machine → gains remote management

On the admin machine (local) with the teamx binary ready:

```bash
# Local; import needs no connection to the server (it stores the cert and claims the seat)
teamx team import "teamx-inv:v1:<...paste letter...>" --name admin --session s:admin
# Expected: ok, status=active, is_lead=true   (auto-approve + as-lead already applied)
```

After import:
- Local `~/.teamx/letters/<invitation_id>/` holds `client.crt / client.key / ca.crt`
- The local **opencode plugin** auto-discovers the server URL from the letter (no need to set `TEAMX_SERVER_URL`), and all `teamx_*` tools talk over the network channel
- The admin can now manage the team remotely: `teamx team status --team <id>`, `teamx goal set`, `teamx team invite`, etc. (pass `--session s:admin --team <id>` when needed)

> Verify: `teamx team status --session s:admin --team <team-id>` should report `my_is_lead: true`.

### 5.1 Clone the team code locally

After importing, the admin (and every member) can clone the server's repositories locally. Two equivalent ways:

**Option 1: `teamx git clone` (bundle transport, recommended inside the team)**

```bash
# Local:
teamx git clone <repo-name> --session s:admin --team <team-id>
# Expected: cloned <repo-name> -> ./<repo-name> (with origin recorded)
cd <repo-name>
# Later commits:
teamx git commit -m "message"
teamx git push <repo-name> --session s:admin --team <team-id>
# or in one step: teamx git commit-push -m "message" --repo <repo-name> --session s:admin --team <team-id>
# Pull updates:
teamx git pull <repo-name> --session s:admin --team <team-id>
```

**Option 2: standard git over mTLS (smart HTTP, for those who prefer plain git)**

```bash
# Local, one-time config: write the certificate paths into ~/.gitconfig (auto-read from the imported letter):
teamx git setup --server https://<cloud-host-A>:8888
# Then use plain git (the URL carries the mTLS certs automatically):
git clone https://<cloud-host-A>:8888/git/<team-id>/<repo-name>
cd <repo-name>
git pull && git push
```

> Notes:
> - clone needs **read** permission: lead letters (`--auto-approve`) already grant it on import; ordinary members are granted by the owner via `teamx git grant <repo> <member_id> --permission read`.
> - `teamx git setup` writes `http.<server>/.sslCert / .sslKey / .sslCAInfo` into `~/.gitconfig` (`git_client.rs`), so plain git passes mTLS.
> - Both options access the **same server repository** and can be mixed.

---

## 6. Another cloud host B: ttyd + opencode + teamx plugin, and expose ttyd to the team via tunnel

### 6.1 Prepare cloud host B

1. Install the teamx binary (system path `/usr/local/bin/teamx`).
2. Install opencode (per-architecture official installer) and ttyd:
   ```bash
   curl -fsSL https://opencode.ai/install | bash        # installs to ~/.opencode/bin
   sudo cp ~/.opencode/bin/opencode /usr/local/bin/opencode
   sudo install -m 0755 <ttyd-binary> /usr/local/bin/ttyd
   ```
3. Create the cicd user (optional, to isolate the web-terminal identity):
   ```bash
   sudo useradd -m -s /bin/bash -U cicd
   ```

### 6.2 Run ttyd + opencode with systemd

`/etc/systemd/system/ttyd-cicd.service`:

```ini
[Unit]
Description=ttyd web terminal for cicd user (opencode)
After=network-online.target

[Service]
Type=simple
User=cicd
# `opencode -c` resumes the last session; bash indirection avoids a SEGV when
# ttyd execs `opencode -c` directly
ExecStart=/usr/local/bin/ttyd -p 7681 -W -T xterm-256color -t disableReconnect=true /bin/bash -lc "cd /home/cicd && exec /usr/local/bin/opencode -c"
Restart=always
RestartSec=3
WorkingDirectory=/home/cicd
Environment=HOME=/home/cicd
Environment=TEAMX_SERVER_URL=https://<cloud-host-A>:8888
Environment=PATH=/usr/local/bin:/usr/bin:/bin

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload && sudo systemctl enable --now ttyd-cicd
sudo ss -tlnp | grep 7681    # ttyd listening on 0.0.0.0:7681
```

> **Why `opencode -c` + bash indirection**:
> - `opencode -c` (`--continue`) resumes the **last session** on startup (session data lives in cicd's `~/.local/share/opencode/opencode.db`), so every ttyd reconnect returns to the previous workspace instead of opening a fresh session.
> - Running `ttyd ... /usr/local/bin/opencode -c` **directly** SEGV-crashes on ttyd 1.7.7 (a PTY compatibility issue when ttyd forks+execs opencode in resume mode; log shows `code=killed, status=11/SEGV`).
> - Wrapping it as `/bin/bash -lc "cd /home/cicd && exec opencode -c"` runs stably — bash re-initializes the terminal and avoids the crash.
> - If your opencode lives elsewhere (e.g. hub03's `/home/cicd/.local/bin/opencode`), point the ExecStart path at the actual binary.
> - ttyd serves a web terminal on 7681 (directly entering the opencode TUI). **No auth by default**: if the security group exposes it publicly, add `-c user:pass` to `ExecStart` or reach it via tunnel/private network.

### 6.3 Cloud host B joins as a teamx member

Host B needs a member invitation (owner can issue one with the same method as step 4, or a plain invitation without `--as-lead`):

```bash
# Cloud host B:
teamx team import "teamx-inv:v1:<...>" --name cicd-b --session s:b
```

### 6.4 Expose the ttyd port to the team through the teamx tunnel

Cloud host B is the **provider**, exposing its local 7681 to the team (`local` mode — the server binds no public port, most secure):

```bash
# Cloud host B (long-running; use systemd or nohup):
teamx tunnel expose ttyd --port 7681 --mode local \
  --session s:b --team <team-id> --server https://<cloud-host-A>:8888
# Expected: ok tunnel registered: name=ttyd mode=local port=0 target_port=7681
```

### 6.5 The admin maps the ttyd port back locally

The admin (or any authorized member) acts as the **consumer** on the local machine:

```bash
# Local:
teamx tunnel forward ttyd --local-port 17681 \
  --session s:admin --team <team-id>
# Expected: ok forward: name=ttyd listening on 127.0.0.1:17681
```

Then open `http://127.0.0.1:17681/` in a browser to reach cloud host B's web terminal (opencode). **The 7681 public port is never exposed.**

> Tunnel access isolation: if the invitation was bound to a `user`, devices of the same user reach each other's tunnels with zero config; other users are denied by default, with owner/lead exceptions (see `docs/20-manual-tunnel-proxy-cli.en.md` §4.5).

---

## 7. Day-2 operations cheat sheet

| Operation | Command (on the relevant host) |
|---|---|
| Serve status | `sudo systemctl status teamx-serve` |
| Serve logs | `journalctl -u teamx-serve -f` |
| Team status | `teamx team status --session <s> --team <id>` |
| Issue member invite | `teamx team invite "role: desc" --server-url https://A:8888 --session <s> --team <id>` |
| Issue lead + auto-approve | add `--as-lead --auto-approve` |
| List invitations | `teamx team invite list --session <s> --team <id>` |
| Revoke invitation | `teamx team invite revoke <invitation_id> --session <s> --team <id>` |
| Create a repo | `teamx git create <repo> --description "..." --session <s> --team <id>` |
| List repos | `teamx git list --session <s> --team <id>` |
| Clone locally | `teamx git clone <repo> --session <s> --team <id>` |
| Configure plain-git mTLS | `teamx git setup --server https://A:8888` |
| Commit + push | `teamx git commit-push -m "msg" --repo <repo> --session <s> --team <id>` |
| Pull updates | `teamx git pull <repo> --session <s> --team <id>` |
| Grant a member | `teamx git grant <repo> <member_id> --permission write --session <s> --team <id>` |
| Expose a service | `teamx tunnel expose <name> --port <local> --mode local --session <s> --team <id>` |
| Use it locally | `teamx tunnel forward <name> --local-port <p> --session <s> --team <id>` |
| Close a tunnel | `teamx tunnel close <name> --session <s> --team <id>` |

---

## 8. Security and caveats

1. **8888 security group**: only open it to peers that need to collaborate; mTLS already gives cert-level protection, but the security group further shrinks the surface.
2. **ttyd has no auth**: unless reached via tunnel/private network, public exposure must add `-c user:pass`.
3. **`--server-url` must not be `127.0.0.1`**: the server URL in the letter is what remote members connect to.
4. **Owner identity lives on the serve host**: the true owner's cert/DB reside on cloud host A; leads (co-leads) can manage remotely but the founder semantics are preserved.
5. **Certificate is identity**: a leaked letter lets an attacker import that seat; use `team invite revoke` to invalidate it (the cert is rejected at connect).
6. **Schema migration**: v0.2.0 → newer versions auto-migrate on first run (e.g. v14 adds `invitations.auto_approve/as_lead`); no manual steps.

---

## 9. Compatibility: older binary without `--as-lead / --auto-approve`

If the serve host runs v0.1.x/v0.2.0 (no v14), the equivalent flow degrades to two steps:

```bash
# 1) Issue a plain invitation
teamx team invite "管理员: 远程管理团队" --server-url https://A:8888 --session s:cloud --team <id>
# 2) After the member imports, the owner manually approves + promotes on the serve host
teamx team approve <member_id> --session s:cloud --team <id>
teamx team promote-lead <member_id> --session s:cloud --team <id>
```
