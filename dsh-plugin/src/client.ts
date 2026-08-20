/**
 * teamx CLI/RPC client for dsh-plugin.
 * Mirrors opencode-plugin's client.ts but runs on Node (not Bun).
 * Supports both local mode (spawn teamx binary) and network mode (HTTP mTLS RPC).
 * @module @teamx/dsh-plugin/client
 */

import { execFile } from 'node:child_process'
import { readFile } from 'node:fs/promises'
import { homedir } from 'node:os'
import { join } from 'node:path'
import { promisify } from 'node:util'
import https from 'node:https'
import http from 'node:http'
import type { Agent } from '@deepseek-ai/dsh-agent'
import { WebSocket } from 'ws'

const execFileAsync = promisify(execFile)

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

const INSTANCE_FILE = join(homedir(), '.teamx', 'instance.json')

export async function instanceId(): Promise<string> {
  try {
    const raw = await readFile(INSTANCE_FILE, 'utf-8')
    const parsed = JSON.parse(raw)
    return parsed.instance_id || parsed.instanceId || ''
  } catch {
    return ''
  }
}

/**
 * Compute the teamx session key for a dsh agent.
 * Format: `${teamxInstance}:${agentSessionId}` — same as opencode-plugin,
 * so dsh agents and opencode sessions can join the same team.
 */
export function sessionKey(teamxInstance: string, agentSessionId: string): string {
  return `${teamxInstance}:${agentSessionId}`
}

// ---------------------------------------------------------------------------
// TLS helpers (network mode)
// ---------------------------------------------------------------------------

export interface MtlsConfig {
  ca: string
  cert: string
  key: string
}

/**
 * Build mTLS TLS options from environment variables.
 * Reuses the same env conventions as opencode-plugin:
 *   TEAMX_MTLS_CERT, TEAMX_MTLS_KEY, TEAMX_MTLS_CA
 */
export async function mtlsFor(): Promise<MtlsConfig | null> {
  const certPath = process.env.TEAMX_MTLS_CERT
  const keyPath = process.env.TEAMX_MTLS_KEY
  const caPath = process.env.TEAMX_MTLS_CA
  if (!certPath || !keyPath || !caPath) return null
  const [cert, key, ca] = await Promise.all([
    readFile(certPath, 'utf-8'),
    readFile(keyPath, 'utf-8'),
    readFile(caPath, 'utf-8'),
  ])
  return { ca, cert, key }
}

// ---------------------------------------------------------------------------
// Member cache
// ---------------------------------------------------------------------------

const membersByTeam = new Map<string, Map<string, { sessionId: string; name: string; role: string }>>()

export function markMember(teamId: string, sessionId: string, name: string, role: string): void {
  if (!membersByTeam.has(teamId)) membersByTeam.set(teamId, new Map())
  membersByTeam.get(teamId)!.set(sessionId, { sessionId, name, role })
}

export function memberStatus(teamId: string, sessionId: string): { name: string; role: string } | null {
  return membersByTeam.get(teamId)?.get(sessionId) ?? null
}

export function knownMemberSessions(teamId: string): string[] {
  return [...(membersByTeam.get(teamId)?.keys() ?? [])]
}

export function clearMemberCache(teamId?: string): void {
  if (teamId) membersByTeam.delete(teamId)
  else membersByTeam.clear()
}

// ---------------------------------------------------------------------------
// runCli — spawn teamx binary (local mode)
// ---------------------------------------------------------------------------

const TEAMX_BIN = process.env.TEAMX_BIN || 'teamx'
const DEFAULT_TIMEOUT_MS = 30_000

/**
 * Run a teamx CLI command and return parsed JSON output.
 * In local mode (no TEAMX_SERVER_URL), spawns the teamx binary directly.
 * In network mode, delegates to runRpc (HTTP mTLS).
 */
