# 21-review: Full-Codebase Code Review (Complete)

| Item | Content |
| --- | --- |
| Review date | 2026-08-22 |
| Reviewer | LLM: **ox-alpha** (opencode session) |
| Scope | Rust core, 13 files (~7300 lines), opencode-plugin, dsh-plugin (TS ~4600 lines of source), install.sh, tests/ |
| Method | Per-file human-grade close reading + cross-verified call chains; re-ran `cargo test` / `clippy` / both plugins' `tsc --noEmit` / smoke·concurrency·cli-edge E2E live tests |
| Verification result | cargo test 41/41 passed; clippy 0 warnings; typecheck all passed; smoke/concurrency/cli-test all green |

---

## Overall Assessment

Architecture quality is above average: the append-only ledger + state-machine projection design is clear, with seq allocation and writes in the same transaction guaranteeing monotonicity;
mTLS identity derived from the certificate CN rather than self-reported session is the right decision; SOCKS5 parsing boundaries are rigorous with unit tests.

The main issues concentrate on: **consistency/authorization blind spots in the tunnel subsystem**, **several private-key file permission oversights**, and
**behavior drift between dsh-plugin and opencode-plugin**.

---

## High

### H1 Revoked members can still use tunnels
- Location: `crates/teamx/src/serve.rs` `handle_tunnel_ws` / `handle_tunnel_forward`
- `/ws` checks `is_revoked` after the handshake (serve.rs:551), and the RPC layer also blocks it (serve.rs:761),
  but the two tunnel WS entry points only check `teams_for_member` (excluding left/denied), not revocation.
  After an invitation is revoked, its certificate still passes mTLS handshake and the data plane bypasses auth entirely:
  it can keep registering tunnels / act as a proxy exit for egress.
- Fix: add an `is_revoked` check to both tunnel handlers after resolving member_id; disconnect immediately upon revocation.
- Status: ✅ Fixed (this round)

### H2 Registering a second tunnel on the same provider WS leaks the first one
- Location: `crates/teamx/src/serve.rs` `handle_tunnel_ws`
- `owned: Option<String>` is overwritten after the second register succeeds; on disconnect only the last name is cleaned up,
  so previously registered tunnel entries and frp listener ports leak permanently (until restart or explicit close).
- Fix: track all tunnels registered by this connection with a set instead; clean each up on disconnect; remove synchronously on unregister;
  route binary frames to their owning tunnel by globally unique stream_id (no longer depending on a single owned name).
- Status: ✅ Fixed (this round)

### H3 TEAM.md member key unsanitized → path traversal
- Location: `crates/teamx/src/teamfile.rs:129-131` (key taken verbatim from `### <key>`)
  + `crates/teamx/src/commands.rs` `bootstrap_from_teamfile` (`members_dir.join(&m.key)`)
- `### ../../../evil` would write AGENTS.md / invitation.letter (containing the client private key) outside the project.
  Cloning someone else's repository and running `team create` triggers bootstrap automatically.
