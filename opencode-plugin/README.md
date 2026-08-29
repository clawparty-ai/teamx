# @teamx-ai/opencode-plugin

teamx plugin for [opencode](https://github.com/opencode-ai/opencode) — shared-goal human-in-the-loop collaboration across multiple opencode sessions.

## What it does

- **37+ `teamx_*` tools** for team management, goal tracking, member approval, role assignment, tunneling, and real-time collaboration
- **20+ `/team-*` slash commands** for quick access without leaving the chat
- **`/team-grill` design sessions** for owner-led questions, evidence gathering, and durable decision records
- **Per-session digest injection** via `experimental.chat.system.transform` — the model sees live team state
- **Auto-execute**: directed tasks wake the target session and start working automatically
- **Real-time push**: WebSocket + poll-based digest refresh (configurable via `TEAMX_POLL_INTERVAL`)
- **Network mode**: mTLS HTTP RPC + WS push when `TEAMX_SERVER_URL` is set
- **Activity collection**: work sessions, human activity, tool calls, file changes — stored in SQLite and queryable via Enterprise dashboard

## Architecture

```
opencode-plugin/
├── src/
│   ├── index.ts          # Plugin entry: event hooks, poller, auto-execute, digest injection
│   ├── client.ts         # sessionKey/instanceId/runCli(binary)/runRpc(mTLS)/mtlsFor/member cache
│   ├── tools.ts          # teamx_* tools → ctx.client.tool(...)
│   ├── tunnel.ts         # Tunnel commands (expose/forward/status/close)
│   ├── tunnels-store.ts  # Local tunnel state
│   ├── serve.ts          # Network-mode server management (teamx serve)
│   ├── ws.ts             # WebSocket push client (mTLS, reconnect)
│   ├── activity.ts       # Event → activity row mapping
│   └── i18n/             # English/Chinese message strings
├── assets/
│   ├── agent/            # teamx.md — agent instructions
│   └── command/          # /team-* command markdown files (en + zh)
├── package.json
└── README.md
```

## Installation

The plugin is published to npm as `@teamx-ai/opencode-plugin`. Add it to opencode's `plugin` array (opencode resolves npm specs itself — no manual `npm install` needed):

```json
{
  "plugin": ["@teamx-ai/opencode-plugin"]
}
```

Pin a version if you want: `"plugin": ["@teamx-ai/opencode-plugin@0.1.0"]`. After editing `opencode.json`, restart opencode.

> Older opencode builds that don't resolve npm specs can install manually:
> ```bash
> npm install @teamx-ai/opencode-plugin
> # then reference the bundled entry:
> "plugin": ["node_modules/@teamx-ai/opencode-plugin/dist/teamx.js"]
> ```

## Session identity

Each opencode session gets a unique key: `${teamxInstance}:${sessionID}`

- `teamxInstance` = UUID from `~/.teamx/instance.json`
- `sessionID` = opencode's internal session ID

This key is the session's identity across all teamx operations. Multiple opencode sessions (on the same machine or different machines) can join the same team.

## Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `TEAMX_HOME` | `~/.teamx` | teamx data directory |
| `TEAMX_SERVER_URL` | (auto) | Network-mode server URL (from letter or env) |
| `TEAMX_POLL_INTERVAL` | `15000` | Digest refresh interval (ms); 0 disables |
| `TEAMX_AUTO_EXECUTE` | `1` | Enable auto-execute for directed tasks |
| `TEAMX_MTLS_CERT` | — | mTLS client certificate path |
| `TEAMX_MTLS_KEY` | — | mTLS client key path |
| `TEAMX_MTLS_CA` | — | mTLS CA certificate path |
| `TEAMX_BIN` | `teamx` | teamx binary path |

## Tools (37+)

### Team management
- `teamx_create_team` — Create team, get invite token
- `teamx_join` — Join team via invite token
- `teamx_leave` — Leave current team
- `teamx_list_teams` — List teams with goal/role overview
- `teamx_status` — Full team status (members, goal, events)
- `teamx_sync` — Pull latest state + new events
- `teamx_archive` — Archive completed team (owner)
- `teamx_team_destroy` — Destroy team permanently (owner)

### Goal management
- `teamx_set_goal` — Set/update team goal (owner)
- `teamx_share_goal` — Share goal, activate team (owner)
- `teamx_close_goal` — Close achieved goal (owner)

### Member/role management
- `teamx_approve` / `teamx_deny` — Approve/deny pending members
- `teamx_set_role` — Choose or assign role
- `teamx_role_propose` — Propose custom role
- `teamx_role_approve` / `teamx_role_deny` — Approve/deny custom role
- `teamx_role_update` — Update role label/description
- `teamx_set_state` — Set idle/active state

### Interaction
- `teamx_ask` — Ask a member a question
- `teamx_respond` — Answer an open question
- `teamx_publish` — Publish event to team ledger

### Network/tunnel
- `teamx_team_invite` — Generate invite with role
- `teamx_team_import` — Import invite letter
- `teamx_team_invite_list` / `teamx_team_invite_revoke` — Manage invites
- `teamx_tunnel_expose` / `teamx_tunnel_forward` — Tunnel management
- `teamx_tunnel_list` / `teamx_tunnel_status` / `teamx_tunnel_close`
- `teamx_serve_start` / `teamx_serve_status` / `teamx_serve_stop` — Server management
- `teamx_serve_token` — Generate member connection token

### Git repository management
- `teamx_git_setup` — Configure stock `git` for mTLS from the invitation letter (then use plain `git clone/pull/push`)
- `teamx_git_create` — Create a new git repository (owner/admin)
- `teamx_git_delete` — Delete a git repository (owner/admin)
- `teamx_git_list` — List accessible repositories
- `teamx_git_clone` — Clone a repository to local machine
- `teamx_git_pull` — Pull changes from remote repository
- `teamx_git_push` — Push changes to remote repository
- `teamx_git_commit` — Commit local changes (git add -A + commit)
- `teamx_git_commit_push` — Commit local changes then push
- `teamx_git_grant` — Grant a member repository access (read/write/admin)
- `teamx_git_permissions` — Show repository access permissions

## Slash commands (20+)

| Command | Description |
|---------|-------------|
| `/team-create <name>` | Create a new team |
| `/team-join <token> <name>` | Join team via invite token |
| `/team-status` | Show team status |
| `/team-sync` | Pull latest state |
| `/team-goal-set <title>` | Set team goal |
| `/team-goal-share` | Share goal with members |
| `/team-goal-close` | Close achieved goal |
| `/team-approve <member>` | Approve pending member |
| `/team-deny <member>` | Deny pending member |
| `/team-invite <role>` | Generate invite |
| `/team-import <path>` | Import invite letter |
| `/team-publish <type>` | Publish event |
| `/team-role-set <role>` | Set/assign role |
| `/team-state <idle\|active>` | Set working state |
| `/team-ask <member> <msg>` | Ask a question |
| `/team-respond <id> <msg>` | Answer a question |
| `/team-grill <topic>` | Start an owner-led design session (`--doc` and `--resume` supported) |
| `/team-serve-start` | Start network server |
| `/team-serve-stop` | Stop network server |
| `/team-tunnel-expose` | Expose local port |
| `/team-tunnel-forward` | Forward teammate's tunnel |
| `/team-git-create <name>` | Create git repository |
| `/team-git-delete <name>` | Delete git repository |
| `/team-git-list` | List git repositories |
| `/team-git-clone <repo>` | Clone repository |
| `/team-git-pull <repo>` | Pull repository changes |
| `/team-git-push <repo>` | Push repository changes |
| `/team-git-commit -m <msg>` | Commit local changes |
| `/team-git-commit-push -m <msg>` | Commit and push in one step |
| `/team-git-grant <name> <member>` | Grant repository access |
| `/team-git-permissions <name>` | Show repository permissions |
| `/team-help` | Show available commands |

For starting, resuming, delegating fact gathering, and completing a design session, see the [Grill with Docs usage guide](../docs/23-manual-grill-with-docs-usage.md).

## Digest injection

The plugin injects a live team digest into the system prompt via `experimental.chat.system.transform`. The digest includes:
- Team name and status
- Current goal and its state
- Member list with roles and states
- Recent events (last 5)
- Open questions

Refreshed every `TEAMX_POLL_INTERVAL` ms (default 15s) via poller or WebSocket push.

## Auto-execute

When a task is published with `--assignee <member>`, the plugin detects it and calls `session.promptAsync()` to wake the target session. Disable with `TEAMX_AUTO_EXECUTE=0`.

## Network mode

When `TEAMX_SERVER_URL` is set (or discovered from an imported letter), the plugin switches from local binary execution to HTTP mTLS RPC:

1. **Local mode**: spawns `teamx` binary for each operation
2. **Network mode**: POST to `https://server/rpc` with mTLS client cert

WebSocket push provides real-time event notifications. Both modes support the full feature set.

## Git over mTLS (standard `git` protocol)

The teamx server also speaks the **standard Git Smart HTTP protocol** over the same mTLS channel, so you can use a stock `git` client (no `teamx git` wrapper needed):

```bash
# 1. Configure stock git to use your invitation letter's client cert (once):
teamx git setup --server https://server

# 2. Use plain git against the server (cert paths are picked up automatically):
git clone https://server/git/<team_id>/<repo>
git pull
git push
```

- `git setup` reads `client.crt` / `client.key` / `ca.crt` from the member's private directory (`~/.teamx/letters/<id>/`) and writes per-URL config into `~/.gitconfig` (`http.<server>/.sslCert`, `.sslKey`, `.sslCAInfo`).
- Server auth is the same mTLS as the RPC channel: the client cert CN → member identity.
- Permissions: `read` grants clone/pull/fetch; `write` grants push.
- The `teamx git clone/pull/push` wrapper commands remain available as an alternative.

## Activity collection

The plugin tracks:
- **Work sessions**: idle→busy→idle segments with duration
- **Human activity**: user inputs, approvals, commands within work sessions
- **Tool calls**: which teamx tools are called and when
- **File changes**: which files are modified during work sessions

Activity rows are stored in `~/.teamx/activity.db` and queryable via the Enterprise dashboard (`teamx ui`).

## Building

```bash
cd opencode-plugin
bun install
bun run build    # builds to dist/teamx.js
bun run typecheck
bun run check:protocols  # verifies generated design-session adapters are current
```

`/team-grill` is generated from the repository's host-neutral `protocols/grill-with-docs.md`. Edit that source and run `bun run generate:protocols`; do not edit the generated command assets directly.