export async function runCli(
  args: string[],
  timeoutMs: number = DEFAULT_TIMEOUT_MS,
): Promise<any> {
  // Network mode: HTTP RPC
  if (process.env.TEAMX_SERVER_URL) {
    return runRpc(args)
  }

  // Local mode: spawn binary
  try {
    const { stdout } = await execFileAsync(TEAMX_BIN, [...args, '--json'], {
      timeout: timeoutMs,
      maxBuffer: 1024 * 1024,
      env: { ...process.env },
    })
    return JSON.parse(stdout.trim())
  } catch (err: any) {
    if (err.code === 'ENOENT') {
      throw new Error(
        `teamx binary not found. Install teamx or set TEAMX_BIN. Searched: ${TEAMX_BIN}`,
      )
    }
    throw new Error(`teamx ${args[0]} failed: ${err.stderr || err.message}`)
  }
}

// ---------------------------------------------------------------------------
// runRpc — HTTP mTLS RPC (network mode, mirrors opencode-plugin)
// ---------------------------------------------------------------------------

interface RpcResponse {
  ok: boolean
  result?: any
  error?: string
}

/**
 * Map a teamx CLI command + args to an RPC method + params.
 * Mirrors opencode-plugin's runRpcRaw command→method mapping.
 */
function mapCommandToRpc(args: string[]): { method: string; params: Record<string, any> } | null {
  const cmd = args[0]
  const rest = args.slice(1)
  const params: Record<string, any> = {}

  // Helper: parse --key value pairs from args
  const parseFlags = (a: string[]) => {
    for (let i = 0; i < a.length; i++) {
      if (a[i].startsWith('--') && i + 1 < a.length) {
        params[a[i].slice(2)] = a[i + 1]
        i++
      }
    }
  }

  switch (cmd) {
    case 'team':
      if (rest[0] === 'create') { parseFlags(rest.slice(1)); return { method: 'team.create', params } }
      if (rest[0] === 'join') { parseFlags(rest.slice(1)); return { method: 'team.join', params } }
      if (rest[0] === 'approve') { parseFlags(rest.slice(1)); return { method: 'team.approve', params } }
      if (rest[0] === 'deny') { parseFlags(rest.slice(1)); return { method: 'team.deny', params } }
      if (rest[0] === 'list') { parseFlags(rest.slice(1)); return { method: 'team.list', params } }
      if (rest[0] === 'status') { parseFlags(rest.slice(1)); return { method: 'team.status', params } }
      if (rest[0] === 'leave') { parseFlags(rest.slice(1)); return { method: 'team.leave', params } }
      if (rest[0] === 'archive') { parseFlags(rest.slice(1)); return { method: 'team.archive', params } }
      if (rest[0] === 'destroy') { parseFlags(rest.slice(1)); return { method: 'team.destroy', params } }
      if (rest[0] === 'invite') { parseFlags(rest.slice(1)); return { method: 'team.invite', params } }
      if (rest[0] === 'invite-list') { parseFlags(rest.slice(1)); return { method: 'team.invite_list', params } }
      if (rest[0] === 'invite-revoke') { parseFlags(rest.slice(1)); return { method: 'team.invite_revoke', params } }
      if (rest[0] === 'import') { parseFlags(rest.slice(1)); return { method: 'team.import', params } }
      break
    case 'goal':
      if (rest[0] === 'set') { parseFlags(rest.slice(1)); return { method: 'goal.set', params } }
      if (rest[0] === 'share') { parseFlags(rest.slice(1)); return { method: 'goal.share', params } }
      if (rest[0] === 'close') { parseFlags(rest.slice(1)); return { method: 'goal.close', params } }
      break
    case 'role':
      if (rest[0] === 'list') { parseFlags(rest.slice(1)); return { method: 'role.list', params } }
      if (rest[0] === 'set') { parseFlags(rest.slice(1)); return { method: 'role.set', params } }
      if (rest[0] === 'propose') { parseFlags(rest.slice(1)); return { method: 'role.propose', params } }
      if (rest[0] === 'approve') { parseFlags(rest.slice(1)); return { method: 'role.approve', params } }
      if (rest[0] === 'deny') { parseFlags(rest.slice(1)); return { method: 'role.deny', params } }
      if (rest[0] === 'update') { parseFlags(rest.slice(1)); return { method: 'role.update', params } }
      break
    case 'member':
      if (rest[0] === 'set-state') { parseFlags(rest.slice(1)); return { method: 'member.set_state', params } }
      break
    case 'publish':
      if (rest[0] === 'activity') { parseFlags(rest.slice(1)); return { method: 'activity.push', params } }
      break
    case 'events':
      parseFlags(rest)
      return { method: 'events', params }
    case 'log':
      parseFlags(rest)
      return { method: 'log', params }
  }

  return null
}

