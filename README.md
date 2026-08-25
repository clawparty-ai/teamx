# teamx

> Shared-goal team collaboration for [opencode](https://github.com/opencode-ai/opencode). Humans in the loop.

teamx turns opencode into a **human-led team workspace**. The owner shares one goal with the team, and every member works toward it — often in their own way, on their own implementation.

This is different from the common "multi-agent" model. Instead of decomposing a task into subtasks and handing each to an isolated agent, teamx keeps **humans in the loop** and embraces a **shared-goal** model:

- Every team member is a human with an opencode session (an AI collaborator) at their side.
- Everyone sees the same goal; members are not pre-assigned disjoint subtasks — they may work the same goal from different angles.
- The owner stays in the loop: approving membership, clarifying direction, reviewing progress, and verifying the goal before closing it.

State is shared through a persistent event ledger until the goal is achieved.

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

## Concept: Idempotent delivery

A common pain in enterprise software delivery: **documentation, code, and delivery drift apart**. Management tools try to force consistency, but execution deviates in practice — and front-line engineers end up feeling that "the management overhead is just extra work."

teamx's team mode flips this. Requirements, design, prototypes, tests, and documentation all run under one automated execution control, so delivery becomes **idempotent**: change the requirement document, and teamx automatically drives every stage — design, review, test plans, development, documentation — to a new, consistent delivery, until the goal is achieved.

Humans stay in the loop only where judgment matters: approving membership, resolving review conflicts, and verifying the goal before closing it. The details (documents, communication, code) are handled by AI. Automation plus a strict review gate delivers quality close to — or better than — a human-only process, while AI absorbs the coordination work that used to burn engineering hours.

The net effect: **the same headcount delivers more.** Less human effort per delivery, higher consistency, and teams ship more features per unit of people.

See the worked example: [`templates/01-product-dev-team.TEAM.md`](templates/01-product-dev-team.TEAM.md) — a four-role product team (PM / UI-Dev / Java-Dev / Tester) that enforces three iron rules (design-first, mandatory review, test-first) so every iteration delivers requirements, prototypes, dev docs, and test plans in lockstep.

## Features

- **Goal lifecycle** — `proposed → shared → in_progress → achieved → closed` with owner-driven transitions
- **Role system** — built-in roles (owner, contributor, reviewer, ...) plus user-proposed custom roles
- **Invitation letters** — owner issues mTLS client certificates bundled into one-time invitation letters; members import and join with cryptographic identity
- **Network mode** — `teamx serve` runs an mTLS HTTP server with WebSocket push; members collaborate in real time over LAN **or the public internet**
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

`teamx serve` is a self-hosted mTLS server. Because every member authenticates with a client certificate (mTLS) and all traffic is encrypted, it works both on a LAN and on the **public internet** — e.g. a VPS or a home server behind a forwarded port.

```bash
# Owner machine:
/team serve start               # Start mTLS server on :5781
/team invite "reviewer: reviews code" --server-url https://teamx.example.com:5781

# Member machine (anywhere in the world):
/team import <letter>           # Import invitation
# Set env for real-time push:
export TEAMX_SERVER_URL=https://teamx.example.com:5781
export TEAMX_MTLS_CERT=~/.teamx/letters/<id>/client.crt
export TEAMX_MTLS_KEY=~/.teamx/letters/<id>/client.key
export TEAMX_MTLS_CA=~/.teamx/letters/<id>/ca.crt
```

The server binds with a certificate that covers its hostname/IP (use `--san <hostname|ip>` or the plugin's auto-detected IP), and members verify it against the team CA bundled in their invitation letter. See [docs/network-mode.md](docs/network-mode.md) for the full design and [docs/manual-test-network.md](docs/manual-test-network.md) for the manual test runbook.

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

The suite runs: unit tests, CLI edge cases, mTLS identity + revocation, WebSocket push + reconnect, cross-network verification, and plugin unit tests.

Manual test runbooks:
- [Two-person workflow](docs/demo.md)
- [Three-person workflow](docs/demo-3p.md)
- [Network mode](docs/manual-test-network.md)

## Security Model

teamx uses **mTLS (mutual TLS)** for network-mode authentication and encryption — the same mechanism used by service meshes and enterprise VPNs, strong enough to run over the public internet:

- **Identity**: every member holds a client certificate issued by the team's CA, with the member id and role embedded in the certificate CN (`member:<id>:<role>`). RPC handlers derive the actor's identity from the client certificate CN — no self-reported session keys.
- **Encryption**: all traffic between members and the server is encrypted with TLS 1.2/1.3.
- **Invitation letters**: the owner issues one-time invitation letters containing the client certificate + key; a member imports the letter to obtain their identity.
- **Revocation**: `team invite-revoke` invalidates a certificate immediately — revoked members are rejected at connect and disconnected from active WebSocket connections.
- **Authorization model**: certificate = "can connect", owner approval = "can work". Pending members can connect but cannot publish or act until the owner approves.
- **Cross-team isolation**: network RPC checks that a certificate holder belongs to the team being accessed; non-members cannot read other teams' invite tokens, members, roles, or events.

Local (single-machine, CLI-only) mode relies on a self-reported `session_key`, which is acceptable only on a trusted machine. See [goal-v1.md](docs/goal-v1.md) for the trust model.

## Tech Stack

- **CLI**: Rust (axum + tokio-rustls + rusqlite + rcgen)
- **Plugin**: TypeScript (opencode plugin API)
- **Storage**: SQLite WAL
- **Transport**: mTLS (ring + x509-parser), WebSocket (axum ws)

## License

MIT
