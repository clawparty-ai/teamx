# teamx TEAM.md Team Initialization — Test Plan

> Related design: `docs/05-design-teamfile.md`; this plan covers the TEAM.md-driven initialization feature newly added on the main branch.

## 1. Scope and Goals

Covers the full functional surface of `team create` detecting `.teamx/TEAM.md` and auto-initializing the project:

1. **TEAM.md parser**: parse results for valid / invalid / missing-section files.
2. **Create integration**: when TEAM.md is detected, automatically completes goal set, member letter issuance, AGENTS.md generation, and working-directory creation.
3. **Member AGENTS.md merging**: project-root AGENTS.md + TEAM.md member descriptions merged correctly.
4. **Letter dual output**: written to file (`.teamx/members/[name]/invitation.letter`) and printed to the CLI alike.
5. **Compatibility**: without TEAM.md, `team create` keeps its original behavior; an invalid TEAM.md does not block (degrades with a warning).

## 2. Test Strategy (Layered)

| Layer | Approach | Location | Run |
|---|---|---|---|
| Unit tests | `teamfile.rs` parser (Rust `#[cfg(test)]`) | `crates/teamx/src/teamfile.rs` | `cargo test` |
| CLI integration tests | scripted assertions against the real binary + temporary TEAM.md directories | `tests/teamfile-test.sh` (new) | `./tests/run-all.sh` |
| Regression | the existing full suite (ensures unchanged behavior when TEAM.md is absent) | `tests/run-all.sh` | Same as above |

## 3. Test Environment

- macOS / Linux, Rust toolchain, bun (plugin build, if the plugin side is involved).
- Tests use `TEAMX_HOME` + `TEAMX_DB` pointing to temporary directories/databases (`mktemp`); `~/.teamx/teamx.db` is never touched.
- `.teamx/TEAM.md` is constructed under a temporary project directory (kept isolated from the repository root under test).

## 4. Test Data Isolation

- Each case gets its own temporary project directory (containing `.teamx/TEAM.md`).
- On exit, a `trap` cleans up the temporary directories (`TEAMX_HOME`, project directory, `teamx.db*`).

## 5. Entry / Exit Criteria

- **Entry**: after feature changes, `./tests/run-all.sh` must pass.
- **Exit**: `cargo test` fully green + `teamfile-test.sh` fully green + no regression in the existing suite; the TEAM.md initialization demo recorded during manual acceptance.

## 6. Known Limitations (Not Defects)

- TEAM.md parsing is lenient and fault-tolerant; unsupported sections are ignored (no error raised).
- Letters are not cleaned up automatically after being saved (retained for audit).
- Member roles accept any role key; built-in roles are not enforced.
