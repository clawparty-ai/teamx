/**
 * teamx CLI/RPC client for dsh-plugin.
 * Mirrors opencode-plugin's client.ts but runs on Node (not Bun).
 * Supports both local mode (spawn teamx binary) and network mode (HTTP mTLS RPC).
 * @module @teamx-ai/dsh-plugin/client
 */

import { execFile } from 'node:child_process'
import { existsSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs'
import { readFile } from 'node:fs/promises'
import { randomUUID } from 'node:crypto'
import { homedir } from 'node:os'
import { dirname, join } from 'node:path'
import { promisify } from 'node:util'
import https from 'node:https'
import http from 'node:http'

const execFileAsync = promisify(execFile)

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

export const TEAMX_HOME = process.env.TEAMX_HOME ?? join(homedir(), '.teamx')

const INSTANCE_FILE = join(TEAMX_HOME, 'instance.json')

/**
 * Read (or create) the teamx instance id. Synchronous, matching
 * opencode-plugin: generates a UUID on first run and persists it.
 */
export function instanceId(): string {
  const file = INSTANCE_FILE
  if (existsSync(file)) {
    try {
      const parsed = JSON.parse(readFileSync(file, 'utf8')) as { instance_id?: string }
      if (parsed.instance_id) return parsed.instance_id
    } catch {
      // fall through and regenerate
    }
  }
  const id = randomUUID()
  mkdirSync(TEAMX_HOME, { recursive: true })
  writeFileSync(file, JSON.stringify({ instance_id: id }, null, 2))
  return id
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
// Network-mode server URL discovery (mirrors opencode-plugin)
// ---------------------------------------------------------------------------

/**
 * Discover the network-mode server URL from imported invitation letters.
 * A letter embeds its server URL (teamx_invitation.server.url). Returns the
 * most recently imported letter's server URL, or null when no letter records
 * one (pure single-machine V1 mode).
 */
function discoverServerUrl(): string | null {
  const dir = join(TEAMX_HOME, 'letters')
  if (!existsSync(dir)) return null
  let entries: string[] = []
  try {
    entries = readdirSync(dir)
  } catch {
    return null
  }
  let best: { url: string; mtime: number } | null = null
  for (const id of entries) {
    const letterPath = join(dir, id, 'letter.json')
    if (!existsSync(letterPath)) continue
    try {
      const letter = JSON.parse(readFileSync(letterPath, 'utf8')) as {
        teamx_invitation?: { server?: { url?: string } }
      }
      const url = letter.teamx_invitation?.server?.url
      if (!url) continue
      const mtime = statSync(letterPath).mtimeMs
      if (!best || mtime > best.mtime) best = { url, mtime }
    } catch {
      // skip unreadable letters
    }
  }
  return best?.url ?? null
}

/**
 * Resolve the network-mode server URL. Order: explicit TEAMX_SERVER_URL env
 * wins; otherwise the most recently imported invitation letter's embedded
 * server URL; empty string means pure V1 CLI mode (no network server).
 */
export function resolveServerUrl(): string {
  return process.env.TEAMX_SERVER_URL || discoverServerUrl() || ''
}

// ---------------------------------------------------------------------------
// TLS helpers (network mode)
// ---------------------------------------------------------------------------

export interface MtlsConfig {
  ca: string
  cert: string
  key: string
}

/** Host portion of a URL (for letter matching). */
function hostOf(url: string): string {
  try {
    return new URL(url).hostname
  } catch {
    return url
  }
}

/**
 * If any of the given env paths points inside an imported-letter directory
 * (a directory containing `letter.json`), return that directory.
 *
 * A launcher that provisions one seat per process sets TEAMX_MTLS_* at start;
 * even a partial set (e.g. CERT+CA without KEY) then pins the intended letter.
 * On a host with several letters for the SAME server (re-issued seats, other
 * members' experiments), recency heuristics can pick the wrong seat — the
 * env-pointed letter is the operator's explicit choice and must win.
 */
function letterDirFromEnv(paths: (string | undefined)[]): string | null {
  for (const p of paths) {
    if (!p) continue
    const dir = dirname(p)
    if (dir && existsSync(join(dir, 'letter.json'))) return dir
  }
  return null
}

/**
 * Build mTLS TLS options. Resolution order:
 * 1. Explicit env vars TEAMX_MTLS_CERT/KEY/CA — but only when all three are
 *    set AND the files exist (a stale/partial env must not throw or shadow
 *    valid letter material).
 * 2. **Env-pinned letter directory**: when any env path points inside an
 *    imported-letter directory, use that letter's complete triple. This keeps
 *    a partially-set env from falling through to recency heuristics that, on
 *    hosts with several letters for the same server, select the wrong seat.
 * 3. Auto-discovery from imported invitation letters under
 *    `TEAMX_HOME/letters/<id>/` (client.crt/client.key/ca.crt): among letters
 *    whose embedded server URL host matches `serverUrl`, the most recently
 *    written one wins; otherwise the most recent letter overall. This mirrors
 *    opencode-plugin and covers the common case where no env was configured.
 * Returns null when neither source yields complete material — callers should
 * treat that as "not provisioned yet" (retryable), not as fatal.
 */
export async function mtlsFor(serverUrl?: string): Promise<MtlsConfig | null> {
  const certPath = process.env.TEAMX_MTLS_CERT
  const keyPath = process.env.TEAMX_MTLS_KEY
  const caPath = process.env.TEAMX_MTLS_CA
  if (
    certPath &&
    keyPath &&
    caPath &&
    existsSync(certPath) &&
    existsSync(keyPath) &&
    existsSync(caPath)
  ) {
    const [cert, key, ca] = await Promise.all([
      readFile(certPath, 'utf-8'),
      readFile(keyPath, 'utf-8'),
      readFile(caPath, 'utf-8'),
    ])
    return { ca, cert, key }
  }

  // Partial env: honor the operator's explicit letter choice (step 2).
  const pinnedDir = letterDirFromEnv([certPath, keyPath, caPath])
  if (pinnedDir) {
    const cert = join(pinnedDir, 'client.crt')
    const key = join(pinnedDir, 'client.key')
    const ca = join(pinnedDir, 'ca.crt')
    if (existsSync(cert) && existsSync(key) && existsSync(ca)) {
      const [c, k, a] = await Promise.all([
        readFile(cert, 'utf-8'),
        readFile(key, 'utf-8'),
        readFile(ca, 'utf-8'),
      ])
      return { ca: a, cert: c, key: k }
    }
  }

  // Letter-based discovery
  const dir = join(TEAMX_HOME, 'letters')
  if (!existsSync(dir)) return null
  let entries: string[] = []
  try {
    entries = readdirSync(dir)
  } catch {
    return null
  }
  const wantedHost = serverUrl ? hostOf(serverUrl) : ''
  let best: { sub: string; mtime: number } | null = null
  let bestHostMatch: { sub: string; mtime: number } | null = null
  for (const id of entries) {
    const sub = join(dir, id)
    const letterPath = join(sub, 'letter.json')
    const cert = join(sub, 'client.crt')
    const key = join(sub, 'client.key')
    const ca = join(sub, 'ca.crt')
    if (!existsSync(letterPath) || !existsSync(cert) || !existsSync(key) || !existsSync(ca)) {
      continue
    }
    try {
      const letter = JSON.parse(readFileSync(letterPath, 'utf8')) as {
        teamx_invitation?: { server?: { url?: string } }
      }
      const host = hostOf(letter.teamx_invitation?.server?.url ?? '')
      const mtime = Math.max(
        statSync(letterPath).mtimeMs,
        statSync(cert).mtimeMs,
        statSync(key).mtimeMs,
        statSync(ca).mtimeMs,
      )
      if (wantedHost && host === wantedHost) {
        // Exact host match — keep the NEWEST such letter (a host may have
        // several letters across teams; readdir order is not meaningful).
        if (!bestHostMatch || mtime > bestHostMatch.mtime) {
          bestHostMatch = { sub, mtime }
        }
        continue
      }
      if (!best || mtime > best.mtime) best = { sub, mtime }
    } catch {
      // skip unreadable letters
    }
  }
  const chosen = bestHostMatch ?? best
  if (!chosen) return null
  const [c, k, a] = await Promise.all([
    readFile(join(chosen.sub, 'client.crt'), 'utf-8'),
    readFile(join(chosen.sub, 'client.key'), 'utf-8'),
    readFile(join(chosen.sub, 'ca.crt'), 'utf-8'),
  ])
  return { ca: a, cert: c, key: k }
}

// ---------------------------------------------------------------------------
// Member cache
// ---------------------------------------------------------------------------

export interface MemberInfo {
  /** Whether this agent is a known teamx member. */
  isMember: boolean
  /** teamx member id (from sync: teams[].team.my_member_id). */
  memberId?: string
  /** team id the agent belongs to (first team). */
  teamId?: string
  /** display name (from team list: teams[].name is team name; member name resolved from status). */
  name?: string
  role?: string
}

const members = new Map<string, MemberInfo>()

/** Mark an agent as a teamx member (or not). */
export function markMember(agentId: string, isMember: boolean, info?: Partial<MemberInfo>): void {
  if (isMember) {
    const prev = members.get(agentId)
    members.set(agentId, { isMember: true, ...prev, ...info })
  } else {
    members.set(agentId, { isMember: false })
  }
}

export function memberStatus(agentId: string): MemberInfo | null {
  return members.get(agentId) ?? null
}

/** Agent ids currently known to be teamx members (for the poller/WS loop). */
export function knownMemberSessions(): string[] {
  const out: string[] = []
  for (const [agentId, info] of members) {
    if (info.isMember) out.push(agentId)
  }
  return out
}

export function clearMemberCache(agentId?: string): void {
  if (agentId) members.delete(agentId)
  else members.clear()
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
  // Network mode: HTTP RPC (explicit env or discovered from imported letter)
  const serverUrl = resolveServerUrl()
  if (serverUrl) {
    return runRpc(args, serverUrl)
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
  data?: any
  error?: string
}

/** kebab/snake normalize: `goal-title` → `goal_title`, `no-advance` → `no_advance`. */
function toSnake(s: string): string {
  return s.replace(/-/g, '_')
}

/**
 * Convert a V1-style CLI arg vector into an RPC { method, args } payload for
 * network mode. Mirrors opencode-plugin's cliArgsToRpc.
 *
 * CLI shape: `[group] <subcommand> [positional...] [--flag value | --flag]...`
 * Positional slots are mapped to their RPC field names per method.
 */
export function cliArgsToRpc(argv: string[]): { method: string; args: Record<string, string | boolean | number | null> } {
  const positional: string[] = []
  const flags: Record<string, string | boolean | number | null> = {}
  let i = 0
  while (i < argv.length) {
    const a = argv[i]
    if (a.startsWith('--')) {
      const key = toSnake(a.slice(2))
      const next = argv[i + 1]
      if (next !== undefined && !next.startsWith('--')) {
        flags[key] = next
        i += 2
      } else {
        flags[key] = true
        i += 1
      }
    } else {
      positional.push(a)
      i += 1
    }
  }

  // Determine the dotted method from the first two positional tokens.
  // Grouped commands (team.*, goal.*, member.*, role.*, loopx.*, tunnel.*,
  // activity.*, discussion.*) use a two-token method; top-level commands
  // (publish/ask/respond/events/log/sync) treat every positional as a parameter.
  const GROUPED = new Set(['team', 'goal', 'member', 'role', 'loopx', 'tunnel', 'activity', 'discussion'])
  const p0 = positional[0] ?? ''
  const p1 = positional[1] ?? ''
  let method: string
  let rest: string[]
  if (GROUPED.has(p0) && p1) {
    method = `${p0}.${toSnake(p1)}`
    rest = positional.slice(2)
  } else {
    method = p0
    rest = positional.slice(1)
  }

  const args: Record<string, string | boolean | number | null> = { ...flags }

  // Map positional parameters to their RPC field names per method.
  // Only commands with positional args are listed; flag-only commands
  // (sync/events/log/team.list/team.status/...) need no slots.
  const slots: Record<string, string[]> = {
    'team.create': ['name'],
    'team.join': ['token', 'name'],
    'team.approve': ['member_id'],
    'team.deny': ['member_id'],
    'team.invite': ['role_desc'],
    'team.invite_revoke': ['id'],
    'team.import': ['letter'],
    'goal.set': ['title', 'body'],
    'member.set_state': ['state'],
    'member.discussion_quota': ['action'],
    'role.set': ['role'],
    'role.propose': ['role', 'label', 'description'],
     'role.approve': ['role'],
     'role.deny': ['role'],
     'role.update': ['role'],
     // Keep in sync with opencode-plugin/src/client.ts — these were missing and
     // silently dropped the positional args in network mode.
     'loopx.report': ['project'],
     'tunnel.status': ['name'],
     'tunnel.close': ['name'],
     'publish': ['type'],
     'ask': ['member_id'],
     'respond': ['ask_id', 'answer'],
     // discussions (server RPC): threads + messages (network mode).
     // Mirrors opencode-plugin/src/client.ts — keep in sync.
     'discussion.create': ['title'],
     'discussion.list': [],
     'discussion.open': ['id'],
     'discussion.say': ['id', 'body'],
     'discussion.reply': ['id', 'message_id', 'body'],
     'discussion.close': ['id'],
     'discussion.subscribe': ['id'],
     'discussion.unsubscribe': ['id'],
  }
  const fieldNames = slots[method] ?? []
  for (let k = 0; k < rest.length && k < fieldNames.length; k++) {
    args[fieldNames[k]] = rest[k]
  }

  return { method, args }
}

async function runRpc(args: string[], serverUrl: string): Promise<any> {
  const { method, args: params } = cliArgsToRpc(args)

  const mtls = await mtlsFor()
  const body = JSON.stringify({
    method,
    args: params,
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
          if (parsed.ok) resolve(parsed.data)
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

