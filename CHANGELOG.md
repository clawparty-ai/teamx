# Changelog

## 0.1.0 — 2026-08-17

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

Fixed all high/medium priority findings (see `code-review-codex-0817.md`):

- **Cross-team read bypass** (security): non-members can no longer read invite tokens, members, roles, or events of arbitrary teams.
- **Pending members cannot publish** (authorization).
- **Non-object payload panic** (robustness): `publish --data '[]'` no longer panics.
- **Invitation letter path traversal** (security): `invitation_id` must be a UUID.
- **PKI partial-rebuild correctness**: `server.key` loss no longer invalidates issued member certificates.
- **Auto-execute seq watermark** (correctness): `shouldAutoExecute` uses `e.seq > lastExecutedSeq` for repeat triggers.
- **Directed task type matching** (correctness): `assignedToMe` matches any event with `assignee_member_id`.
- **Non-owner cannot `role set owner`** (authorization).

### Security Model

V1 has no real authentication (`session_key` self-reported, `invite_token` visible to all members). This is a "trust this machine" collaboration convention. Documented in `goal-v1.md` and `v1-spec.md`.

### Testing

`tests/run-all.sh` runs a 9-step automated suite: CLI edge cases, mTLS identity + revocation, WebSocket push + disconnect + reconnect, cross-network LAN verification, and plugin unit tests (auto-execute, WebSocket, state machine). `tests/acceptance.sh` runs real-model acceptance tests (headless `opencode run --agent teamx`).

### Tech Stack

Rust CLI (axum + tokio-rustls + rusqlite + rcgen) · TypeScript plugin (opencode plugin API) · SQLite WAL · mTLS (ring + x509-parser + base64) · WebSocket (axum ws)
