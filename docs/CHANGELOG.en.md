# Changelog

## 0.2.0 — 2026-08-30

Teamx 0.2.0 grows from a shared-goal state machine into a full network collaboration platform: managed Git over mTLS, a transparent tun0 proxy, an owner-led design-review workflow, desktop GUI, and user-scoped tunnel isolation.

### Highlights

- **Git code hosting over mTLS** — native `git clone/pull/push` through the team server, plus automated repo provisioning when teams are created / members approved.
- **tun0 transparent proxy** (Linux + macOS) with a local DNS proxy, per-target egress routing, watchdog self-healing, and event-driven I/O (no busy-poll).
- **User identity & per-user tunnel ACL** — one person can own multiple devices; the same user's devices reach each other's tunnels with zero config, other users are denied, owners/leads keep oversight.
- **grill-with-docs** — an owner-led design deliberation workflow (design tree + fact requests + durable ADRs) delivered host-neutrally via a single protocol + generated adapters.
- **Desktop GUI** — tray app with a native control panel, live logs, and root-privileged tun0 management.
- **`@teamx-ai` plugins** published to npm (opencode + dsh).

### What's new

#### Git hosting (network mode)

- Team repositories managed over the teamx server, with standard **Git Smart HTTP over mTLS** — native `git clone` / `pull` / `push` work out of the box.
- **Team automation**: creating a team auto-initializes its repo; approving a member auto-grants read; importing an invitation auto-clones.
- Repo-level permissions (read / write / admin), `teamx git` CLI for clone/pull/push/commit/grant.

#### Networking & tunnels

- **Tunnel self-healing**: WS keep-alive heartbeat + auto-reconnect on drop, with a production runbook.
- **Per-target egress routing** for the SOCKS5 proxy: route domains/IPs to specific proxy exits.
- **tun0 transparent proxy** (virtual NIC) on Linux + macOS: no per-app configuration.
- **Local DNS proxy** with the original DNS kept as fallback (plus AAAA-poisoning guard).
- tun0 engineering: watchdog phase-1, async bridge setup, AsyncFd event-driven readiness (removes the 2 ms busy-poll).

#### Team collaboration

- **Multiple team leads** — owner can promote backup leads / co-leads (`is_lead`).
- **User identity + per-user tunnel ACL** (see Highlights). Device invitations carry a `user_id` in the certificate CN; `teamx user list` audits devices per person.

#### Design workflow

- **doc-flow**: TEAM.md `## Documents` section, generated doc-contract snapshot at team create, a declarative document lifecycle engine with permission + dynamic state-machine validation, and `doc.*` events wired into the publish loop.
- **grill-with-docs**: owner-led multi-round design sessions with a dependency-aware Design Tree, stable `DQ-`/`FR-` identifiers, Fact Reports, ADRs, and `CONTEXT.md` glossary — delivered as `/team-grill` (opencode) and a DSH runtime skill, generated from one host-neutral protocol.

#### Desktop GUI (macOS)

- Tray app → native egui control panel (not browser), live log panel, tun0 root authorization prompt, LaunchAgent install mode, double-clickable `Teamx.app`.

#### Plugins & publishing

- `@teamx-ai/opencode`, `@teamx-ai/dsh`, and installable bundle published to npm; dsh plugin registers the teamx runtime skill.

#### Engineering & docs

- Full codebase audit pass (security / correctness / usability) plus two rounds of review fixes (DNS cache, waiter leak, pipe deadlock, AAAA guards, UX).
- **Bilingual docs**: every design/manual/test/review doc now has cn/en pairs; new feasibility analyses, pure-CLI tunnel/proxy manual, and a comprehensive E2E suite (43 tunnel/proxy checks).
- Feasibility analysis for local traffic capture (L1 HTTP / L2 TCP) — a preview of the enterprise roadmap.

### Notes

- CLI/DB schema migrated to **v11** (adds `users` table, `members.user_id`, `invitations.user_id`). Existing databases are upgraded automatically on first run.
- Tunnel access semantics changed for *user-bound* devices: same-user devices can forward each other's tunnels; other users are denied. Legacy (unbound) members keep the previous team-wide access, so no existing team is affected.

## 0.1.0 — 2026-08-20

First release: Rust CLI (SQLite event ledger + state machine + mTLS server) + opencode plugin (30+ tools + `/team` commands + agent routing) + network mode + multi-member collaboration.

### Network Mode (N0—N4)

Team collaboration is no longer limited to a single machine. Members connect to a shared server over LAN with mTLS identity and real-time WebSocket push.

