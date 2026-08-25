# teamx Network Mode Manual Test: Owner + Tester + Reviewer, Three in Parallel

> Scenario: **one team lead** creates a team, starts an embedded `teamx serve`, and **issues two invitation letters** (one for testing, one for code review); the two members each import their letter and connect to the owner's serve over **mTLS + WebSocket**; after **approval they start working simultaneously**, with the owner seeing both members' progress in real time.
>
> Compared to `docs/13-demo-team.md` (V1 single-machine token-based join), this document walks through **network mode** (form ①: owner-embedded serve): identity comes from mTLS client certificates (invitation letters), push goes over WebSocket — no more "self-declared session + polling".

## 0. Prerequisites

1. `./install.sh` has been run and opencode restarted (`/Team` and `/team` subcommands available).
2. `which teamx` → `~/.local/bin/teamx`; `cd ~/github/teamx && ./tests/smoke.sh` all PASS.
3. Three opencode windows (same machine is fine; two machines see §3).
4. The owner's machine allows inbound port `5781` (only needed cross-machine).

## 1. Network-Mode Data Flow (Three People)

```
┌─ Owner window ─────────────┐   ┌─ Tester window ─────────┐   ┌─ Reviewer window ────────┐
│ /Team agent + plugin       │   │ /Team agent + plugin    │   │ /Team agent + plugin     │
│ create / serve start/invite│   │ import letter → mTLS    │   │ import letter → mTLS     │
└────────────┬───────────────┘   └────────────┬────────────┘   └────────────┬────────────┘
             │ spawn teamx serve (owner box)  │  wss://<owner-ip>:5781      │
             ▼                                 ▼                            ▼
   ┌───────────────────────────────────────────────────────────────────────────────┐
   │  teamx serve (mTLS enforced) — SQLite ledger (single source of truth) + RPC + WS broadcast (team→member)│
   └───────────────────────────────────────────────────────────────────────────────┘
```

- **Identity**: each member's client certificate CN = `member:<id>:<role>`; the server parses member identity from the certificate (no self-reported sessions).
- **Push**: whenever any party `publishes` and it lands in the ledger, the server broadcasts in real time to all online members' WS connections by team.
- **Certificate ≠ authorization**: a certificate only gets you "connected + submit a join request"; only after the owner's `approve` do you enter `active` and get to work.

## 2. Recommended: Single Machine, Three Windows (Embedded Serve)

### 2.1 Owner Window —— Create Team + Start Service + Issue Two Invitations

```
/Team 创建团队「网络协作组」，目标：完成一次真实的多人协作（测试 + 评审）。
```

Expected: `teamx_create_team` → returns `owner_member_id` (note it down) and the team id.

Then start the embedded serve (prints the server URL):

```
/team serve start
```

Expected: returns `server_url: https://<your LAN IP>:5781` (e.g. `https://172.20.10.3:5781`). Note this address; invitations and member connections both need it.

> Manual equivalent: `teamx serve --addr 0.0.0.0 --port 5781 --san <your LAN IP> &`
> Find your LAN IP: macOS `ipconfig getifaddr en0`; Linux `hostname -I | awk '{print $1}'`.
>
> Optional (if the owner also wants real-time push): issue yourself a certificate too and set `TEAMX_SERVER_URL` to connect to the serve —
> `teamx cert issue <owner_member_id> owner --out ~/.teamx/owner-cert`, then
> `export TEAMX_SERVER_URL=https://<your LAN IP>:5781` together with
> `export TEAMX_MTLS_CERT=~/.teamx/owner-cert/member.crt TEAMX_MTLS_KEY=~/.teamx/owner-cert/member.key TEAMX_MTLS_CA=~/.teamx/ca/ca.crt`, and restart opencode.
> Skipping this doesn't affect collaboration; the owner can just use `teamx_sync` to pull.

Issue two invitation letters (**be sure to pass `--server-url <address from above>`**):

```
/team invite "测试工程师: 负责功能测试并汇报缺陷" --server-url https://<你的局域网IP>:5781
```

```
/team invite "reviewer: 负责代码评审并给出意见" --server-url https://<你的局域网IP>:5781
```

Expected: both return a single-line **letter** (`teamx-inv:v1:...`) — the first for testing, the second for review. Copy each one and send them via chat/file to the two members.

### 2.2 Tester Window —— Import + Connect

1. First import the invitation letter with the CLI (**stores certificates + claims the pending seat**; on a shared local DB this is one step):

   ```
   /team import <tester's letter> --name 测试员
   ```

   Expected: `teamx_team_import` → `status=pending`, `role=role-<hex>` (a key auto-derived from the Chinese label「测试工程师」), plus a hint that the certificate was stored under `~/.teamx/letters/<invitation_id>/`.

