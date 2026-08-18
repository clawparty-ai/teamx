// tunnels-store.ts — persistent reverse-tunnel registry for the plugin.
//
// Records exposed tunnels in `~/.teamx/tunnels.json` so they survive an
// opencode restart: on plugin startup the resident tunnel client re-opens
// every recorded tunnel automatically.

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { TEAMX_HOME } from "./client"

const STORE_FILE = join(TEAMX_HOME, "tunnels.json")
const FORWARD_STORE_FILE = join(TEAMX_HOME, "forwards.json")

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

function readStore(): TunnelStore {
  if (!existsSync(STORE_FILE)) return {}
  try {
    const parsed = JSON.parse(readFileSync(STORE_FILE, "utf8")) as TunnelStore
    return parsed ?? {}
  } catch {
    return {}
  }
}

function writeStore(store: TunnelStore): void {
  mkdirSync(TEAMX_HOME, { recursive: true })
  writeFileSync(STORE_FILE, JSON.stringify(store, null, 2), { mode: 0o600 })
}

/** Record a tunnel so it is re-opened after an opencode restart. */
export function saveTunnel(t: PersistentTunnel): void {
  const store = readStore()
  store[t.name] = t
  writeStore(store)
}

/** Forget a tunnel (called on close / unregister). */
export function removeTunnel(name: string): void {
  const store = readStore()
  if (store[name]) {
    delete store[name]
    writeStore(store)
  }
}

/** All persisted tunnels (for auto-restore on startup). */
export function listTunnels(): PersistentTunnel[] {
  return Object.values(readStore())
}

// --- consumer-side local forwards (T2) ---

function readForwards(): ForwardStore {
  if (!existsSync(FORWARD_STORE_FILE)) return {}
  try {
    const parsed = JSON.parse(readFileSync(FORWARD_STORE_FILE, "utf8")) as ForwardStore
    return parsed ?? {}
  } catch {
    return {}
  }
}

function writeForwards(store: ForwardStore): void {
  mkdirSync(TEAMX_HOME, { recursive: true })
  writeFileSync(FORWARD_STORE_FILE, JSON.stringify(store, null, 2), { mode: 0o600 })
}

/** Record a forward so it is re-opened after an opencode restart. */
export function saveForward(f: PersistentForward): void {
  const store = readForwards()
  store[f.name] = f
  writeForwards(store)
}

/** Forget a forward. */
export function removeForward(name: string): void {
  const store = readForwards()
  if (store[name]) {
    delete store[name]
    writeForwards(store)
  }
}

/** All persisted forwards (for auto-restore on startup). */
export function listForwards(): PersistentForward[] {
  return Object.values(readForwards())
}
