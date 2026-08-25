# teamx V1 Spec

teamx is a team-collaboration state kernel (Rust CLI + SQLite) plus an opencode plugin. V1 targets a single local machine: all opencode sessions and teamx run on the same machine, with no network service.

## Architecture

- **CLI-only**: no server is started. On every call the plugin spawns a `teamx <cmd> --session <key> --json` subprocess.
- **Global storage**: a single SQLite database `~/.teamx/teamx.db` (overridable via `TEAMX_DB`). WAL mode + `busy_timeout` 5s + single-writer transactions support concurrent multi-process reads and serialized writes.
- **Event ledger**: all state changes are written to `events` (append-only); within each team, `seq` increases monotonically within the same transaction as the INSERT, eliminating out-of-order concurrency. Team/Member/Goal current states are projections of the ledger.
- **session_key** = `<instance UUID>:<opencode sessionID>`; the instance UUID is persisted in `~/.teamx/instance.json`.

## State Machines

| Object | States |
|---|---|
| Team | `forming → active → blocked → completed → archived` |
| Member | `pending → active → waiting → idle → left` (plus the terminal `denied` state) |
| Goal | `proposed → shared → refining → in_progress → blocked → achieved → closed` |

> The `paused` state has been removed (semantically redundant with `blocked` and unreachable); to pause work use `publish blocked`.

Legal transitions are defined in `src/state.rs` as an explicit (from, action) → to table; illegal transitions error out immediately without writing any event.

## Schema (SQLite)

```
teams(id, name, owner_member_id, goal_id, state, invite_token, created_at, updated_at)
members(id, team_id, session_key, display_name, role, state, loopx_project, last_seen_at, joined_at, left_at)
    └─ UNIQUE(team_id, session_key): one session has at most one member row per team (rejoin after leave/deny reuses that row)
goals(id, team_id, title, body, state, created_at, updated_at)
    └─ UNIQUE(team_id): one team has exactly one goal
roles(id, team_id, key, label, description, permissions_json, state, proposed_by)   -- state: proposed/approved (default approved); custom roles are proposed by members and approved by the owner
events(id, team_id, member_id, seq, type, payload_json, created_at)  -- ledger
questions(id, team_id, asker_member_id, target_member_id, question, answer, state, created_at, answered_at)
sync_cursors(session_key, team_id, last_seq)                    -- per-session incremental cursor (monotonically advancing)
```

> The redundant `sessions` table was removed in the v3 migration (`members(session_key, team_id)` already covers all of its information and it was previously write-only).

## Event Types

`team.created` `team.state_changed` `team.completed` `membership.pending` `membership.approved` `membership.denied` `member.role_set` `member.state_changed` `member.left` `goal.set` `goal.updated` `goal.shared` `goal.state_changed` `goal.achieved` `progress.published` `clarification.asked` `clarification.responded` `loopx.progress` `decision.broadcast` `role.proposed` `role.approved` `role.denied` `role.updated`

## CLI Commands

```
teamx init
teamx team create <name> --session <key> [--goal-title T] [--goal-body B]
teamx team join <token> --name <name> --session <key> [--loopx-project <dir>]
teamx team approve <member_id> --session <key>          # owner
teamx team deny <member_id> --session <key>             # owner
teamx team list --session <key>
teamx team status [--team <id>] [--session <key>]
teamx team leave --session <key> [--team <id>]     # owner cannot leave (no transfer mechanism)
teamx team archive --session <key> [--team <id>]  # owner; completed → archived
teamx goal set <title> [--body B] --session <key>       # owner
teamx goal share --session <key>                        # owner
teamx goal close --session <key>                        # owner
teamx member set-state <idle|active> --session <key> [--member <id>]  # self-service; owner may set on behalf
teamx role list [--team <id>]
teamx role set <role> --session <key> [--member <id>]   # owner may assign on behalf; only approved roles usable
teamx role propose <key> <label> [desc] --session <key>  # member proposes a custom role
teamx role approve <key> --session <key>                 # owner approves (auto-grants proposer)
teamx role deny <key> --session <key>                    # owner rejects (removes proposal)
teamx role update <key> [--label L] [--description D] --session <key>  # owner edits role label/description
teamx publish <type> [--data <json>] --session <key>
teamx ask <member_id> --question <q> --session <key>
teamx respond <ask_id> --answer <a> --session <key>
teamx events --team <id> [--after <seq>]
teamx sync --session <key> [--no-advance]
teamx loopx report <project> --session <key>
```

