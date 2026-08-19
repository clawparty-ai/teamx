// teamx enterprise — activity collection queue.
//
// The `event` hook in index.ts calls `collect()` for each event it wants to
// record. Rows are buffered and flushed as a batch (2s or 20 rows, whichever
// comes first) via `runCli(["activity", "push", ...])`:
//  - network mode → `activity.push` RPC (serve attributes member_id from the
//    mTLS cert; fills team_id when the member has exactly one team)
//  - V1 local mode → local `teamx activity push --data ... --session ...` CLI
//    (resolves member/team from the session key)
//
// Every row carries the source node identity (node_id from instance.json,
// node_name hostname) for audit. Sensitive fields (tool/command args, user
// message text) are recorded in full.

import { hostname } from "node:os"
import { instanceId, runCli } from "./client"

export type ActivityKind =
  | "tool_call"
  | "step_finish"
  | "command"
  | "file_edit"
  | "work_session"
  | "human_input"
  | "human_approval"
  | "human_command"

export interface ActivityRow {
  /** Omitted in network mode (serve fills from mTLS identity). V1 local: omitted (CLI fills from session). */
  member_id?: string
  /** Omitted when the member has exactly one team; required otherwise. */
  team_id?: string
  node_id: string
  node_name?: string
  started_at: string
  ended_at?: string
  duration_ms?: number
  kind: ActivityKind
  detail?: Record<string, unknown> | null
  tokens_input?: number
  tokens_output?: number
  tokens_reasoning?: number
  cost?: number
  has_human?: boolean
}

let buffer: ActivityRow[] = []
let flushTimer: ReturnType<typeof setTimeout> | undefined
let flushing = false
let nodeId: string
let nodeName: string
let sessionForFlush: (() => string) | null = null
let logFlush: (level: "debug" | "warn", message: string, extra?: Record<string, unknown>) => void = () => {}

/**
 * Enable the activity collector. Must be called once at plugin startup with a
 * resolver that provides the current opencode session key (used by the V1 local
 * CLI path to attribute rows to the correct member).
 */
export function initActivity(opts: {
  sessionKey: () => string
  log: (level: "debug" | "warn", message: string, extra?: Record<string, unknown>) => void
}): void {
  nodeId = instanceId()
  nodeName = hostname()
  sessionForFlush = opts.sessionKey
  logFlush = opts.log
}

/** True when the collector has been enabled (initActivity called). */
export function activityEnabled(): boolean {
  return sessionForFlush !== null
}

/** Queue an activity row; flushes automatically (2s / 20 rows). */
export function collect(row: ActivityRow): void {
  if (!activityEnabled()) return
  buffer.push(row)
  if (buffer.length >= 20) {
    void flush()
    return
  }
  if (!flushTimer) {
    flushTimer = setTimeout(() => {
      flushTimer = undefined
      void flush()
    }, 2000)
  }
}

/** Flush buffered rows (idempotent; concurrent flushes are serialized). */
export async function flush(): Promise<void> {
  if (flushing) return
  flushing = true
  try {
    while (buffer.length > 0) {
      const batch = buffer.splice(0, buffer.length)
      const session = sessionForFlush?.() ?? ""
      const r = await runCli([
        "activity",
        "push",
        "--data",
        JSON.stringify(batch),
        "--session",
        session,
      ])
      if (!r.ok) {
        logFlush("warn", "activity push failed", { stderr: r.stderr, rows: batch.length })
        // Re-queue failed rows so they are retried on the next flush (up to the
        // 2s cadence), keeping the offline-catchup property for the V1 path.
        buffer.unshift(...batch)
        break
      }
    }
  } finally {
    flushing = false
  }
}

/** Drop all buffered rows (e.g. on plugin dispose). */
export function drain(): void {
  if (flushTimer) clearTimeout(flushTimer)
  flushTimer = undefined
  buffer = []
}
