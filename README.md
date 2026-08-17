# teamx

> Multi-agent collaboration for [opencode](https://github.com/opencode-ai/opencode).

teamx turns opencode into a team workspace. Multiple opencode sessions join a shared team, each with a role and a goal, and collaborate through a persistent event ledger until the goal is achieved.

```
┌──────────────┐      ┌──────────────┐      ┌──────────────┐
│  opencode    │      │  teamx CLI   │      │  SQLite DB   │
│  (plugin)    │─────▶│  (Rust)      │─────▶│  (ledger)    │
└──────────────┘      └──────────────┘      └──────────────┘
       │                     │
       │ mTLS                │ axum
       ▼                     ▼
┌──────────────┐      ┌──────────────┐
│  member 2..N │─────▶│ teamx serve  │
└──────────────┘      └──────────────┘
```

## Features

- **Goal lifecycle** — `proposed → shared → in_progress → achieved → closed` with owner-driven transitions
- **Role system** — built-in roles (owner, contributor, reviewer, ...) plus user-proposed custom roles
- **Invitation letters** — owner issues mTLS client certificates bundled into one-time invitation letters; members import and join with cryptographic identity
- **Network mode** — `teamx serve` runs an mTLS HTTP server with WebSocket push; members on the same LAN collaborate in real time
- **Auto-execute** — directed tasks (`publish --assignee`) automatically wake the assigned member's session
- **30+ tools** — full lifecycle exposed as opencode tools and `/team` slash commands with tab completion
- **loopx bridge** — optional integration with [loopx](https://github.com/clawparty-ai/loopx) for stage-progress snapshots

## Quick Start

```bash
# Install (builds Rust CLI + opencode plugin)
./install.sh

# Restart opencode, then:
/team create "My Team"          # You become the owner
/team goal set "Ship feature X" # Draft a goal
/team goal share                # Share goal → team becomes active
/team invite "contributor: builds features"  # Issue invitation letter

# On member's machine (or second opencode session):
/team import <letter>           # Import invitation, get mTLS cert
# Owner approves:
/team approve <member_id>

# Member works:
/team publish progress --data '{"message":"implemented auth"}'
/team publish achieved --data '{}'

# Owner verifies:
/team goal close
```

## Network Mode

For LAN collaboration (multiple machines):

```bash
# Owner machine:
/team serve start               # Start mTLS server on :5781
/team invite "reviewer: reviews code" --server-url https://172.20.10.3:5781

# Member machine:
/team import <letter>           # Import invitation
# Set env for real-time push:
export TEAMX_SERVER_URL=https://172.20.10.3:5781
export TEAMX_MTLS_CERT=~/.teamx/letters/<id>/client.crt
export TEAMX_MTLS_KEY=~/.teamx/letters/<id>/client.key
export TEAMX_MTLS_CA=~/.teamx/letters/<id>/ca.crt
```

See [docs/network-mode.md](docs/network-mode.md) for the full design and [docs/manual-test-network.md](docs/manual-test-network.md) for the manual test runbook.

## Project Structure

```
crates/teamx/           Rust CLI (SQLite event ledger + state machine + mTLS server)
opencode-plugin/        opencode plugin (30+ tools + /team agent + slash commands)
  src/index.ts          Plugin entry, event handling, auto-execute
  src/tools.ts          Tool definitions
  src/client.ts         CLI/RPC client with mTLS transport
  src/ws.ts             WebSocket client (push, reconnect)
  src/serve.ts          Server lifecycle management
  assets/agent/         Agent routing instructions (teamx.md)
  assets/command/       Slash command files (/team create, /team invite, ...)
install.sh              One-click install / --uninstall
tests/                  run-all.sh (9-step automated suite)
docs/                   Design docs, manual test runbooks, specs
```

## Configuration

| Variable | Default | Description |
|---|---|---|
| `TEAMX_DB` | `~/.teamx/teamx.db` | SQLite database path |
| `TEAMX_SERVER_URL` | — | Network mode server URL (enables WebSocket push) |
| `TEAMX_MTLS_CERT` | auto-discovered | mTLS client certificate (PEM) |
| `TEAMX_MTLS_KEY` | auto-discovered | mTLS client key (PEM) |
| `TEAMX_MTLS_CA` | auto-discovered | mTLS CA certificate (PEM) |
| `TEAMX_POLL_INTERVAL` | `15000` | Polling interval in ms (0 = disabled when WS connected) |
| `TEAMX_WS_HEARTBEAT_SECS` | `30` | WebSocket heartbeat interval |
| `TEAMX_BIN` | `teamx` | CLI executable name |

## Testing

```bash
./tests/run-all.sh    # Full automated suite (9 steps)
```

The suite runs: unit tests, CLI edge cases, mTLS identity + revocation, WebSocket push + reconnect, cross-network LAN verification, and plugin unit tests.

Manual test runbooks:
- [Two-person workflow](docs/demo.md)
- [Three-person workflow](docs/demo-3p.md)
- [Network mode](docs/manual-test-network.md)

## Security Model (V1)

V1 has **no real authentication**. Session keys are self-reported, invitation tokens are visible to all team members. This is a "trust this machine" collaboration convention — owner approval and roles are collaboration semantics, not security boundaries.

See [goal-v1.md](goal-v1.md) for the trust model and [docs/v2-design.md](docs/v2-design.md) for the planned V2 with real auth.

## Tech Stack

- **CLI**: Rust (axum + tokio-rustls + rusqlite + rcgen)
- **Plugin**: TypeScript (opencode plugin API)
- **Storage**: SQLite WAL
- **Transport**: mTLS (ring + x509-parser), WebSocket (axum ws)

## License

MIT
