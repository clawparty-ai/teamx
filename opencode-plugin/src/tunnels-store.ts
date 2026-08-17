// tunnels-store.ts — persistent reverse-tunnel registry for the plugin.
//
// Records exposed tunnels in `~/.teamx/tunnels.json` so they survive an
// opencode restart: on plugin startup the resident tunnel client re-opens
// every recorded tunnel automatically.

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { TEAMX_HOME } from "./client"

const STORE_FILE = join(TEAMX_HOME, "tunnels.json")

export interface PersistentTunnel {
  name: string
  port: number
  lan_ip?: string
  server_url: string
  created_at: string
}

export type TunnelStore = Record<string, PersistentTunnel>

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
