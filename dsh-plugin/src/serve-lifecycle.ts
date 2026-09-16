// serve-lifecycle.ts — lifecycle management for the embedded `teamx serve`
// (network-mode) and `teamx console` (member user-console) subprocesses.
//
// Ported from opencode-plugin/src/serve.ts + console.ts, adapted for Node:
// child processes are spawned with node:child_process instead of Bun.spawn.
//
// The plugin spawns a local `teamx serve` / `teamx console`, records the PID
// (+ console token URL) so `status`/`stop` tools can inspect and stop it.
// @module @teamx-ai/dsh-plugin/serve-lifecycle

import { spawn } from 'node:child_process'
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { createServer } from 'node:net'
import { join } from 'node:path'
import { TEAMX_BIN, TEAMX_HOME } from './client.js'

function teamxDir(): string {
  return TEAMX_HOME
}

// ---------------------------------------------------------------------------
// serve (network-mode server)
// ---------------------------------------------------------------------------

type ServeRecord = {
  pid: number
  addr: string
  port: number
  url: string
  db: string
  started_at: string
}

function serveFile(): string {
  return join(teamxDir(), 'serve.json')
}

function readServeRecord(): ServeRecord | null {
  const f = serveFile()
  if (!existsSync(f)) return null
  try {
    return JSON.parse(readFileSync(f, 'utf8')) as ServeRecord
  } catch {
    return null
  }
}

function writeServeRecord(rec: ServeRecord): void {
  mkdirSync(teamxDir(), { recursive: true })
  writeFileSync(serveFile(), JSON.stringify(rec, null, 2), { mode: 0o600 })
}

function clearServeRecord(): void {
  if (existsSync(serveFile())) {
    try {
      writeFileSync(serveFile(), '{}')
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

export interface ServeStatus {
  running: boolean
  pid?: number
  url?: string
  addr?: string
  port?: number
  db?: string
  started_at?: string
}

export function serveStatus(): ServeStatus {
  const rec = readServeRecord()
  if (!rec) return { running: false }
  if (!processAlive(rec.pid)) return { running: false, pid: rec.pid, url: rec.url }
  return {
    running: true,
    pid: rec.pid,
    url: rec.url,
    addr: rec.addr,
    port: rec.port,
    db: rec.db,
    started_at: rec.started_at,
  }
}

/** Spawn a local `teamx serve` subprocess (idempotent, per instance). */
export async function serveStart(opts: { addr?: string; port?: number; db?: string }): Promise<ServeStatus> {
  const current = serveStatus()
  if (current.running) return current

  // Secure default: bind loopback like the CLI does. Binding a public
  // interface is opt-in (`addr`) so LAN-exposed ports are not opened by
  // accident when the plugin auto-starts the server.
  const addr = opts.addr ?? '127.0.0.1'
  const port = opts.port ?? 5781
  const db = opts.db ?? join(teamxDir(), 'teamx.db')

  const args = ['serve', '--addr', addr, '--port', String(port)]
  if (opts.db || process.env.TEAMX_DB) args.push('--db', db)

  const proc = spawn(TEAMX_BIN, args, {
    stdio: 'ignore',
    detached: false,
    env: { ...process.env, TEAMX_HOME: teamxDir(), TEAMX_PARENT_PID: String(process.pid) },
  })
  // Keep the plugin process alive only while the server is wanted.
  proc.unref()

  const rec: ServeRecord = {
    pid: proc.pid ?? 0,
    addr,
    port,
    url: `https://${addr}:${port}`,
    db,
    started_at: new Date().toISOString(),
  }
  writeServeRecord(rec)

  // Poll liveness briefly; mTLS /health checks are done by the caller tools
  // via runCli (which presents its own mTLS material).
  for (let i = 0; i < 25; i++) {
    if (processAlive(rec.pid)) break
    await new Promise((r) => setTimeout(r, 200))
  }
  return serveStatus()
}

/** Stop the embedded serve subprocess, if any (per instance). */
export async function serveStop(): Promise<ServeStatus> {
  const rec = readServeRecord()
  if (!rec) return { running: false }
  if (processAlive(rec.pid)) {
    try {
      process.kill(rec.pid, 'SIGTERM')
      for (let i = 0; i < 20 && processAlive(rec.pid); i++) {
        await new Promise((r) => setTimeout(r, 100))
      }
      if (processAlive(rec.pid)) process.kill(rec.pid, 'SIGKILL')
    } catch {
      // ignore
    }
  }
  clearServeRecord()
  return { running: false }
}

// ---------------------------------------------------------------------------
// console (member user-console)
// ---------------------------------------------------------------------------

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

function recordFile(): string {
  return join(teamxDir(), 'console-server.json')
}

function readConsoleRecord(): ConsoleRecord | null {
  const f = recordFile()
  if (!existsSync(f)) return null
  try {
    return JSON.parse(readFileSync(f, 'utf8')) as ConsoleRecord
  } catch {
    return null
  }
}

function writeConsoleRecord(rec: ConsoleRecord): void {
  mkdirSync(teamxDir(), { recursive: true })
  writeFileSync(recordFile(), JSON.stringify(rec, null, 2), { mode: 0o600 })
}

function clearConsoleRecord(): void {
  const f = recordFile()
  if (existsSync(f)) {
    try {
      writeFileSync(f, '{}')
    } catch {
      // ignore
    }
  }
}

export function consoleStatus(): ConsoleStatus {
  const rec = readConsoleRecord()
  if (!rec) return { running: false }
  if (!processAlive(rec.pid)) {
    // Stale record: the console died. Clear it so a later start is clean.
    clearConsoleRecord()
    return { running: false, pid: rec.pid, url: rec.url }
  }
  return { running: true, pid: rec.pid, port: rec.port, url: rec.url, started_at: rec.started_at }
}

/** Open a URL in the default browser (no-op guard for automation). */
export function openBrowser(url: string): void {
  if (process.env.TEAMX_NO_BROWSER_OPEN === '1') return
  try {
    const cmd =
      process.platform === 'darwin'
        ? ['open', url]
        : process.platform === 'win32'
          ? ['cmd', '/c', 'start', '', url]
          : ['xdg-open', url]
    const proc = spawn(cmd[0], cmd.slice(1), { stdio: 'ignore', detached: true })
    proc.unref()
  } catch {
    // ignore — opening a browser is best-effort; the URL is still returned.
  }
}

/** Is a TCP port free on 127.0.0.1 right now? */
function isPortFree(port: number): Promise<boolean> {
  return new Promise((resolve) => {
    const srv = createServer()
    srv.unref()
    srv.on('error', () => resolve(false))
    srv.listen(port, '127.0.0.1', () => {
      srv.close(() => resolve(true))
    })
  })
}

/** An OS-assigned free port (high random range). */
function ephemeralPort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const srv = createServer()
    srv.unref()
    srv.on('error', reject)
    srv.listen(0, '127.0.0.1', () => {
      const addr = srv.address() as { port: number }
      srv.close(() => resolve(addr.port))
    })
  })
}

