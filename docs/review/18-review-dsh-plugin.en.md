# dsh-plugin Code Review

- Document type: review
- Review subject: `dsh-plugin/` (teamx's deepseek-harness plugin, V1 initial implementation)
- Review date: 2026-08-20
- Review scope: `dsh-plugin/src/*.ts` (client/tools/commands/ws/digest/auto-execute/index/i18n) checked against `crates/teamx/src/cli.rs`, `crates/teamx/src/serve.rs`, `crates/teamx/src/commands.rs` and the dsh runtime events/APIs

## Overall Conclusion

The skeleton structure is reasonable (module layout and the mapping approach relative to opencode-plugin are correct), but there are **3 fatal design errors + about 15 functional bugs**; the current version **cannot actually run**. The issues are mainly concentrated in: CLI argument spelling mistakes (most tools pass positional arguments as flags), inconsistent session identity, event names that do not match dsh's actual API, and a broken polling/WS/member-cache chain.

## CRITICAL (blocks execution)

### C1. The `team sync` subcommand does not exist — digest/sync all broken

Both `tools.ts` `teamx_sync` and `digest.ts` `refreshDigest` call `runCli(['team', 'sync', ...])`. But in the CLI, `Sync` is a **top-level command** (`cli.rs:107`), not a subcommand of `team`. The correct call is `runCli(['sync', '--no-advance', '--session', key])` (this is exactly how opencode-plugin does it, `index.ts:290`).

- Impact: digest injection and every sync-dependent feature (membership detection, heartbeat refresh) are all broken.

### C2. Wrong `events` arguments — poller never works

`index.ts:157` calls `runCli(['events', '--session', sessKey, '--since', '0'])`, but the CLI's `Events` (`cli.rs:84`) only accepts `--after` and `--team`, with **no `--session`/`--since`**. clap fails immediately → the poller throws on every iteration and the exception is swallowed.

### C3. Misread return structure of `team list` — member cache stays empty forever

`index.ts:79-84`:

```ts
const result = await runCli(['team', 'list', '--session', key])
if (Array.isArray(result)) {           // ❌ actually returns { teams: [...] }, always false
  for (const team of result) {
    markMember(team.id, agentId, ...)   // ❌ field is team_id, not id
```

The actual structure (`commands.rs:983-997`) is `{ "teams": [{ "team_id", "name", "my_role", ... }] }`.

- Impact: `markMember`/`registerAgent` never execute → member cache, digest polling, WS push, and auto-execute — **the whole chain is broken**.

### C4. Most tools pass positional arguments as flags — nearly all CLI calls fail with "unexpected argument"

In the CLI, `name`/`token`/`member_id`/`role`/`title`/`state`/`role_desc`/`letter`/`ask_id`/`id` are all **positional** (clap `#[arg(value_name=...)]` without `long`), but the dsh-plugin writes them everywhere as `--name`/`--token`/`--member`/`--role`/`--title`/`--state`/`--ask-id`:

| Tool | Wrong call | Correct call |
|------|---------|---------|
| `teamx_create_team` | `team create --name X` | `team create X` |
| `teamx_join` | `team join --token T` | `team join T --name N` |
| `teamx_approve`/`deny` | `team approve --member M` | `team approve M` |
| `teamx_team_invite` | `team invite --role R` | `team invite "R: desc"` |
| `teamx_team_import` | `team import --json/--path` | `team import <letter>` (positional) |
| `teamx_team_invite_revoke` | `--token` | `team invite-revoke <id>` |
| `teamx_set_goal` | `goal set --title T` | `goal set T` |
| `teamx_set_role` | `role set --role R` | `role set R` |
| `teamx_role_propose` | `--role/--label` | `role propose KEY LABEL [DESC]` |
| `teamx_role_approve/deny/update` | `--role` | positional `role` |
| `teamx_set_state` | `member set-state --state S` | `member set-state S` |
| `teamx_ask` | `ask --member M --question Q` | `ask M --question Q` |
| `teamx_respond` | `respond --ask-id A` | `respond A --answer X` |

### C5. Inconsistent session identity — tools and index use different session keys

- `tools.ts:15-17` `getKey(exec)` returns the **bare `exec.agent.session.id`** (no instance prefix)
- `commands.ts:27-29` likewise returns the bare id
- But `index.ts:115/127/188` uses `sessionKey(teamxInstance, agentId)` (**with the prefix**)
- Impact: teams/members created by tools use identity A while digest/heartbeat use identity B → **the same agent drifts between two identities**, and `team list` can never find the team the tools just created.

### C6. The `agent/status` event does not exist — heartbeat never fires

`index.ts:107` listens for `agent/status`, but the dsh agent actually only emits `agent/created` / `agent/disposed` / `agent/session-start` (verified against the event map in `runtime-types.ts`). There is no `agent/status`. The idle heartbeat + idle digest refresh block is dead code.

## HIGH

### H1. `agent/dispose` spelling mistake → cleanup never runs

`index.ts:141` listens for `agent/dispose`; the actual event name is **`agent/disposed`**. When an agent exits, its cache is never cleaned up.

### H2. auto-execute chain broken

- `index.ts:160` and `:187` iterate over `(globalThis as any).__teamxAgents` — **this global is never assigned**. auto-execute's state lives in the module-level `state` variable of `auto-execute.ts`, but it is not exposed to index for iteration.
- `auto-execute.ts:69` `refreshDigest(agentId, agentId)` — the second parameter is a session key but receives the bare agentId (same as C5).
- `auto-execute.ts:64` `memberStatus(teamId, agentId)` — teamId comes from `state.agentTeam`, but since `registerAgent` is never called (C3), this is always empty.

### H3. WS push never connects

`index.ts:177` calls `knownMemberSessions('')` with an empty-string team id → always returns an empty array (the member cache is keyed by `Map<teamId, ...>`). The WS branch never executes.

### H4. mapCommandToRpc missing `sync` + positional parsing errors

- `client.ts` `mapCommandToRpc` has no `sync` branch → in network mode, digest refresh fails directly with "Cannot map CLI command to RPC".
- And `parseFlags` only recognizes `--key value`, so all positional arguments (the table in C4) are likewise unavailable in RPC mode.

### H5. tsconfig `noCheck: true` masks all type errors

`tsconfig.json` has `noCheck: true`, which skips type checking entirely — the previous "compiles fine" verification was meaningless. This also explains why so many `as any`s and wrong fields were not caught by the compiler. Checking should at least be enabled for the plugin's own code (`skipLibCheck` only skips node_modules; it does not let your own source slip through).

## MEDIUM

### M1. The `args()` helper is dead code

`tools.ts:20-36` defines `args()` but it is never called. Its logic itself is also flawed (the empty-value skip logic easily misalignes). Delete it or use it properly.

### M2. Unused imports

- `tools.ts:12` imports `sessionKey, instanceId, markMember, knownMemberSessions`, none used
- `commands.ts:11` imports `sessionKey`, unused
- `auto-execute.ts:8` imports `runCli`, unused
- `index.ts:137-139` the empty `agent/created` listener is dead code

### M3. `i18n.ts` essentially unused

An `M` constant is defined, but tools/index/commands all hardcode strings. Either wire it in or delete it.

### M4. Duplicate WS implementations

`client.ts:268+` exports `connectWs` (never used), and `ws.ts` has an independent `WsClient` class as well. Duplicated logic in two places; keep one.

### M5. `commands.ts` parseFlags does not handle quoted arguments

`--message "hello world"` gets split into two words. Slash-command user experience will suffer.

### M6. `rejectUnauthorized: !!mtls`

In `ws.ts` and `client.ts`, when mTLS material is missing this becomes `rejectUnauthorized: false` → accepts any self-signed certificate, a security downgrade. Network mode should require mTLS.

## What Is Correct

- Module layout (client/tools/commands/ws/digest/auto-execute) maps one-to-one onto opencode-plugin; clear design
- `defineTool`'s `parameters`/`output`/`render` usage conforms to the dsh schema spec
- Flat command names (`team-create` etc.) match the dsh command-name regex `/^[a-z][a-z0-9_-]*$/`
- `sessionKey()` format `${instance}:${agentId}` matches opencode (it just isn't used by tools)
- `agent/session-start` and `agent/disposed` event names verified correct
- The `followup` API exists (`runtime-types.ts:124`) — auto-execute chose the right wake-up primitive

## Suggested Fix Order

1. **Fix C4** (positional arguments) — go through the CLI tool by tool, and fix C5 at the same time (use `sessionKey(instance, agentId)` uniformly). These two are the largest workload and are prerequisites for "being able to run"
2. **Fix C1/C2** — correct the sync/events calls
3. **Fix C3/H3** — fix `team list` parsing (`result.teams` + `team_id`/`my_role`); only then does the member cache mean anything
4. **Fix C6/H1** — switch to the real events; if there are no idle events, consider deriving them from `internal/status` or session events
5. **Fix H2/H4** — bridge auto-execute state into index; add sync + positional support to mapCommandToRpc
6. **Fix H5** — turn type checking on and let the compiler catch things
7. Clean up M1-M6 dead code/duplicated implementations

---

# Second-Round Review (after the first round of fixes)

- Review date: 2026-08-20 (second round)
- Conclusion: Round one's C1-C6/H1-H5/M1-M6 have been fixed, but this round, comparing against network mode in `serve.rs` and the JSON output of `commands.rs`, uncovered **new field-name mismatch bugs**; network mode still cannot run.

## Second-Round CRITICAL

### R2-C1. `runRpc` request-body field name wrong: `params` should be `args`

`client.ts runRpc` sends `{ method, params }`, but `serve.rs`'s `RpcRequest` declares:

```rust
struct RpcRequest { method: String, #[serde(default)] args: Value }
```

`#[serde(default)]` makes `args` default to null, so in network mode **all RPC command arguments are lost** (the server reads `args` as always-null, and every `args.get(...)` in `dispatch` returns None). opencode-plugin uses `{ method, args }` (`client.ts:404`).

### R2-C2. `runRpc` response-body field name wrong: `result` should be `data`

`client.ts runRpc` reads `parsed.result`, but `serve.rs rpc` returns:

```rust
(StatusCode::OK, Json(json!({ "ok": true, "data": data })))
```

The field name is **`data`**, not `result`. In network mode every RPC return value resolves to `undefined`. opencode-plugin reads `data.data` (`client.ts:427`).

### R2-C3. WS endpoint identity mechanism misjudged: headers are ineffective, identity relies on the mTLS certificate

`serve.rs`'s `/ws` endpoint identifies members by the **mTLS peer certificate CN** (`parse_member_cn(identity.0)`); it reads no HTTP headers:

```rust
async fn ws_handler(..., Extension(identity): Extension<PeerIdentity>, ...) {
    let member_id = pki::parse_member_cn(&identity.0) ...  // comes from the certificate CN
    let teams = commands::teams_for_member(&conn, &mid)?;
    let mut rx = state.hub.subscribe(&member_id, &teams);
```

The `X-Teamx-Team`/`X-Teamx-Session` headers sent by dsh-plugin's `ws.ts` are **completely ignored**; the `team`/`session` parameters of `createWsClient` are misleading dead parameters. Moreover, when there is no mTLS certificate (`mtlsFor()` returns null), `ws.ts` still tries to connect → the server replies `no_identity` → infinite reconnect loop. Network-mode WS push requires an mTLS certificate to work.

### R2-C4. Digest field names wrong: `display_name`/`payload` mistakenly written as `name`/`data`

`commands.rs`'s sync output (`member_json`/`event_json`):

```rust
member_json => { "id", "display_name", "role", "state", ... }   // display_name, not name
event_json  => { "seq", "team_id", "member_id", "type", "payload", ... }  // payload, not data
```

`digest.ts formatDigest` uses `m.name` (should be `display_name`) and `e.data?.message` (should be `e.payload?.message`), garbling digest content (member names show undefined and event messages are lost).

## Second-Round MEDIUM

### R2-M1. `agent/status` idle heartbeat does not check membership

opencode-plugin checks `isMember === true` before sending heartbeats (`index.ts:469`). The dsh-plugin's `agent/status` idle branch sends `publish activity` directly, so non-member sessions also send (failures swallowed). It should check `memberStatus(agentId)?.isMember` in the `markMember` cache.

### R2-M2. Network-mode server URL cannot be auto-discovered from the invitation letter

opencode-plugin has `discoverServerUrl()`: after a member imports an invitation letter, it automatically enters network mode using the letter's embedded `server.url` (no manual `TEAMX_SERVER_URL`). The dsh-plugin lacks this; after importing a letter, members remain in local mode. Missing feature.

### R2-M3. Positional slots for `sync`/`events`/`log` in `cliArgsToRpc` are dead config

`sync: ['session']`, `events: ['after', 'team']`, `log: ['team', 'limit', 'after']` — these methods pass all their arguments via `--flag` (no positionals), so `rest` is always empty and slots never hit. `sync: ['session']` is especially misleading (session is a flag, not a positional). These slot entries should be removed.

## Second-Round Verified Correct

- R2 verified that `publish activity --data {kind:session.idle}` is a valid command (maps `progress.published`, does not change goal/team state); heartbeat logic is correct
- R2 verified `team list` returns `{teams: []}` for non-members (no error); membership detection is correct
- R2 verified `followup` returns `void`; `await agent.followup(...)` is legal
- R2 verified all 17 cases of `cliArgsToRpc` pass (method + args mapping correct)
- R2 verified the sync response's `teams[].team.my_member_id` matches auto-execute's matching logic
