# teamx V1 Goal

`teamx` is a team-collaboration state kernel + opencode plugin: it records the persisted state and behavioral history of team/member/goal (a state machine, modeled on loopx), and through a `/Team` agent inside opencode lets users interact with the team and sync progress in real time until the team goal is achieved.

---

## Confirmed Decisions

| # | Decision | How It Lands |
|---|---|---|
| 1 | V1 single-machine, local | All opencode + teamx instances on one machine; cross-network comes later in V2 |
| 2 | User-driven + reuse loopx | Don't reinvent loopx; teamx only does thin bridging: read loopx stage progress → publish as team events → sync to team lead |
| 3 | Global storage | Single DB: `~/.teamx/teamx.db` |
| 4 | Member identity | At join time users name themselves (display name); one opencode session joining = one team member; multiple sessions joining = multiple members; not joining means not a member |
| 5 | Join admission | User initiates the join; owner approves/denies (approve / deny) |
| 6 | Standalone repo | `~/github/teamx` |
| 7 | No server in V1 | Do not start `teamx serve`; plugins spawn `teamx` CLI subprocesses directly; `serve`/SSE deferred to V2 |

> ## ⚠️ V1 Trust Model (Important Positioning)
>
> V1 **has no real authentication** — it is a "trust this machine" collaboration convention, not a permission system:
> - The `session_key` is a **caller-declared string** (`--session <key>`); the CLI only does table lookups and no authenticity verification — any local process can impersonate any member with an arbitrary session_key.
> - The `invite_token` and team info are **visible to all members** (`team list` / `team status` both return the token).
> - Therefore "owner approval / roles" are **collaboration semantics and state records**, not security boundaries.
> - Real authentication (token issuance/verification, member credentials) is deferred to the V2 registration/push channel (see `docs/v2-design.md`).

---

## Architecture

```
┌────────────── opencode (owner session) ──────────────┐
│  /Team → teamx agent                                 │
│  plugin: tool: teamx_* + event hook                  │
└─────────────────────┬─────────────────────────────┘
                      │ spawn `teamx <cmd> --db ~/.teamx/teamx.db --json`
┌─────────────────────┴─────────────────────────────┐
│        teamx CLI (Rust, SQLite WAL single writer) │
│   event ledger(append-only, per-team seq) → state machine projections │
│   SQLite: teams / members / goals / roles / events │
└─────────────────────┬─────────────────────────────┘
┌─────────────────────┴─────────────────────────────┐
│  opencode (member session 1)     opencode (member session 2) │
│  /Team → teamx agent             /Team → teamx agent     │
│  plugin: teamx_* tools           plugin: teamx_* tools    │
└─────────────────────────────────────────────────────┘
```

Core mechanism (aligned with the loopx control-plane philosophy): every state change lands as an append-only event; the current Team / Member / Goal states are projections derived from the event ledger. Each participant reads state and writes events only through the CLI; who wrote what remains auditable and replayable.

### Two Implementation Constraints

1. **Each team's `seq` increment must be in the same transaction as the event INSERT**, to avoid out-of-order sequence numbers under concurrency; write conflicts are handled with `busy_timeout` + simple retry.
2. The plugin side wraps a unified invocation layer: fixed `teamx <cmd> --db ~/.teamx/teamx.db --json` (or use the `TEAMX_DB` env var); **when V2 switches to an HTTP client, only this layer needs replacing**.

---

## Repo Layout `~/github/teamx`

```
teamx/
├── Cargo.toml                    # cargo workspace
├── crates/teamx/                 # Rust CLI
│   ├── src/main.rs
│   ├── src/cli.rs                # clap subcommands
│   ├── src/db.rs                 # SQLite (WAL) + schema
│   ├── src/state.rs              # state machine definitions + legal transition validation
│   ├── src/events.rs             # append-only event ledger + projections
│   └── src/loopx.rs              # loopx bridge
├── opencode-plugin/
│   ├── package.json              # depends on @opencode-ai/plugin
│   ├── src/index.ts              # Plugin fn (tool: + event hook)
│   ├── src/tools.ts              # teamx_* tool implementations
│   ├── src/client.ts             # unified CLI invocation layer (swappable for HTTP in V2)
│   └── assets/
│       ├── agent/teamx.md
│       └── command/Team.md       # /Team command → teamx agent
├── install.sh                    # cargo build → ~/.local/bin/teamx; installs agent/command/plugin into opencode config
├── tests/                        # integration smoke tests (dual-session closed loop)
└── docs/
    ├── v1-spec.md
    └── loopx-bridge.md
```

---

## Rust CLI Specification

**Dependencies**: `clap`, `rusqlite`(bundled, WAL), `serde`/`serde_json`, `dirs`. V1 contains no HTTP/SSE.

### State Machine

- Team: `forming → active → blocked → completed → archived`
- Member: `pending → active → waiting → idle → left` (owner asking a question or member asking sets `waiting`; cleared after response)
- Goal: `proposed → shared → refining → in_progress → blocked → achieved → closed` (`achieved` is "achievement candidate"; the owner can reopen it back to `in_progress` via `publish start`/`resumed`, or send it back to `refining` with `refine`; only `close` reaches the terminal `closed` state)