- Fix: validate at parse time that the key contains only `[A-Za-z0-9._-]`, no path separators, and doesn't start with `.`,
  erroring on illegal keys (TEAM.md parse errors surface as warnings and don't block team creation).
- Status: ✅ Fixed (this round)

---

## Medium

### M1 `team join` can join a destroyed team
- Location: `crates/teamx/src/commands.rs` `cmd_team_join`
- It only rejects `completed|archived`; destroyed passes the check and the member stays pending forever and invisible to every command
  (`memberships_for_session` filters out destroyed teams).
- Fix: add `destroyed` to the rejection conditions. Status: ✅ Fixed (this round)

### M2 Tunnel dial failure doesn't notify the peer; streams hang half-open
- Location: provider side `crates/teamx/src/tunnel_client.rs` `run_expose` (dial failure only logs)
  + `opencode-plugin/src/tunnel.ts` `openStream` (connection errors only delete from the map)
- The server-side stream entry and consumer wait forever; a `close_stream` should be sent to the server.
- Status: ✅ Fixed (both Rust + TS sides)

### M3 frp relay bind failure leaves a zombie entry
- Location: `crates/teamx/src/tunnel.rs` `run_tcp_relay`
- Bind failure returns Err but the registry entry and port remain; status shows active though actually unusable.
- Fix: on bind failure call `registry.remove(team, name)` to roll back and release the port.
- Status: ✅ Fixed (this round)

### M4 Private-key file permission oversights (4 places)
- `crates/teamx/src/pki.rs::write_pem`: writes first then chmods, leaving a 0644 window → changed to create atomically with `OpenOptions.mode(0o600)`. ✅
- `crates/teamx/src/commands.rs::store_letter`: same as above → likewise changed to atomic 0600 creation. ✅
- `crates/teamx/src/commands.rs::cmd_cert_issue --out`: member.key had no chmod at all → added 0600 (cert tightened too). ✅
- `crates/teamx/src/commands.rs::bootstrap_from_teamfile`: invitation.letter (contains private keys) written into the project directory without chmod → chmod 0600, plus `.teamx/members/` added to `.gitignore` to prevent accidental commits. ✅
- Status: ✅ All fixed (this round)

### M5 dsh-plugin RPC slots table missing entries; network-mode commands broken
- Location: `dsh-plugin/src/client.ts` `cliArgsToRpc` slots
- Compared with opencode-plugin it lacks `'loopx.report': ['project']`, `'tunnel.status'/'tunnel.close': ['name']`
  → these three commands drop positional arguments in network mode and fail. The two hand-copied tables have drifted.
- Fix: fill in the missing slots. Status: ✅ Fixed (this round); long-term suggestion to extract a shared module (not done this round)

### M7 forwardTunnel reports the wrong port when port is 0
- Location: `opencode-plugin/src/tunnel.ts` `tryBind`
- `bound = port` is assigned before listen; when port=0 the actual bind uses a random port but ready() returns 0.
- Fix: resolve `server.address().port` in the success callback; surface errno info in the error branch.
- Status: ✅ Fixed (this round)

### M8 expose/forward ready() race
- Location: `opencode-plugin/src/tunnel.ts`
- If ack/bind completes before the caller invokes `ready()`, the result is discarded and ready() waits idly for 10s then falsely reports failure.
- Fix: store the most recent known result; `ready()` returns immediately if a result already exists.
- Status: ✅ Fixed (this round)

### M9 opencode-plugin WS reconnects infinitely after receiving a revoked close
- Location: `opencode-plugin/src/ws.ts`
- No special handling for the `{type:"close",code:"revoked"}` sentinel; onclose always reconnects and never stops
  (the dsh version handles gaveUp correctly). Fix: on receiving the error/close sentinel, decide whether to stop reconnecting based on code.
- Status: ✅ Fixed (this round)

### M10 Plugin starts server on 0.0.0.0 by default, contradicting the CLI's safe default
- Location: `opencode-plugin/src/serve.ts` `serveStart` defaults to `addr="0.0.0.0"` (the CLI defaults to 127.0.0.1).
- mTLS can backstop RPC, but this exposes frp tunnel ports onto the LAN.
- Fix: default changed to 127.0.0.1; pass addr explicitly when the LAN is needed.
- Status: ✅ Fixed (this round)

### M6 Global single lock serializes all RPCs (not fixed this round; recorded as follow-up work)
- Location: `crates/teamx/src/serve.rs` — a single `Mutex<Connection>` wraps every request including read-only ones.
- Suggestion: r2d2_sqlite connection pool + WAL concurrent reads; involves architectural change, scheduled separately.

---

## Low

| # | Location | Issue | Handling this round |
| --- | --- | --- | --- |
| L1 | `broadcast.rs::subscribe` | A member's second WS connection overwrites the old sender, silently orphaning the old connection; unbounded channel backlog under slow consumers | ✅ Subscription keys get a per-connection unique suffix so multiple connections coexist; close-sentinel semantics unchanged |
| L2 | `events.rs` row_to_event / emit | Corrupted payloads silently become None/empty string | ✅ Parse failures warn via eprintln (return semantics unchanged) |
| L4 | `state.rs` vs `publish_plan` | Team-level `(Blocked,PublishStart)=>Active` inconsistent with the goal-level rule table (that edge is actually unreachable via publish) | ⏸ Not changing semantics; archived as a note here only |
| L5 | `tunnel_client.rs` env_mtls | Bad PEM panics directly; https_post has no timeout/status-code check | ✅ panic changed to returning Err; https_post gained timeout and status-code checking |
| L6 | `loopx.rs` timeout thread leak | After timeout, the detached thread keeps waiting on the child process | ⏸ Not handled (low impact) |
| L7 | `cmd_ask` etc. | Pending members can initiate questions (publish has a guard); dead parameter in `ensure_owner`; three copies of cliArgsToRpc | ⏸ Not handled |
| L8 | `install.sh` / `serve.ts` | Version fallback hardcoded; clearRecord writes {} instead of unlinking | ⏸ Not handled |

## Nits (not handled, for the record)

- `serve.rs` IPv6 bind detection `contains(':')` has an unhelpful error message
- `main.rs print_human` degrades nested objects to JSON output
- `cmd_sync` sorts cross-team events by per-team seq, which is meaningless
- dsh client.ts maxBuffer capped at 1MB
- `TunnelCmd::*`'s `--session/--team` are dead parameters never used (identity comes from the certificate); misleading to users

---

## What Was Done Well

1. Ledger + state-machine projection: seq is allocated within an Immediate transaction; cross-process concurrency verified correct in practice (15 concurrent writers with strictly increasing seq).
2. sync cursor advances monotonically using `MAX(last_seq, excluded.last_seq)` with regression tests.
3. Identity model: authorization always goes through the certificate CN; `role set owner` guards against usurpation; RPC blocked after revocation.
4. SOCKS5 parser is a pure function with complete boundary tests; UUID validation in store_letter shows good traversal awareness.
5. Testing culture: 41 unit tests + 13 categories of E2E scenario scripts.

## Verification After This Round's Fixes

- `cargo test --workspace`: all passed
- `cargo clippy --workspace`: 0 warnings
- opencode-plugin / dsh-plugin: `tsc --noEmit` passed, bundle build passed
- `tests/smoke.sh`, `tests/concurrency.sh`, `tests/cli-test.sh`: all green
- New unit tests: TEAM.md key sanitization (legal/traversal cases), join destroyed rejection