Global flags: `--db <path>`, `--json`. Default output is human-readable text; `--json` outputs machine-readable JSON (the plugin always appends `--json`).

publish types and their state effects:

| type | event | Goal | Team |
|---|---|---|---|
| start | goal.state_changed | → in_progress | - |
| progress | progress.published | → in_progress (if shared/refining) | - |
| activity | progress.published | - | - |
| decision | decision.broadcast | - | - |
| update | decision.broadcast | - | - |
| blocked | goal.state_changed | → blocked | → blocked |
| resumed | goal.state_changed | → in_progress | → active |
| achieved | goal.achieved | → achieved | - |
| refine | goal.state_changed | → refining | - |

## Role Catalog (defaults)

`owner / observer / supervisor / contributor / subtask-implementer / reviewer`, seeded at team creation. In V1 permissions are advisory only (`permissions_json` stays `{}`), with no enforcement.

Custom roles: any member can use `role propose` to propose their own job role (the key must not conflict with built-in roles; state=proposed); after the owner runs `role approve` the role enters the catalog and is automatically granted to the proposer, while `role deny` removes the proposal; the owner can edit any role's label/description via `role update`. `role set` only allows approved roles.

## opencode Plugin

The three-piece set (installed into `~/.config/opencode/` by `install.sh`):

- `agent/teamx.md`: `mode: all`, permission `"teamx_*": allow`, embedding the protocol of "sync before acting, report progress as it happens, and broadcast after the owner summarizes".
- `command/Team.md`: `agent: teamx`, provides the `/Team` route.
- `plugins/teamx.js`: registers 21 `teamx_*` tools + an `event` hook (`session.idle` → auto-publish activity events, membership caching).

Tool list: `teamx_create_team teamx_set_goal teamx_share_goal teamx_close_goal teamx_archive teamx_join teamx_approve teamx_deny teamx_set_role teamx_set_state teamx_list_teams teamx_status teamx_sync teamx_publish teamx_ask teamx_respond teamx_role_propose teamx_role_approve teamx_role_deny teamx_role_update teamx_loopx_report`

The client layer `opencode-plugin/src/client.ts` is the sole seam swapped for HTTP in V2.

## Security Boundary (V1)

- **No authentication**: `session_key` is self-reported by the caller (`--session`) and the CLI does not verify caller identity; `invite_token` is visible to all team members (both `team list` and `team status` return it).
- **Positioning**: V1 is a "trust this machine" collaboration convention; "owner approval/roles" are collaboration semantics, not a security boundary.
- **Owner protection**: the owner cannot `team leave` (no ownership-transfer mechanism, preventing orphaned teams); one session can be owner of at most **one** non-`archived` team (`team create` rejects a second team, except for idempotent reuse of the same name); once a `team` is `completed/archived` no one can join it again.
- **Real authentication arrives in V2** (token issuance/verification and member credentials, see `docs/02-design-v2-architecture.md`).

## Concurrency & Consistency

- `db::with_write`: `BEGIN IMMEDIATE` + up to 20 busy retries (50ms each), then a timeout error.
- seq computation and INSERT share one transaction → each team's timeline is strictly ordered.
- Sync cursors advance monotonically: `set_cursor` uses `MAX(last_seq, excluded.last_seq)`, so concurrent writes never roll a cursor back (eliminating duplicate delivery).
- Legal-transition validation happens before any write → ledger and projections always agree.
- Member/goal uniqueness constraints (`members(team_id,session_key)`, `goals(team_id)`) are enforced by the database; the application layer no longer relies on guesswork.

## Layout

```
crates/teamx/src/{main,cli,db,state,events,commands,loopx}.rs
opencode-plugin/{src/{index,tools,client}.ts, assets/{agent/teamx.md, command/Team.md}}
install.sh
tests/smoke.sh
```

## V2 Directions (not part of this plan)

`teamx serve` (HTTP+SSE), cross-network authentication, TUI toast, SSE→system prompt injection, role-permission enforcement, read-only web panel.
