# teamx Code Review — 2026-08-17

> **Status: all fixed** (see the CHANGELOG's "code review fixes"). All 8 high/medium-priority items plus minor suggestions have landed with regression tests added (`tests/cli-test.sh`, `tests/mtls-test.sh`, `tests/plugin-unit/auto-execute.test.ts`).

## Scope

- Repo: `/Users/caishu/github/teamx`
- Objects: Rust CLI (`crates/teamx`), opencode plugin (`opencode-plugin`), install script
- Result: this review made no source changes

## Verification Results

- `cargo test --workspace`: all 14 unit tests pass
- `cargo clippy --all-targets -- -D warnings`: passes
- `bunx tsc --noEmit` (`opencode-plugin`): passes
- Several additional uncovered issues reproduced manually; see below for details

## High-Priority Issues

- Network mode allows cross-team read bypass.
  - `cmd_team_status` skips session/membership validation entirely when `--team` is passed.
  - `cmd_role_list`, `cmd_events`, and `cmd_log --team` likewise perform no ownership validation.
  - Although the RPC layer's `dispatch` resolves identity from the mTLS certificate, these commands still allow reading state, events, roles, and `invite_token` by arbitrary team id.
  - Locations: `crates/teamx/src/commands.rs:735`, `crates/teamx/src/commands.rs:1403`, `crates/teamx/src/commands.rs:1907`, `crates/teamx/src/serve.rs:449`

- `pending` members can publish/change team state before approval.
  - `memberships_for_session` only excludes `left/denied`.
  - `cmd_publish` doesn't check the actor's `pending/waiting` state.
  - Reproduced: unapproved `inst:b` can successfully run `publish decision`.
  - Locations: `crates/teamx/src/commands.rs:229`, `crates/teamx/src/commands.rs:1722`

- `publish --data '[]' --assignee <id>` panics.
  - When `data` is valid but non-object JSON (array/string/number), `payload["assignee_member_id"] = ...` triggers a `serde_json` panic.
  - CLI exit code 101.
  - Location: `crates/teamx/src/commands.rs:1758`

- Path traversal in invitation letter import.
  - `store_letter` builds its directory from an unvalidated `invitation_id`.
  - Reproduced: `invitation_id: "../../teamx-escaped"` writes `letter.json`, `client.crt`, `client.key`, `ca.crt` into `/tmp/teamx-escaped/`.
  - Location: `crates/teamx/src/commands.rs:1218`

## Medium-Priority Issues

- Missing PKI files cause accidental CA rebuild.
  - `ensure_pki` regenerates the entire CA + server cert whenever any one of four files is missing.
  - If only `server.key` is lost, it overwrites the existing CA, invalidating every issued member cert.
  - Location: `crates/teamx/src/pki.rs:77`

- Plugin auto-execute fires only once.
  - `alreadyExecuted: autoExecutedSeq.has(sessionID)` passes a "has ever executed" boolean instead of the current seq watermark.
  - New directed tasks on the same session after that will not auto-wake again.
  - Location: `opencode-plugin/src/index.ts:271`

- Directed-task type matching too narrow.
  - `assignedToMe` only recognizes `decision.broadcast` / `goal.shared`.
  - But any Rust-side `publish` type with `--assignee` writes `assignee_member_id`.
  - Therefore directed tasks of types like `start` / `progress` / `achieved` never trigger auto-execution.
  - Locations: `opencode-plugin/src/index.ts:153`, `crates/teamx/src/commands.rs:1756`

- Non-owners can set their own role to `owner`.
  - `ensure_owner` still uses `owner_member_id`, so there is no direct privilege escalation.
  - But the plugin's `isOwnerSession` relies on `my_role === "owner"`, letting a member bypass auto-execute or display as owner.
  - Locations: `crates/teamx/src/commands.rs:1430`, `opencode-plugin/src/index.ts:138`

## Minor Suggestions

- `teamx serve` parses the bind address with `format!("{}:{}")`, which doesn't support bare IPv6 addresses.
- `loopx::loopx_status` invokes the `loopx` subprocess without a timeout and may hang for a long time.
- `team_status_json` and first-time `sync` read the team's entire event history into memory — heavy when event volume grows; consider paging/limiting at the SQL layer.
- Neither `serve` nor the plugin's `serveStart` propagates `TEAMX_SERVER_URL` to the current plugin session after startup; subsequent tools may still take the local CLI path.
- The plugin's membership cache isn't invalidated after `leave` / `deny`; it may keep publishing activity for departed members.

## Conclusion

The overall structure is clear, and the testing foundation around the state machine and event ledger is solid. Suggested fix priority:

1. Network-mode authorization and team ownership validation
2. Invitation letter path validation
3. `publish` non-object payload panic
4. Plugin auto-execute watermark logic and task-type matching
