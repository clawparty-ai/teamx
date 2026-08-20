# @teamx/dsh-plugin

teamx plugin for [deepseek-harness (dsh)](https://github.com/deepseek-ai/deepseek-harness) — shared-goal human-in-the-loop collaboration across multiple dsh agents.

## What it does

- **28 `teamx_*` tools** registered via `ctx.tools.register(defineTool(...))` for team management, goal tracking, member approval, role assignment, and real-time collaboration
- **21 `/team-*` flat slash commands** registered via `ctx.commands.register`
- **Per-agent digest injection** via `ctx.systemPrompt.variable` — each agent sees live team state in its system prompt
- **Auto-execute**: directed tasks wake the target agent via `agent.followup()`
- **Real-time push**: WebSocket + poll-based digest refresh (configurable via `pollIntervalMs`)
- **Network mode**: mTLS HTTP RPC + WS push when `TEAMX_SERVER_URL` is set

## Architecture

```
dsh-plugin/
├── src/
│   ├── index.ts          # Plugin entry: Cordis apply(), event hooks, poller, auto-execute, digest
│   ├── client.ts         # sessionKey/instanceId/runCli(binary)/runRpc(mTLS)/mtlsFor/member cache
│   ├── tools.ts          # teamx_* tools → ctx.tools.register(defineTool(...))
│   ├── commands.ts       # /team-* flat slash commands → ctx.commands.register
│   ├── ws.ts             # WebSocket push client (Node ws package, mTLS, reconnect)
│   ├── digest.ts         # Per-agent digest cache + sync refresh + formatting
│   ├── auto-execute.ts   # Directed task detection → agent.followup()
│   └── i18n.ts           # Message strings
├── package.json
├── tsconfig.json
└── README.md
```

## Installation

The plugin loads via dsh's cordis plugin system. Add to your dsh profile's `cordis.patch.yml`:

```yaml
- id: teamx
  name: '@teamx/dsh-plugin'
```

For development, use a `file:` dependency in your dsh profile:

```json
{
  "dependencies": {
    "@teamx/dsh-plugin": "file:/path/to/teamx/dsh-plugin"
  }
}
```

## Session identity

Each dsh agent gets a unique key: `${teamxInstance}:${agentSessionID}`

- `teamxInstance` = UUID from `~/.teamx/instance.json`
- `agentSessionID` = dsh's `agent.session.id`

This key is the agent's identity across all teamx operations. **dsh agents and opencode sessions can join the same team** — they share the same session key format.

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

## Tools (28)

### Team management
- `teamx_create_team` — Create team, get invite token
- `teamx_join` — Join team via invite token
- `teamx_leave` — Leave current team
- `teamx_list_teams` — List teams with goal/role overview
- `teamx_status` — Full team status (members, goal, events)
- `teamx_sync` — Pull latest state + new events
- `teamx_archive` — Archive completed team (owner)
- `teamx_team_destroy` — Destroy team permanently (owner)
- `teamx_team_invite` — Generate invite with role
- `teamx_team_import` — Import invite letter
- `teamx_team_invite_list` / `teamx_team_invite_revoke` — Manage invites

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

## Slash commands (21)

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
| `/team-help` | Show available commands |

## Digest injection

The plugin injects a live team digest into each agent's system prompt via `ctx.systemPrompt.variable('teamx_digest', ...)`. The digest includes:
- Team name and status
- Current goal and its state
- Member list with roles and states
- Recent events (last 3)
- Open questions

Each agent gets its own isolated digest (via `agent.ctx.systemPrompt`). Refreshed every `pollIntervalMs` ms (default 15s) via poller or WebSocket push.

## Auto-execute

When a task is published with `--assignee <member>`, the plugin detects it and calls `agent.followup(message)` to wake the target agent. The agent receives a structured message with the task details and digest, then starts working automatically.

Disable with `TEAMX_AUTO_EXECUTE=0`.

## Cordis plugin lifecycle

```
apply(ctx, config)
  ├── ctx.on('ready')
  │   ├── Discover teamx instance ID
  │   ├── Register 28 teamx_* tools
  │   └── Register 21 /team-* commands
  │
  ├── ctx.on('agent/session-start')
  │   ├── Check team membership → markMember()
  │   ├── Register systemPrompt.variable('teamx_digest')
  │   ├── Register systemPrompt.section('teamx:digest')
  │   └── Initial digest refresh
  │
  ├── ctx.on('agent/status')
  │   └── On idle: publish heartbeat + refresh digest
  │
  ├── ctx.on('agent/dispose')
  │   └── Unregister agent, clear digest
  │
  ├── Poller (every pollIntervalMs)
  │   ├── Fetch events for known sessions
  │   ├── Process auto-execute for directed tasks
  │   └── Refresh digest
  │
  └── WS push (when TEAMX_SERVER_URL set)
      ├── Connect to /ws endpoint (mTLS)
      └── On event: refresh digest + process auto-execute
```

## Network mode

When `TEAMX_SERVER_URL` is set (or discovered from an imported letter), the plugin switches from local binary execution to HTTP mTLS RPC:

1. **Local mode**: spawns `teamx` binary via `child_process.execFile` for each operation
2. **Network mode**: POST to `https://server/rpc` with mTLS client cert (Node `https` module)

WebSocket push provides real-time event notifications via the `ws` npm package (supports mTLS client certs). Both modes support the full feature set.

## Multi-agent collaboration

The key feature: multiple dsh agents can collaborate on the same team goal, just like multiple opencode sessions.

```
Agent A (owner)                    Agent B (contributor)
    │                                   │
    ├── teamx_create_team ──────────►    │
    ├── teamx_set_goal ─────────────►    │
    ├── teamx_share_goal ───────────►    │
    ├── teamx_invite(contributor) ──►    │
    │                                   ├── teamx_join(token)
    │   ◄──── teamx_approve ────────────┤
    │                                   │
    ├── teamx_publish(progress, ─────►   │
    │      assignee=B)                   │
    │                                   ├── [auto-execute: followup()]
    │                                   ├── teamx_sync()
    │                                   ├── [work on task]
    │                                   ├── teamx_publish(achieved)
    │   ◄──── teamx_sync ───────────────┤
    ├── teamx_close_goal ───────────►    │
```

Session key format `${teamxInstance}:${agentSessionID}` is shared with opencode-plugin, so **dsh agents and opencode sessions can join the same team**.

## Differences from opencode-plugin

| Feature | opencode-plugin | dsh-plugin |
|---------|----------------|------------|
| Runtime | Bun | Node 22+ |
| Tool registration | `ctx.client.tool(...)` | `ctx.tools.register(defineTool(...))` |
| Command registration | Markdown files in assets/ | `ctx.commands.register(...)` |
| System prompt injection | `experimental.chat.system.transform` | `ctx.systemPrompt.variable()` + `.section()` |
| Auto-execute | `session.promptAsync()` | `agent.followup()` |
| WS client | Bun WebSocket | `ws` npm package |
| Activity collection | Yes (enterprise) | No (V1 core only) |
| Tunnel/proxy tools | Yes | No (V1 core only) |

## Dependencies

- `@deepseek-ai/cordis` — Plugin framework
- `@deepseek-ai/dsh-tools` — Tool registration (`defineTool`)
- `@deepseek-ai/dsh-agent` — Agent interface (`agent.followup()`)
- `@deepseek-ai/dsh-session` — Session events
- `@deepseek-ai/dsh-system-prompt` — System prompt injection
- `@deepseek-ai/dsh-commands` — Slash command registration
- `ws` — WebSocket client (mTLS support)

## Development

```bash
cd dsh-plugin
npm install        # installs dsh packages via file: references
npx tsc --noEmit   # type-check (noCheck: true for unbuilt dsh packages)
```

The dsh packages must be built first for full type checking:
```bash
cd /path/to/deepseek-harness
pnpm install && pnpm build
```

## Testing

Tests are in `tests/` (to be implemented). The test plan covers:
1. `client.ts` unit tests (sessionKey, runCli mock, member cache)
2. `tools.ts` integration (spawn real binary, verify CLI args)
3. Multi-agent collaboration loop (owner + member, full lifecycle)
4. Auto-execute trigger (directed task → followup called)