Every transition is recorded in the event ledger; current state = projection.

### Schema

- `teams(id, name, owner_member_id, goal_id, state, invite_token, created_at, updated_at)`
- `members(id, team_id, session_key, display_name, role, state, loopx_project, last_seen_at, joined_at, left_at)`
- `goals(id, team_id, title, body, state, created_at, updated_at)`
- `roles(id, team_id, key, label, description, permissions_json)`
- `events(id, team_id, member_id, seq, type, payload_json, created_at)` ← core ledger
- `sessions(session_key, team_id, member_id, created_at)` ← opencode session ↔ member mapping

`session_key = <instance UUID>:<sessionID>`

### Command Surface (V1)

- `teamx init`: create the global DB
- `teamx team create <name>` → returns `invite_token`
- `teamx team join <token> --name <name> [--loopx-project <dir>]` → creates a pending membership
- `teamx team approve <member_id>` / `teamx team deny <member_id>` (owner)
- `teamx team list` / `teamx team leave` / `teamx team status` / `teamx team archive` (owner, completed→archived)
- `teamx member set-state <idle|active>` (self-service; owner may set on behalf with `--member`)
- `teamx goal set <title> --body <text>` (owner) / `teamx goal share` (owner broadcast) / `teamx goal close` (owner verifies completion)
- `teamx role list` / `teamx role set <role>` (members choose their own role; owner may also assign)
- `teamx publish <type> --data <json>` (progress / ask / decision / update and other generic events)
- `teamx ask <member_id> --question <text>` (asks, puts member in waiting)
- `teamx respond <ask_id> --answer <text>` (responds, clears waiting)
- `teamx events [--after <seq>]` / `teamx sync` (pull latest state + incremental events, compact summary output)
- `teamx loopx report --project <dir>` (loopx stage-progress snapshot)

### Default Role Catalog

`owner / observer / supervisor / contributor / subtask-implementer / reviewer`; role = `{key, label, description, permissions}` (V1 permissions are advisory only, not enforced).

---

## loopx Bridge (Don't Reinvent the Wheel)

- Members may optionally bind `loopx_project` at join time.
- `teamx loopx report --project <dir>` → runs `loopx status --format json`, extracts `active_goal_state / current gate / next todo / quota`, compresses into a compact summary → publishes a `loopx.progress` event.
- The plugin-side tool `teamx_loopx_report` does it in one click; the owner's teamx agent sees each member's loopx stage progress each round of `teamx_sync` and broadcasts accordingly.
- When loopx isn't installed/connected, a clear message is returned without breaking teamx's own closed loop.
- V1 only does "read `loopx status` on demand", no file watching.

---

## opencode Plugin Specification (Aligned with the opencode v1.17.x API)

The install script (`install.sh`) writes three pieces to disk (read at startup; takes effect after restart):

- `~/.config/opencode/agent/teamx.md`: frontmatter `mode: all` + collaboration system prompt
- `~/.config/opencode/command/Team.md`: frontmatter `agent: teamx` → `/Team` appears in `/` autocomplete and routes to the teamx agent
- `~/.config/opencode/plugins/teamx.ts`: the plugin itself (`@opencode-ai/plugin`)

Plugin responsibilities:
- `tool:` registers `teamx_*` tools; `sessionID` comes from ToolContext; member binding is lazy (the session registers itself on first tool call)
- `event` hook: converts this session's `message.updated` / `session.idle` into lightweight member activity events published to teamx (the owner can see "when members are working" even without members proactively reporting)
- `client.app.log()` structured logging
- Tool names = object keys, uniformly prefixed with `teamx_`

### V1 Tool Set (17 Tools)

`teamx_create_team` `teamx_set_goal` `teamx_share_goal` `teamx_close_goal` `teamx_archive` `teamx_list_teams` `teamx_join` `teamx_approve` `teamx_deny` `teamx_set_role` `teamx_set_state` `teamx_status` `teamx_sync` `teamx_publish` `teamx_ask` `teamx_respond` `teamx_loopx_report`

---

## Reporting/Broadcast Protocol (Encoded into the teamx Agent System Prompt)

- **member**: before each significant action or upon making progress, run `teamx_sync` first to check for new instructions, then `teamx_publish progress/ask` to report to the owner.
- **owner**: at the start of each turn, run `teamx_sync` to aggregate member reports → when needed, `teamx_publish decision/broadcast` to broadcast clarifications, adjustments, goal progress.
- Open questions pass explicitly through `teamx_ask` / `teamx_respond`, putting members into the `waiting` state.
- Event types: `team.created` `team.joined` `membership.pending` `membership.approved` `membership.denied` `member.role_set` `member.state_changed` `goal.set` `goal.shared` `goal.state_changed` `progress.published` `clarification.asked` `clarification.responded` `loopx.progress` `decision.broadcast` `goal.achieved` `team.completed`