2. Make this window use network mode (connecting to the owner's serve for real-time push): **just restart opencode** — the letter contains the server URL (`teamx_invitation.server.url`); the plugin discovers it at startup and establishes mTLS RPC/WS automatically, no need to set `TEAMX_SERVER_URL` manually.

> With multiple servers / when overriding is needed you may still set it explicitly:
> ```bash
> export TEAMX_SERVER_URL="https://<你的局域网IP>:5781"
> # then restart opencode
> ```
> Manual equivalent: `teamx team import <letter> --name 测试员 --session <this session key>`.

### 2.3 Reviewer Window —— Import + Connect

Same as above, using the second letter:

```
/team import <reviewer's letter> --name 评审员
```

Expected: `status=pending`, `role=reviewer` (an ASCII label used directly as the key).

Then likewise **restart opencode** (the plugin discovers the server URL from the letter automatically and connects).

### 2.4 Owner Window —— Approve Both Members

```
/Team 审批所有待审批成员。
```

Expected: `teamx_approve` × 2 (both tester and reviewer become `active`, roles retained).

### 2.5 Three People Work in Parallel (Verify Real-Time Push)

**Tester window**, type:

```
/Team 同步团队状态，开始编写测试用例，完成后向团队汇报「测试用例编写完成」。
```

**Reviewer window** (almost simultaneously), type:

```
/Team 同步团队状态，开始代码评审，完成后向团队汇报「代码评审完成」。
```

**Owner window**, type:

```
/Team 同步团队状态，观察两名成员的最新进展。
```

Expected (the core of network mode):
- After any member runs `teamx_publish progress`, **all other online members (WS-connected) receive the push within <1s** (TUI toast "new event"), no manual `teamx_sync` needed.
- If the owner is also WS-connected (see the optional step in §2.1), they'll get real-time toasts; otherwise they pull both `progress.published` events (testing/review) sorted by seq with `teamx_sync`.

## 3. Advanced: Two Machines (True Cross-Network)

The owner is on machine A; tester/reviewer on machine B (or one each). Steps are identical to §2; the only difference is **how members connect**:

1. Owner (machine A) creates the team + `serve start` + invites with `--server-url https://<machine A's LAN IP>:5781`.
2. Member (machine B) first imports the letter locally to store certificates (at this point the local DB has no such invitation, so it returns `status=stored`, disk-only):
   ```bash
   teamx team import <letter> --name 测试员 --session <this session key>
   ```
3. Member **restarts opencode** (the plugin discovers the server URL from the letter automatically and connects; only set `export TEAMX_SERVER_URL` explicitly when overriding across multiple servers).
4. Member runs `/team import <letter>` again inside `/Team` (now over RPC, binding the certificate identity to the pre-allocated seat on the server, becoming pending; the plugin also auto-claims on first RPC).
5. After the owner approves, parallel work begins.

> Members **open no inbound ports at all** (outbound registration); certificate = "can connect", approve = "can work"; after revocation even connecting is refused.

## 4. Verification Checklist (Any Terminal)

```bash
# team_id returned when the owner created the team
teamx team status --team <team_id> --json
#  → three members: owner(owner/active), tester(role-xxx/active), reviewer(reviewer/active)

teamx events --team <team_id> --json
# the event chain should contain (by seq):
# team.created → invitation.created×2 → membership.pending×2 → membership.approved×2
#   → progress.published(testing) → progress.published(review) (order depends on who reports first)

# Liveness: online connection count of the owner's serve (number of WS-connected members; 3 if the owner also connects)
curl --cacert ~/.teamx/ca/ca.crt --cert <owner cert> --key <owner key> https://<ip>:5781/health
#  → "connections": 2 (two members online; 3 if the owner connects too)
```

## 5. Troubleshooting

| Symptom | Cause / Fix |
|---|---|
| Member's `import` errors `cannot read ca.crt` | owner didn't run `serve start` first (PKI not generated); start serve or run `teamx cert init` first |
| Member can't connect / TLS handshake fails | invitation used `127.0.0.1` as `--server-url` (should be the LAN IP); or firewall blocks 5781 |
| Certificate verification failure (unable to verify) | server cert SAN missing the LAN IP → restart serve with `--san <IP>` (`serve start` passes it automatically) |
| Members don't see real-time push | `TEAMX_SERVER_URL` not set or opencode not restarted; or still in pure CLI polling mode |
| RPC errors `member has been revoked` | that member's invitation was revoked by the owner via `invite-revoke` |
| Member's `status` errors `member ... not found` | hasn't `import`ed on the server yet (step 4 of the two-machine scenario); or certificate doesn't match the letter's member_id |

## 6. Automated Equivalent Verification

Automated tests already exist for the three-person/network flow, driving the same event chain and mTLS/WS paths without any real model:

```bash
./tests/run-all.sh            # full suite (includes mtls-test.sh / ws-test.ts / cross-network.sh)
./tests/cross-network.sh      # single-machine LAN simulation (full mTLS over a non-loopback IP)
./tests/mtls-test.sh          # certificate identity + revocation enforcement
bun tests/ws-test.ts          # WS push: register / real-time broadcast / heartbeat / revoked disconnect
```

## 7. Test Record

- Date: ____
- Method: □ Single machine, three windows　□ Two machines
- Result: □ All passed　□ Partially passed (issues: __________)
- Did both members receive each other's/owner's pushes in real time: □ Yes　□ No
- Event chain: `invitation.created×2 → membership.pending×2 → membership.approved×2 → progress.published×2` complete? □ Yes　□ No
