// tunnels-store.ts — persistent reverse-tunnel registry for the plugin.
//
// Records exposed tunnels in `<TEAMX_HOME>/tunnels.json` (per instance) so
// they survive a restart: on plugin startup the resident tunnel client
// re-opens every recorded tunnel automatically.
//
// Ported from opencode-plugin/src/tunnels-store.ts (dsh uses TEAMX_HOME
// instead of a per-workspace .teamx dir — dsh instances are per-host, not
// per-workspace).
// @module @teamx-ai/dsh-plugin/tunnels-store

import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { TEAMX_HOME } from './client.js'

function storeFile(name: string): string {
  return join(TEAMX_HOME, name)
}

export interface PersistentTunnel {
  name: string
  port: number
  mode?: 'local' | 'frp'
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
  const file = storeFile('tunnels.json')
  if (!existsSync(file)) return {}
  try {
    const parsed = JSON.parse(readFileSync(file, 'utf8')) as TunnelStore
    return parsed ?? {}
  } catch {
    return {}
  }
}

function writeStore(store: TunnelStore): void {
  mkdirSync(TEAMX_HOME, { recursive: true })
  writeFileSync(storeFile('tunnels.json'), JSON.stringify(store, null, 2), { mode: 0o600 })
}

/** Record a tunnel so it is re-opened after a restart. */
export function saveTunnel(
  name: string,
  port: number,
  mode: string | undefined,
  lan_ip: string | undefined,
  server_url: string,
): void {
  const store = readStore()
  store[name] = {
    name,
    port,
    mode: mode as 'local' | 'frp' | undefined,
    lan_ip,
    server_url,
    created_at: new Date().toISOString(),
  }
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

// --- consumer-side local forwards ---

function readForwards(): ForwardStore {
  const file = storeFile('forwards.json')
  if (!existsSync(file)) return {}
  try {
    const parsed = JSON.parse(readFileSync(file, 'utf8')) as ForwardStore
    return parsed ?? {}
  } catch {
    return {}
  }
}

function writeForwards(store: ForwardStore): void {
  mkdirSync(TEAMX_HOME, { recursive: true })
  writeFileSync(storeFile('forwards.json'), JSON.stringify(store, null, 2), { mode: 0o600 })
}

/** Record a forward so it is re-opened after a restart. */
export function saveForward(name: string, local_port: number, server_url: string): void {
  const store = readForwards()
  store[name] = { name, local_port, server_url, created_at: new Date().toISOString() }
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
