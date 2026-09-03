// console.ts — lifecycle management for the embedded `teamx console`
// subprocess (member web console, view-only). The plugin spawns a local
// `teamx console` per workspace, waits for the printed token URL, and can
// open the default browser at that URL (`/team console`).

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { teamxBin, teamxDir } from "./client"

type ConsoleRecord = {
  pid: number
  port: number
  url: string
  started_at: string
}

export interface ConsoleStatus {
  running: boolean
  pid?: number
  port?: number
  url?: string
  started_at?: string
  error?: string
}

function recordFile(cwd: string): string {
  return join(teamxDir(cwd), "console-server.json")
}

function readRecord(cwd: string): ConsoleRecord | null {
  const f = recordFile(cwd)
  if (!existsSync(f)) return null
  try {
    return JSON.parse(readFileSync(f, "utf8")) as ConsoleRecord
  } catch {
    return null
  }
}

function writeRecord(cwd: string, rec: ConsoleRecord): void {
  mkdirSync(teamxDir(cwd), { recursive: true })
  writeFileSync(recordFile(cwd), JSON.stringify(rec, null, 2), { mode: 0o600 })
}

function clearRecord(cwd: string): void {
  const f = recordFile(cwd)
  if (existsSync(f)) {
    try {
      writeFileSync(f, "{}")
    } catch {
      // ignore
    }
  }
}

function processAlive(pid: number): boolean {
  try {
    process.kill(pid, 0)
    return true
  } catch {
    return false
  }
}

export function consoleStatus(cwd?: string): ConsoleStatus {
  const dir = cwd ?? process.cwd()
  const rec = readRecord(dir)
  if (!rec) return { running: false }
  if (!processAlive(rec.pid)) {
    // Stale record: the console died. Clear it so a later start is clean.
    clearRecord(dir)
    return { running: false, pid: rec.pid, url: rec.url }
  }
  return { running: true, pid: rec.pid, port: rec.port, url: rec.url, started_at: rec.started_at }
}

/** Open a URL in the default browser (no-op guard for automation). */
export function openBrowser(url: string): void {
  if (process.env.TEAMX_NO_BROWSER_OPEN === "1") return
  try {
    const cmd =
      process.platform === "darwin"
        ? ["open", url]
        : process.platform === "win32"
          ? ["cmd", "/c", "start", "", url]
          : ["xdg-open", url]
    const proc = Bun.spawn(cmd, { stdout: "ignore", stderr: "ignore" })
    // Prevent the child from keeping the plugin alive forever.
    setTimeout(() => {
      try {
        proc.kill()
      } catch {
        // ignore
      }
    }, 15_000).unref?.()
  } catch {
    // ignore — opening a browser is best-effort; the URL is still returned.
  }
}

/**
 * Start a local `teamx console` subprocess for the workspace (idempotent).
 * Waits up to ~15s for the startup line that carries the one-time token URL,
 * then records it so `status`/`stop` and re-open work without re-spawning.
 */
export async function consoleStart(opts: { port?: number; cwd?: string }): Promise<ConsoleStatus> {
  const cwd = opts.cwd ?? process.cwd()
  const current = consoleStatus(cwd)
  if (current.running) return current

  const port = opts.port ?? 9528
  const configPath = join(teamxDir(cwd), "console.json")
  const args = ["console", "--port", String(port), "--config", configPath]

  let proc: ReturnType<typeof Bun.spawn>
  try {
    proc = Bun.spawn([teamxBin(), ...args], {
      cwd,
      stdout: "ignore",
      stderr: "pipe",
      env: { ...(process.env as Record<string, string>), TEAMX_HOME: teamxDir(cwd) },
    })
  } catch (e) {
    return { running: false, error: `无法启动 teamx console: ${String(e)}` }
  }

  const stderrRaw = proc.stderr
  if (typeof stderrRaw === "number") {
    proc.kill()
    return { running: false, error: "teamx console 启动失败（stderr 不可用）" }
  }
  const stderr = stderrRaw as ReadableStream<Uint8Array>
  const reader = stderr.getReader()

  const decoder = new TextDecoder()
  let buf = ""
  let url = ""
  const deadline = Date.now() + 15_000
  while (Date.now() < deadline && !url) {
    const { value, done } = await reader.read().catch(() => ({ value: undefined, done: true as boolean }))
    if (done) break
    if (value) buf += decoder.decode(value, { stream: true })
    const m = buf.match(/open:\s+(https?:\/\/\S+)/)
    if (m) url = m[1]
  }

  // Drain stderr in the background so a chatty console never blocks.
  void (async () => {
    try {
      // eslint-disable-next-line no-constant-condition
      while (true) {
        const { done } = await reader.read()
        if (done) break
      }
    } catch {
      // ignore
    }
  })()

  if (!url) {
    try {
      proc.kill()
    } catch {
      // ignore
    }
    return {
      running: false,
      error: "teamx console 启动超时或未输出 token（本机 teamx 二进制可能不含 console 命令）",
    }
  }

  writeRecord(cwd, { pid: proc.pid, port, url, started_at: new Date().toISOString() })
  return { running: true, pid: proc.pid, port, url, started_at: new Date().toISOString() }
}

/** Stop the embedded console subprocess, if any (per workspace). */
export async function consoleStop(cwd?: string): Promise<ConsoleStatus> {
  const dir = cwd ?? process.cwd()
  const rec = readRecord(dir)
  if (!rec) return { running: false }
  if (processAlive(rec.pid)) {
    try {
      process.kill(rec.pid, "SIGTERM")
      for (let i = 0; i < 20 && processAlive(rec.pid); i++) {
        await new Promise((r) => setTimeout(r, 100))
      }
      if (processAlive(rec.pid)) process.kill(rec.pid, "SIGKILL")
    } catch {
      // ignore
    }
  }
  clearRecord(dir)
  return { running: false }
}