/**
 * Start a local `teamx console` subprocess (idempotent). Waits up to ~15s for
 * the startup line that carries the one-time token URL, then records it so
 * `status`/`stop` and re-open work without re-spawning.
 *
 * Port: the preferred port (9528, or an explicit `port`) is used when free;
 * otherwise a high random free port is chosen.
 */
export async function consoleStart(opts: { port?: number }): Promise<ConsoleStatus> {
  const current = consoleStatus()
  if (current.running) return current

  const preferred = opts.port ?? 9528
  let port = preferred
  if (!(await isPortFree(port))) {
    if (opts.port !== undefined) {
      return { running: false, error: `端口 ${port} 已被占用，请换一个或先停掉占用进程` }
    }
    port = await ephemeralPort()
  }
  const configPath = join(teamxDir(), 'console.json')
  const args = ['console', '--port', String(port), '--config', configPath]

  let proc: ReturnType<typeof spawn>
  try {
    proc = spawn(TEAMX_BIN, args, {
      stdio: ['ignore', 'ignore', 'pipe'],
      env: { ...process.env, TEAMX_HOME: teamxDir(), TEAMX_PARENT_PID: String(process.pid) },
    })
  } catch (e) {
    return { running: false, error: `无法启动 teamx console: ${String(e)}` }
  }
  if (!proc.stderr) {
    proc.kill()
    return { running: false, error: 'teamx console 启动失败（stderr 不可用）' }
  }

  const decoder = new TextDecoder()
  let buf = ''
  let token = ''
  const deadline = Date.now() + 15_000
  while (Date.now() < deadline && !token) {
    const chunk: Buffer = await new Promise((resolve) => {
      const timer = setTimeout(() => resolve(Buffer.alloc(0)), 500)
      proc.stderr?.once('data', (d: Buffer) => {
        clearTimeout(timer)
        resolve(d)
      })
    })
    if (chunk.length) buf += decoder.decode(chunk, { stream: true })
    const m = buf.match(/token=([0-9a-fA-F]{16,})/)
    if (m) token = m[1]
  }
  // Drain stderr in the background so a chatty console never blocks.
  proc.stderr?.resume()

  if (!token) {
    try {
      proc.kill()
    } catch {
      // ignore
    }
    return {
      running: false,
      error: 'teamx console 启动超时或未输出 token（本机 teamx 二进制可能不含 console 命令）',
    }
  }
  // Build the URL explicitly (loopback + the port we actually bound), so the
  // browser always opens `https://127.0.0.1:<port>/?token=...`.
  const url = `https://127.0.0.1:${port}/?token=${token}`

  writeConsoleRecord({ pid: proc.pid ?? 0, port, url, started_at: new Date().toISOString() })
  return { running: true, pid: proc.pid ?? 0, port, url, started_at: new Date().toISOString() }
}

/** Stop the embedded console subprocess, if any (per instance). */
export async function consoleStop(): Promise<ConsoleStatus> {
  const rec = readConsoleRecord()
  if (!rec) return { running: false }
  if (processAlive(rec.pid)) {
    try {
      process.kill(rec.pid, 'SIGTERM')
      for (let i = 0; i < 20 && processAlive(rec.pid); i++) {
        await new Promise((r) => setTimeout(r, 100))
      }
      if (processAlive(rec.pid)) process.kill(rec.pid, 'SIGKILL')
    } catch {
      // ignore
    }
  }
  clearConsoleRecord()
  return { running: false }
}
