// serve.ts — lifecycle management for the embedded `teamx serve` subprocess
// (network-mode, N0). The plugin spawns a local `teamx serve` and records the
// PID so `serve status`/`serve stop` can inspect and stop it.

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { homedir } from "node:os"
import { join } from "node:path"
import { teamxBin, TEAMX_HOME } from "./client"

const SERVE_FILE = join(TEAMX_HOME, "serve.json")

type ServeRecord = {
  pid: number
  addr: string
  port: number
  url: string
  db: string
  started_at: string
}

function readRecord(): ServeRecord | null {
  if (!existsSync(SERVE_FILE)) return null
  try {
    return JSON.parse(readFileSync(SERVE_FILE, "utf8")) as ServeRecord
  } catch {
    return null
  }
}

function writeRecord(rec: ServeRecord): void {
  mkdirSync(TEAMX_HOME, { recursive: true })
  writeFileSync(SERVE_FILE, JSON.stringify(rec, null, 2), { mode: 0o600 })
}

function clearRecord(): void {
  if (existsSync(SERVE_FILE)) {
    try {
      // best-effort unlink
      writeFileSync(SERVE_FILE, "{}")
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

function localIP(): string {
  // Heuristic for a LAN-facing address; falls back to 127.0.0.1.
  try {
    const os = require("node:os") as typeof import("node:os")
    const nets = os.networkInterfaces()
    for (const name of Object.keys(nets)) {
      for (const net of nets[name] ?? []) {
        if (net.family === "IPv4" && !net.internal) return net.address
      }
    }
  } catch {
    // ignore
  }
  return "127.0.0.1"
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
  const rec = readRecord()
  if (!rec) return { running: false }
  if (!processAlive(rec.pid)) return { running: false, pid: rec.pid, url: rec.url }
  return { running: true, pid: rec.pid, url: rec.url, addr: rec.addr, port: rec.port, db: rec.db, started_at: rec.started_at }
}

/** Spawn a local `teamx serve` subprocess (idempotent). */
export async function serveStart(opts: { addr?: string; port?: number; db?: string }): Promise<ServeStatus> {
  const current = serveStatus()
  if (current.running) return current

  const addr = opts.addr ?? "0.0.0.0"
  const port = opts.port ?? 5781
  const db = opts.db ?? process.env.TEAMX_DB ?? join(TEAMX_HOME, "teamx.db")
  const ip = localIP()

  const args = ["serve", "--addr", addr, "--port", String(port)]
  if (opts.db || process.env.TEAMX_DB) args.push("--db", db)

  const proc = Bun.spawn([teamxBin(), ...args], {
    stdout: "pipe",
    stderr: "pipe",
    env: { ...(process.env as Record<string, string>) },
  })

  const rec: ServeRecord = {
    pid: proc.pid,
    addr,
    port,
    url: `http://${ip}:${port}`,
    db,
    started_at: new Date().toISOString(),
  }
  writeRecord(rec)

  // Poll /health until ready (up to ~5s).
  const base = `http://127.0.0.1:${port}`
  for (let i = 0; i < 25; i++) {
    try {
      const res = await fetch(`${base}/health`)
      if (res.ok) return serveStatus()
    } catch {
      // not up yet
    }
    await new Promise((r) => setTimeout(r, 200))
  }
  return serveStatus()
}

/** Stop the embedded serve subprocess, if any. */
export async function serveStop(): Promise<ServeStatus> {
  const rec = readRecord()
  if (!rec) return { running: false }
  if (processAlive(rec.pid)) {
    try {
      process.kill(rec.pid, "SIGTERM")
      // wait briefly for graceful exit
      for (let i = 0; i < 20 && processAlive(rec.pid); i++) {
        await new Promise((r) => setTimeout(r, 100))
      }
      if (processAlive(rec.pid)) process.kill(rec.pid, "SIGKILL")
    } catch {
      // ignore
    }
  }
  clearRecord()
  return { running: false }
}
