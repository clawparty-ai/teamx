# teamx ↔ loopx Bridge (V1)

Goal: **do not reinvent loopx's wheel**. teamx only adds a thin bridge: read loopx's stage progress and publish it into the team ledger as a `loopx.progress` event, so the team lead can see each member's loopx stage progress via `teamx_sync`.

## Principle

- loopx is another independent state kernel (a Python CLI, the goal/gate/todo/quota control plane for long-running tasks); teamx does not copy its state model.
- The member may optionally bind `--loopx_project <dir>` at `teamx team join`; the bound directory is the project where that member manages long tasks with loopx.
- `teamx loopx report <project>` runs `loopx status --format json` inside `<project>`, extracts a compact summary on a best-effort basis, then writes a `loopx.progress` event.
- Owner side: each turn, `teamx_sync` returns this event as a new event, and the owner agent broadcasts team progress accordingly.

## LoopxDigest Structure

```json
{
  "project": "/path/to/project",
  "available": true,
  "error": null,
  "goal_state": "active",
  "gate": "await owner decision",
  "next_todo": "implement auth",
  "quota": "eligible=yes",
  "raw": { "...": "original loopx status JSON" }
}
```

- When `available=false`, an `error` explains why (loopx not installed / project not connected / non-JSON output); **no event is written**, and it returns `{ok:false, note:"loopx unavailable; teamx core loop is unaffected"}` — teamx's own closed loop is unaffected.
- Field extraction is best-effort: compatible with keys like `active_goal_state`/`goal_state`/`state`, `gate`/`user_gate`, `next_todo`, `quota`/`quota_should_run`; for nested objects it takes their `state/text/title/summary` subfields or concatenates scalars.

## Plugin Side

The `teamx_loopx_report` tool:
- With `project` passed: runs `loopx report` directly against that project.
- Without `project`: reads from the current session's bound `loopx_project` (set at join); if unbound, returns guidance.

## Boundaries

- V1 does **on-demand reads only** (`loopx status` executes when an agent calls `teamx_loopx_report`); no file watching, no heartbeat polling.
- loopx progress is read-only into the teamx ledger; teamx never writes any state to loopx.
- loopx schema changes do not affect teamx's closed loop: extraction failures only produce an `error` note, with the original JSON preserved in `raw`.