- **mTLS server** (N0): `teamx serve` runs an axum + tokio-rustls server with mandatory mutual TLS. RPC handlers derive member identity from client certificate CN (replacing self-reported session keys). `team.import` binds certificate identity to a pre-allocated seat via a dedicated path. Supports `--san <ip>` for LAN IP in server certificate SAN; plugin `serve start` auto-detects and passes the local IP.
- **WebSocket push** (N1): `GET /ws` endpoint registers subscribers by client certificate CN. Events are fanned out per team via `broadcast::Hub` (team→member→sender registry). 30s heartbeat, automatic cleanup on disconnect.
- **Revocation enforcement** (I2): `team invite-revoke` triggers active WebSocket disconnect for the revoked member. Certificates are rejected at connect/RPC time. Certificate = "can connect", approve = "can work"; revocation cuts off both.
- **Plugin event-driven** (N3): When WebSocket is connected, the poller sleeps (zero polling). On disconnect, exponential backoff reconnect (1s→60s) with polling fallback. Event frames are debounced (200ms) to batch bursts.
- **Cross-network verification** (N4): `tests/cross-network.sh` validates the full mTLS chain over a non-loopback IP. `docs/n4-cross-network.md` provides a two-machine runbook.

### Invitation Letters (I0—I1)

Members join via one-time invitation letters containing mTLS client certificates, replacing shared session keys.

- **`team invite "<role>: <desc>"`** (owner): Issues an mTLS client certificate (CN=`member:<id>:<role>`) and generates a self-contained invitation letter (`teamx-inv:v1:<base64>`). Role is automatically added to the team catalog.
- **`team import <letter>`** (member): Unpacks the letter, stores certs, and claims the pre-allocated seat (pending, auto-roled). Cross-machine: stores locally and prompts to connect for registration.
- **`team invite-list` / `team invite-revoke <id>`** (owner): List/revoke invitation letters; revoked certificates are rejected at connect.
- Plugin tools: `teamx_team_invite`, `teamx_team_import`, `teamx_team_invite_list`, `teamx_team_invite_revoke`.
- Slash commands: `/team invite`, `/team import`, `/team invite-list`, `/team invite-revoke`.

### Custom Roles

- Members can propose custom roles (`role propose`); owner approval automatically grants the role to the proposer.
- Owner can update any role's label/description (`role update`).
- `role set` only accepts approved roles (built-in + approved custom); pending roles trigger an error with a reminder to wait for approval.

### Command System

- **`/team <subcommand>`** routing: `create`, `join`, `status`, `sync`, `goal`, `approve`, `deny`, `role`, `state`, `ask`, `respond`, `publish`, `archive`, `help`. All subcommands have flat aliases (`/team-create`, `/team-invite`, ...) for tab completion.
- `teamx log` audit replay (resolves member names, supports `--team`, `--session`, `--limit`, `--after`).
- Owner uniqueness constraint: one session can own at most one non-archived team; archive before creating another.

### Three-Person Collaboration Demo

- `docs/demo-3p.md`: owner + contributor + reviewer workflow.
- `tests/three-member.sh`: automated end-to-end test (multi-member approval, parallel roles, Q&A, broadcast, close+archive).

### Production Hardening

- **State machine completeness**: removed unreachable `paused` state; added `team archive` (completed→archived) and `member set-state idle|active`; `achieved` can be reopened by the owner (start/resume→in_progress, refine→refining).
- **Data model**: removed redundant `sessions` table; added `UNIQUE(team_id, session_key)` on `members` and `UNIQUE(team_id)` on `goals`; member re-entry reuses the same row; sync cursor advances monotonically.
- **Authorization/robustness**: owner cannot `team leave`; `team approve/deny` supports `--team` disambiguation; `team create` is idempotent (same name reuses); `publish --data` falls back to `{"message": s}` for non-JSON.
- **Notification storm fix**: per-session notified-seq watermark; same batch of events only toasts once.
- **M2 polling + agent injection**: works without a server; polling refreshes digest + `experimental.chat.system.transform` injects team state into agent context.

### Code Review Fixes

Fixed all high/medium priority findings (see `docs/code-review-codex-0817.md`):

- **Cross-team read bypass** (security): non-members can no longer read invite tokens, members, roles, or events of arbitrary teams.
- **Pending members cannot publish** (authorization).
- **Non-object payload panic** (robustness): `publish --data '[]'` no longer panics.
- **Invitation letter path traversal** (security): `invitation_id` must be a UUID.
- **PKI partial-rebuild correctness**: `server.key` loss no longer invalidates issued member certificates.
- **Auto-execute seq watermark** (correctness): `shouldAutoExecute` uses `e.seq > lastExecutedSeq` for repeat triggers.
- **Directed task type matching** (correctness): `assignedToMe` matches any event with `assignee_member_id`.
- **Non-owner cannot `role set owner`** (authorization).

### Security Model

V1 has no real authentication (`session_key` self-reported, `invite_token` visible to all members). This is a "trust this machine" collaboration convention. Documented in `docs/goal-v1.md` and `v1-spec.md`.

### Testing

`tests/run-all.sh` runs a 9-step automated suite: CLI edge cases, mTLS identity + revocation, WebSocket push + disconnect + reconnect, cross-network LAN verification, and plugin unit tests (auto-execute, WebSocket, state machine). `tests/acceptance.sh` runs real-model acceptance tests (headless `opencode run --agent teamx`).

### Tech Stack

Rust CLI (axum + tokio-rustls + rusqlite + rcgen) · TypeScript plugin (opencode plugin API) · SQLite WAL · mTLS (ring + x509-parser + base64) · WebSocket (axum ws)