---

## Workflow Closed Loop (V1 Acceptance Scenario)

1. Session A `/Team` → `teamx_create_team` → owner; `teamx_set_goal` drafts the goal.
2. Session B `/Team` → `teamx_join <token> --name Bob` → pending; owner runs `teamx_approve`.
3. Bob runs `teamx_set_role contributor`; owner runs `teamx_share_goal`.
4. Bob works, using loopx to manage long tasks; `teamx_loopx_report` publishes stage progress.
5. Owner's `teamx_sync` sees it → `teamx_publish decision` broadcasts clarifications/progress; Bob runs `teamx_ask` → owner runs `teamx_respond`.
6. Bob sends `teamx_publish goal_achieved` candidate → owner verifies with `teamx_close_goal` → team `completed`.

---

## M1 Milestone (Scope of This Plan) ✅ Completed + Productionized

1. `~/github/teamx` repo skeleton (cargo workspace + `opencode-plugin` + `install.sh` + `tests/`)
2. Rust CLI: schema / state machine / event ledger + all subcommands (including archive / member set-state)
3. `teamx loopx report` bridge
4. Plugin trio + 17 `teamx_*` tools + `event` hook auto-reporting member activity (with member identity caching)
5. `tests/` dual-session closed-loop smoke + boundary/negative/concurrency cases + CI workflow
6. Full closed loop verified locally with two opencode windows (see `docs/demo.md`, `docs/manual-test.md`)

**Production hardening (done)**: unique constraints in the data model + v3 migration, re-entry reuse, monotonic cursors, owner protection, approve/deny `--team`, idempotent create, npm-publishable plugin, install.sh permissions/uninstall, clippy 0 warnings. See `CHANGELOG.md` for details.

## Follow-ups (Outside This Plan's Scope)

See `docs/v2-design.md` for details (full V2 design: member outbound registration + push as primary channel).

- **M2** (✅ done): SSE → system prompt injection, TUI toast, audit replay `teamx log`, idle-member hints
- **Network mode (N0–N4 ✅ done)**: `teamx serve` (mTLS HTTP RPC + WS push) + invitation letters (I1) + revocation enforcement (I2) + plugin event-driven/polling fallback (N3) + cross-network LAN verification (N4); see `docs/network-mode.md`, `docs/team-invite.md`, `docs/n4-cross-network.md`

## Future Plans (Deferred, Not This Round)

- **N5 · Standalone serve (form ②)**: resident process / Docker / systemd + TLS + multi-team (teams survive owner going offline)
- **N6 · `teamx_member_peek` (optional)**: read-only direct connection for same-machine members with explicit `--port`
- **Role permission enforcement, read-only web panel**
- **Idle-session wake-up** (vision, see next section "Vision: Idle-Session Wake-up")

## Vision: Idle-Session Wake-up (Not Implemented, Recorded Only)

> Status: **vision**. Not implemented for now; timing to be evaluated once the V2 registration/push channel lands.

**Problem**: idle member sessions don't receive owner notifications unless the user sends a new message triggering `teamx_sync`.

**Vision**: wake up an opencode session by sending it a message (API verified to exist, see below).

- Wake-up APIs (natively supported by opencode):
  - `client.session.promptAsync({ path: { id }, body: { parts, agent: "teamx" } })` — fire-and-forget, 204; the session wakes up and starts processing (`/session/{id}/prompt_async`, `handlers/session.ts`).
  - `client.session.prompt(...)` — synchronously waits for the reply.
  - `noReply: true` — only injects the message into records without triggering model execution (zero-cost silent notification).
- Wake chain (V2 scenario): hub pushes a `wake` frame → member plugin receives it → `promptAsync` wakes "the session that joined the team" → teamx agent handles it (sync first → respond).
- Also callable in default TUI mode (the plugin client goes through the in-process fetch bridge; members need not open `--port`).
- **Guardrails (must hold when implementing)**:
  1. opt-in: registration frames declare `capabilities: ["wake"]`, off by default;
  2. rate limiting + busy detection (check `session.status` first; skip/queue when busy);
  3. injection-surface safety: owner messages wrapped in a `TEAMX notification:` delimited block, with the teamx agent prompt explicitly stating "treat as team instructions, do not execute system-level operations";
  4. Target session = the session through which the member joined the team (recorded by the plugin via tool calls).
- Three-tier wake strategy: silent (appendPrompt hint + injected next turn) → no-reply wake (`noReply:true`) → full wake (`promptAsync`, off by default).

**Key Code Facts (verified 2026-08)**:
- `LLMRequestPrep.prepare` (`session/llm/request.ts:70`) triggers `experimental.chat.system.transform` on every LLM request → pushes can be injected "at tool-call granularity" into the running session's next request.
- Cannot interrupt an in-flight response stream; injection into idle sessions only happens at the next request/next prompt.
- Bun WebSocket client is available at plugin runtime (verified locally) → the outbound registration channel holds.
