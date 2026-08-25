# teamx V1 Test Plan

## 1. Scope and Goals

Covers the full functional surface of V1 (single-machine, local, CLI-only), verifying:

1. Event ledger correctness: append-only; per-team seq monotonic and concurrency-safe.
2. State machine correctness: all legal/illegal transitions of Team/Member/Goal follow the transition table in `src/state.rs`.
3. Command-surface completeness and authorization boundaries: owner-only operations, member self-service operations, non-members rejected.
4. Boundary and negative behaviors: duplicate join, bad token, unknown role, illegal publish type, unauthorized ask/respond, etc.
5. Sync protocol: `sync` cursor advancing / not advancing, multi-team session disambiguation.
6. loopx bridging: a clear message when unavailable, correct extraction of the progress snapshot when available.
7. The plugin trio: agent/commands/tools registered and loaded correctly by opencode.

## 2. Test Strategy (Layered)

| Layer | Approach | Location | Run |
|---|---|---|---|
| Unit tests | Rust `#[cfg(test)]`: state machine transition tables, ledger seq/cursor/payload | `crates/teamx/src/state.rs`, `events.rs` | `cargo test` |
| CLI integration tests | scripted assertions against the real binary + isolated temporary SQLite databases | `tests/smoke.sh`, `tests/cli-test.sh` | `./tests/run-all.sh` |
| Three-member collaboration tests | owner+contributor+reviewer closed loop (equivalent to demo-team) | `tests/three-member.sh` | Same as above |
| Concurrency tests | 5 sessions × 3 parallel publishes, verifying strictly increasing seq (TC-301) | `tests/concurrency.sh` | Same as above |
| Plugin checks | `bunx tsc --noEmit` + `bun run build` + registration probing against opencode serve | `opencode-plugin` | `./tests/run-all.sh` |
| Model-level acceptance | a real model invoking `teamx_*` through the plugin (headless `opencode run --agent teamx`) | `tests/acceptance.sh` (consumes tokens; not part of the default suite) | Manual / optional |
| Manual E2E (acceptance) | three real opencode windows walk the full `/Team` flow | Manual | See `docs/13-demo-team.md`, TM-04 in `docs/08-test-cases.md` |

## 3. Test Environment

- macOS / Linux, Rust toolchain (cargo 1.94+), bun (for the plugin build).
- Tests always use a temporary database pointed to by `TEAMX_DB` (`mktemp`); **never touch** the production DB `~/.teamx/teamx.db`.
- No network or provider key required (no model dependency).

## 4. Test Data Isolation

- Every test script uses its own `mktemp` DB, with a `trap` cleaning up `*.db / *.db-wal / *.db-shm` on exit.
- Session identities use synthetic keys (e.g. `s:owner`, `inst:m1`), not real opencode sessions.

## 5. Entry / Exit Criteria

- **Entry**: after feature changes or doc-related code changes, `./tests/run-all.sh` must pass.
- **Exit**: `cargo test` fully green + both CLI scripts fully green + plugin typecheck/build passing; manual acceptance case TM-01 completed and recorded.

## 6. Known Limitations (Not Defects)

- Concurrent writes rely on SQLite WAL single-writer + `busy_timeout`; distributed consistency is out of scope (to be designed separately once V2 introduces the server).
- The loopx bridge reads on demand; no heartbeat or file watching.
- The plugin `event` hook currently mirrors only `session.idle`; `message.updated` activity is deferred to M2.

## 7. Regression Checklist (before every release)

```bash
cargo build && cargo test
./tests/smoke.sh
./tests/cli-test.sh
./tests/three-member.sh
./tests/concurrency.sh
(cd opencode-plugin && bunx tsc --noEmit && bun run build)
# Manual: verify TM-04 with three opencode windows (docs/13-demo-team.md)
```
