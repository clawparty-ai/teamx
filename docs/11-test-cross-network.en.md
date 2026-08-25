# teamx N4 — Cross-Network Integration Runbook (Two Machines)

> Status: **pending verification on real machines** (single-machine LAN simulation already passes `tests/cross-network.sh`)
> Prerequisites: `teamx` installed via `install.sh` to `~/.local/bin/teamx` on both machines; the teamx plugin installed in opencode on both machines.

N4 goal: opencode sessions on different machines collaborating across networks through the owner's embedded `teamx serve` (form ①). This document gives the minimal integration steps and acceptance checklist for **two machines**.

---

## 0. Network and Security Prerequisites

- Both machines are on the same LAN (or members can route to the owner's IP:port).
- The owner machine allows inbound port `5781` (macOS firewall / Linux `ufw allow 5781/tcp`).
- mTLS enforced: network mode has no plaintext fallback; members must hold client certificates (invitation letters) issued by the owner.

## 1. Owner Side (Machine A)

```bash
# 1.1 Confirm the LAN IP (not loopback)
ipconfig getifaddr en0      # macOS; on Linux use `hostname -I | awk '{print $1}'`

# 1.2 Create the team and start the embedded serve in opencode
#   /team create My Team
#   /team serve start        # the plugin auto-detects the LAN IP and runs --addr 0.0.0.0 --san <IP>
#   (or manually) teamx serve --addr 0.0.0.0 --port 5781 --san <your-LAN-IP>

# 1.3 Invite members (be sure to pass --server-url <your-LAN-IP>)
#   /team invite "Test Engineer: responsible for testing and reporting defects" --server-url https://<your-LAN-IP>:5781
#   (or) teamx team invite "Test Engineer: responsible for testing" --server-url https://<IP>:5781 --session <owner-session> --json
#   → yields a single-line invitation letter (teamx-inv:v1:...), sent to the member over a secure channel
```

Key points:
- `serve start` prints `server_url: https://<LAN-IP>:5781`; members point at that address.
- When inviting, `--server-url` must use the **LAN IP** (not `127.0.0.1`), otherwise members cannot connect.

## 2. Member Side (Machine B)

```bash
# 2.1 Import the invitation letter (unpacked locally + certificate/private key stored under ~/.teamx/letters/<id>/)
#   /team import <letter>
#   (or) teamx team import <letter> --name Me --session <member-session> --json
#   → once written to local disk, it prompts "connect to the server to complete registration"

# 2.2 Configure the server address (the plugin builds its mTLS RPC/WS connection from this)
export TEAMX_SERVER_URL="https://<owner-LAN-IP>:5781"
#   At startup, the plugin auto-discovers matching client certificates under ~/.teamx/letters (or specify them explicitly via env):
#   export TEAMX_MTLS_CERT=~/.teamx/letters/<id>/client.crt
#   export TEAMX_MTLS_KEY=~/.teamx/letters/<id>/client.key
#   export TEAMX_MTLS_CA=~/.teamx/letters/<id>/ca.crt

# 2.3 Reopen/restart opencode and enter the team with /Team; the member is now pending,
#     and the owner needs to approve on machine A:
#   /team approve <member_id>
```

Key points:
- Members **open no inbound ports** (outbound registration; NAT-friendly).
- A certificate means "can connect"; owner approval means "can work"; after revocation even connections are refused.

## 3. Acceptance Checklist

| # | Check | Command / Expected |
|---|---|---|
| 1 | Member can connect | member-side `/team status` returns the team (identity comes from the certificate, not a self-reported session) |
| 2 | Real-time mutual visibility | after owner `/team publish decision`, the member's WS shows a "new event" toast within <1s |
| 3 | Certificate identity | member curl `--cacert ca.crt --cert client.crt --key client.key https://<IP>:5781/rpc -d '{"method":"team.status","args":{}}'` returns their own team |
| 4 | No certificate rejected | `curl https://<IP>:5781/health` fails (rejected by mTLS) |
| 5 | Revocation takes effect | after owner `/team invite-revoke <id>`, the member's RPC reports `revoked` and the WS is disconnected |
| 6 | Disconnect fallback | stop the owner serve → the member plugin falls back to polling; restart serve → automatic reconnect and push resumes |

## 4. Troubleshooting

| Symptom | Cause / Fix |
|---|---|
| Member cannot connect / TLS handshake fails | owner firewall not allowing the port; or `--server-url` used `127.0.0.1` |
| Certificate validation failure (unable to verify) | server cert SAN missing the LAN IP → restart serve with `--san <IP>` |
| Member still cannot run status after importing | not approved yet; owner needs `/team approve <member_id>` |
| Logs report `member has been revoked` | that member's invitation was revoked by the owner |

## 5. Single-Machine Automated Verification

Beyond real two-machine integration testing, the repo ships a **single-machine LAN simulation** (exercising the full mTLS chain over a non-loopback IP, equivalently verifying cert SAN + CA trust):

```bash
./tests/cross-network.sh    # auto-skipped when no LAN IP is available
```

Covers: server cert SAN containing the LAN IP, RPC identity resolution over the LAN IP, and `team.import` over the LAN IP.
