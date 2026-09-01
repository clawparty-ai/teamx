// tunnels-store.ts — persistent reverse-tunnel registry for the plugin.
//
// Records exposed tunnels in `<cwd>/.teamx/tunnels.json` (per workspace, not
// global) so they survive an opencode restart: on plugin startup the resident
// tunnel client re-opens every recorded tunnel automatically.

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { teamxDir } from "./client"

function storeFile(cwd: string, name: string): string {
  return join(teamxDir(cwd), name)
}

export interface PersistentTunnel {
  name: string
  port: number
  mode?: "local" | "frp"
  lan_ip?: string
  server_url: string
  created_at: string
}

/** A consumer-side local forward (persisted for restart recovery). */
export interface PersistentForward {
  name: string
  local_port: number
  server_url: string
  created_at: string
}

export type TunnelStore = Record<string, PersistentTunnel>
export type ForwardStore = Record<string, PersistentForward>

function readStore(cwd: string): TunnelStore {
  const file = storeFile(cwd, "tunnels.json")
  if (!existsSync(file)) return {}
  try {
    const parsed = JSON.parse(readFileSync(file, "utf8")) as TunnelStore
    return parsed ?? {}
  } catch {
    return {}
  }
}

function writeStore(cwd: string, store: TunnelStore): void {
  mkdirSync(teamxDir(cwd), { recursive: true })
  writeFileSync(storeFile(cwd, "tunnels.json"), JSON.stringify(store, null, 2), { mode: 0o600 })
}

/** Record a tunnel so it is re-opened after an opencode restart. */
export function saveTunnel(
  name: string,
  port: number,
  mode: string | undefined,
  lan_ip: string | undefined,
  server_url: string,
  cwd: string,
): void {
  const store = readStore(cwd)
  store[name] = {
    name,
    port,
    mode: mode as "local" | "frp" | undefined,
    lan_ip,
    server_url,
    created_at: new Date().toISOString(),
  }
  writeStore(cwd, store)
}

/** Forget a tunnel (called on close / unregister). */
export function removeTunnel(name: string, cwd: string): void {
  const store = readStore(cwd)
  if (store[name]) {
    delete store[name]
    writeStore(cwd, store)
  }
}

/** All persisted tunnels of a workspace (for auto-restore on startup). */
export function listTunnels(cwd: string): PersistentTunnel[] {
  return Object.values(readStore(cwd))
}

// --- consumer-side local forwards (T2) ---

function readForwards(cwd: string): ForwardStore {
  const file = storeFile(cwd, "forwards.json")
  if (!existsSync(file)) return {}
  try {
    const parsed = JSON.parse(readFileSync(file, "utf8")) as ForwardStore
    return parsed ?? {}
  } catch {
    return {}
  }
}

function writeForwards(cwd: string, store: ForwardStore): void {
  mkdirSync(teamxDir(cwd), { recursive: true })
  writeFileSync(storeFile(cwd, "forwards.json"), JSON.stringify(store, null, 2), { mode: 0o600 })
}

/** Record a forward so it is re-opened after an opencode restart. */
export function saveForward(name: string, local_port: number, server_url: string, cwd: string): void {
  const store = readForwards(cwd)
  store[name] = { name, local_port, server_url, created_at: new Date().toISOString() }
  writeForwards(cwd, store)
}

/** Forget a forward. */
export function removeForward(name: string, cwd: string): void {
  const store = readForwards(cwd)
  if (store[name]) {
    delete store[name]
    writeForwards(cwd, store)
  }
}

/** All persisted forwards of a workspace (for auto-restore on startup). */
export function listForwards(cwd: string): PersistentForward[] {
  return Object.values(readForwards(cwd))
}
