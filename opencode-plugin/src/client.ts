// Unified CLI invocation layer for the teamx plugin.
//
// V1 runs the `teamx` binary directly (single-machine, no server). Every call
// shells out to `teamx <cmd> ... --json`. For V2 this module is the single
// seam to replace with an HTTP client against `teamx serve`.

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { homedir } from "node:os"
import { join } from "node:path"

export const TEAMX_HOME = process.env.TEAMX_HOME ?? join(homedir(), ".teamx")

/** Resolve the teamx binary name (override via TEAMX_BIN). */
export function teamxBin(): string {
  return process.env.TEAMX_BIN ?? "teamx"
}

/** Stable per-machine instance id, persisted in ~/.teamx/instance.json. */
export function instanceId(): string {
  const file = join(TEAMX_HOME, "instance.json")
  if (existsSync(file)) {
    try {
      const parsed = JSON.parse(readFileSync(file, "utf8")) as { instance_id?: string }
      if (parsed.instance_id) return parsed.instance_id
    } catch {
      // fall through and regenerate
    }
  }
  const id = crypto.randomUUID()
  mkdirSync(TEAMX_HOME, { recursive: true })
  writeFileSync(file, JSON.stringify({ instance_id: id }, null, 2))
  return id
}

/** session_key = <instance-uuid>:<opencode-session-id> */
export function sessionKey(instance: string, sessionID: string | undefined): string {
  if (!sessionID) throw new Error("teamx: no opencode sessionID available in this tool context")
  return `${instance}:${sessionID}`
}

// ---------------------------------------------------------------------------
// Per-session membership cache.
// The `event` hook fires for EVERY session (all tabs), but only members produce
// ledger activity. Cache the membership check so we don't spawn a `teamx`
// subprocess on every `session.idle` for non-member sessions.
// ---------------------------------------------------------------------------

const memberCache = new Map<string, boolean>()

/** Record whether an opencode session is a teamx member (called by tools). */
export function markMember(sessionID: string | undefined, isMember: boolean): void {
  if (sessionID) memberCache.set(sessionID, isMember)
}

/** Cached membership status: true / false / undefined (unknown yet). */
export function memberStatus(sessionID: string): boolean | undefined {
  return memberCache.get(sessionID)
}

/** Session ids currently known to be teamx members (for the M2 poller). */
export function knownMemberSessions(): string[] {
  const out: string[] = []
  for (const [sid, isMember] of memberCache) {
    if (isMember) out.push(sid)
  }
  return out
}

// ---------------------------------------------------------------------------
// Per-session team digest cache (M2): refreshed by the poller, injected into
// the system prompt via `experimental.chat.system.transform` so a member agent
// sees recent team state even if it skipped `teamx_sync`.
// ---------------------------------------------------------------------------

const digestCache = new Map<string, string>()

export function setDigest(sessionID: string, digest: string): void {
  digestCache.set(sessionID, digest)
}

export function getDigest(sessionID: string): string | undefined {
  return digestCache.get(sessionID)
}

export interface CliResult {
  ok: boolean
  stdout: string
  stderr: string
  data: Record<string, unknown> | null
}

/**
 * Run a teamx CLI invocation via Bun.spawn and parse the JSON output.
 * Non-zero exits are surfaced as `{ ok: false, stderr }` instead of throwing.
 * A 30s timeout guards against a hung `teamx` subprocess.
 */
export async function runCli(args: string[], opts?: { cwd?: string }): Promise<CliResult> {
  const controller = new AbortController()
  const timer = setTimeout(() => controller.abort(), 30_000)
  try {
    const env: Record<string, string | undefined> = { ...(process.env as Record<string, string>) }
    if (process.env.TEAMX_DB) env.TEAMX_DB = process.env.TEAMX_DB
    const proc = Bun.spawn([teamxBin(), ...args, "--json"], {
      stdout: "pipe",
      stderr: "pipe",
      env,
      cwd: opts?.cwd,
      signal: controller.signal,
    })
    const [stdout, stderr] = await Promise.all([
      new Response(proc.stdout).text(),
      new Response(proc.stderr).text(),
    ])
    const exitCode = await proc.exited
    let data: Record<string, unknown> | null = null
    if (exitCode === 0 && stdout.trim()) {
      try {
        data = JSON.parse(stdout.trim())
      } catch {
        data = null
      }
    }
    return {
      ok: exitCode === 0,
      stdout: stdout.trim(),
      stderr: stderr.trim(),
      data,
    }
  } catch (e) {
    return { ok: false, stdout: "", stderr: String(e), data: null }
  } finally {
    clearTimeout(timer)
  }
}

/**
 * Render a CLI result as a compact string suitable as a tool output
 * (the LLM reads it back).
 */
export function renderResult(r: CliResult): string {
  if (!r.ok) {
    return `teamx error: ${r.stderr || r.stdout || "command failed"}`
  }
  if (r.data) {
    return JSON.stringify(r.data, null, 2)
  }
  return r.stdout
}