async function runRpc(args: string[]): Promise<any> {
  const serverUrl = process.env.TEAMX_SERVER_URL
  if (!serverUrl) throw new Error('TEAMX_SERVER_URL not set')

  const mapped = mapCommandToRpc(args)
  if (!mapped) throw new Error(`Cannot map CLI command to RPC: ${args.join(' ')}`)

  const mtls = await mtlsFor()
  const body = JSON.stringify({
    method: mapped.method,
    params: mapped.params,
  })

  const url = new URL('/rpc', serverUrl)
  const isHttps = url.protocol === 'https:'

  return new Promise((resolve, reject) => {
    const options: https.RequestOptions = {
      hostname: url.hostname,
      port: url.port || (isHttps ? 443 : 80),
      path: url.pathname,
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'Content-Length': Buffer.byteLength(body),
      },
      ...(mtls && isHttps
        ? { ca: mtls.ca, cert: mtls.cert, key: mtls.key, rejectUnauthorized: true }
        : {}),
    }

    const transport = isHttps ? https : http
    const req = transport.request(options, (res) => {
      let data = ''
      res.on('data', (chunk) => (data += chunk))
      res.on('end', () => {
        try {
          const parsed: RpcResponse = JSON.parse(data)
          if (parsed.ok) resolve(parsed.result)
          else reject(new Error(parsed.error || 'RPC error'))
        } catch {
          reject(new Error(`Invalid RPC response: ${data.slice(0, 200)}`))
        }
      })
    })

    req.on('error', reject)
    req.setTimeout(30_000, () => {
      req.destroy()
      reject(new Error('RPC timeout'))
    })
    req.write(body)
    req.end()
  })
}

// ---------------------------------------------------------------------------
// WS push client (for real-time event notifications)
// ---------------------------------------------------------------------------

export interface WsClientOptions {
  serverUrl: string
  team: string
  session: string
  token?: string
  onEvent: (event: { type: string; data?: any }) => void
}

/**
 * Connect to the teamx server's WS endpoint for real-time event push.
 * Supports mTLS via TEAMX_MTLS_* env vars.
 * Returns a cleanup function to close the connection.
 */
export async function connectWs(opts: WsClientOptions): Promise<() => void> {
  const mtls = await mtlsFor()
  const url = new URL('/ws', opts.serverUrl)
  const wsUrl = url.toString().replace(/^http/, 'ws')

  const headers: Record<string, string> = {
    'X-Teamx-Team': opts.team,
    'X-Teamx-Session': opts.session,
  }
  if (opts.token) headers['Authorization'] = `Bearer ${opts.token}`

  const ws = new WebSocket(wsUrl, {
    headers,
    ca: mtls?.ca,
    cert: mtls?.cert,
    key: mtls?.key,
    rejectUnauthorized: !!mtls,
  })

  ws.on('message', (raw: Buffer) => {
    try {
      const event = JSON.parse(raw.toString())
      opts.onEvent(event)
    } catch {
      // ignore malformed messages
    }
  })

  ws.on('error', (err) => {
    console.error('[teamx-dsh] WS error:', err.message)
  })

  return () => {
    if (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING) {
      ws.close()
    }
  }
}
