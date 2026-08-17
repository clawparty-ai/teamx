// Unified CLI invocation layer for the teamx plugin.
//
// V1 runs the `teamx` binary directly (single-machine, no server). Every call
// shells out to `teamx <cmd> ... --json`. For V2 this module is the single
// seam to replace with an HTTP client against `teamx serve`.

import { existsSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from "node:fs"
import { homedir } from "node:os"
import { join } from "node:path"

export const TEAMX_HOME = process.env.TEAMX_HOME ?? join(homedir(), ".teamx")

/** Network-mode server URL (e.g. ws://host:5781 or https://host). Unset = V1 CLI mode. */
export const TEAMX_SERVER_URL = process.env.TEAMX_SERVER_URL ?? ""

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

/** kebab-case → snake_case: `goal-title` → `goal_title`, `no-advance` → `no_advance`. */
function toSnake(s: string): string {
  return s.replace(/-/g, "_")
}

// ---------------------------------------------------------------------------
// mTLS transport material (network mode, I1).
//
// The server requires mutual TLS: every RPC must present a client certificate
// signed by the team's CA. This material comes from an imported invitation
// letter (stored under ~/.teamx/letters/<invitation_id>/) or, for the owner,
// a self-issued cert. Explicit env vars win over letter auto-discovery.
// ---------------------------------------------------------------------------

interface MtlsMaterial {
  cert: string
  key: string
  ca: string
  serverName: string
}

function readPem(path: string): string {
  return readFileSync(path, "utf8")
}

/** Env override: TEAMX_MTLS_CERT/KEY/CA point at PEM files. */
function envMtls(serverName: string): MtlsMaterial | null {
  const cert = process.env.TEAMX_MTLS_CERT
  const key = process.env.TEAMX_MTLS_KEY
  const ca = process.env.TEAMX_MTLS_CA
  if (!cert || !key || !ca) return null
  if (!existsSync(cert) || !existsSync(key) || !existsSync(ca)) return null
  return { cert: readPem(cert), key: readPem(key), ca: readPem(ca), serverName }
}

/** Host portion of a URL (for SNI / letter matching). */
function hostOf(url: string): string {
  try {
    return new URL(url).hostname
  } catch {
    return url
  }
}

/**
 * Auto-discover the mTLS material from imported invitation letters under
 * `~/.teamx/letters/<id>/`. Prefers the letter whose embedded server URL
 * matches the configured server; otherwise falls back to the most recent one.
 */
function letterMtls(serverUrl: string): MtlsMaterial | null {
  const dir = join(TEAMX_HOME, "letters")
  if (!existsSync(dir)) return null
  let entries: string[] = []
  try {
    entries = readdirSync(dir)
  } catch {
    return null
  }
  const wantedHost = hostOf(serverUrl)
  let best: { cert: string; key: string; ca: string; host: string; mtime: number } | null = null
  for (const id of entries) {
    const sub = join(dir, id)
    const letterPath = join(sub, "letter.json")
    if (!existsSync(letterPath)) continue
    const certPath = join(sub, "client.crt")
    const keyPath = join(sub, "client.key")
    const caPath = join(sub, "ca.crt")
    if (!existsSync(certPath) || !existsSync(keyPath) || !existsSync(caPath)) continue
    let host = ""
    let mtime = 0
    try {
      const letter = JSON.parse(readFileSync(letterPath, "utf8")) as {
        teamx_invitation?: { server?: { url?: string } }
      }
      host = hostOf(letter.teamx_invitation?.server?.url ?? "")
      for (const f of ["letter.json", "client.crt", "client.key", "ca.crt"]) {
        mtime = Math.max(mtime, statSync(join(sub, f)).mtimeMs)
      }
    } catch {
      continue
    }
    const cand = { cert: certPath, key: keyPath, ca: caPath, host, mtime }
    if (host && host === wantedHost) {
      // exact host match wins immediately
      return {
        cert: readPem(cand.cert),
        key: readPem(cand.key),
        ca: readPem(cand.ca),
        serverName: host,
      }
    }
    if (!best || cand.mtime > best.mtime) best = cand
  }
  if (!best) return null
  return {
    cert: readPem(best.cert),
    key: readPem(best.key),
    ca: readPem(best.ca),
    serverName: best.host || wantedHost || hostOf(serverUrl),
  }
}

/** Resolve the mTLS material for a given server URL (or null if unavailable). */
export function mtlsFor(serverUrl: string): MtlsMaterial | null {
  const host = hostOf(serverUrl)
  return envMtls(host) ?? letterMtls(serverUrl)
}

/**
 * Convert a V1-style CLI arg vector into an RPC { method, args } payload for
 * network mode. Handles `team.status --team x --session key` style vectors.
 *
 * CLI shape: `[group] <subcommand> [positional...] [--flag value | --flag]...`
 */
export function cliArgsToRpc(argv: string[]): { method: string; args: Record<string, string | boolean | number | null> } {
  const positional: string[] = []
  const flags: Record<string, string | boolean | number | null> = {}
  let i = 0
  while (i < argv.length) {
    const a = argv[i]
    if (a.startsWith("--")) {
      // Normalize the flag name so `--goal-title` and `--no-advance` arrive as
      // `goal_title` / `no_advance`, matching the RPC field names.
      const key = toSnake(a.slice(2))
      const next = argv[i + 1]
      if (next !== undefined && !next.startsWith("--")) {
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
  // Only grouped commands (team.*, goal.*, member.*, role.*, loopx.*) use a
  // two-token method; top-level commands (publish/ask/respond/events/log/sync)
  // treat every positional as a parameter. Subcommand names are normalized so
  // `member set-state` maps to the RPC method `member.set_state`.
  const GROUPED = new Set(["team", "goal", "member", "role", "loopx", "tunnel"])
  const p0 = positional[0] ?? ""
  const p1 = positional[1] ?? ""
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
  // Note: for publish/ask the data/question arrive via --data/--question flags,
  // so only the leading positional is listed here.
  const slots: Record<string, string[]> = {
    "team.create": ["name"],
    "team.join": ["token", "name"],
    "team.approve": ["member_id"],
    "team.deny": ["member_id"],
    "team.invite": ["role_desc"],
    "team.invite_revoke": ["id"],
    "team.import": ["letter"],
    "goal.set": ["title", "body"],
    "member.set_state": ["state"],
    "role.set": ["role"],
    "role.propose": ["role", "label", "description"],
    "role.approve": ["role"],
    "role.deny": ["role"],
    "role.update": ["role"],
    "publish": ["type"],
    "ask": ["member_id"],
    "respond": ["ask_id", "answer"],
    "loopx.report": ["project"],
    "tunnel.list": [],
    "tunnel.status": ["name"],
    "tunnel.close": ["name"],
  }
  const fieldNames = slots[method] ?? []
  for (let k = 0; k < rest.length && k < fieldNames.length; k++) {
    args[fieldNames[k]] = rest[k]
  }

  return { method, args }
}

/**
 * Run an RPC call against the configured network-mode server.
 * The plugin normally passes V1-style CLI args; this translates and posts them.
 */
export async function runRpc(argv: string[]): Promise<CliResult> {
  const { method, args } = cliArgsToRpc(argv)
  const base = TEAMX_SERVER_URL.replace(/\/$/, "")
  const mtls = mtlsFor(TEAMX_SERVER_URL)
  try {
    const init: RequestInit & { tls?: Record<string, unknown> } = {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ method, args }),
      signal: AbortSignal.timeout(30_000),
    }
    if (mtls) {
      init.tls = {
        cert: mtls.cert,
        key: mtls.key,
        ca: mtls.ca,
        serverName: mtls.serverName,
      }
    }
    const res = await fetch(`${base}/rpc`, init)
    const text = await res.text()
    let data: Record<string, unknown> | null = null
    try {
      data = JSON.parse(text)
    } catch {
      data = null
    }
    if (!res.ok || data?.ok !== true) {
      const errMsg = (data as { error?: string })?.error ?? `HTTP ${res.status}`
      return { ok: false, stdout: "", stderr: errMsg, data: null }
    }
    const inner = (data as { data?: unknown }).data
    return {
      ok: true,
      stdout: typeof inner === "string" ? inner : JSON.stringify(inner),
      stderr: "",
      data: (inner as Record<string, unknown>) ?? null,
    }
  } catch (e) {
    return { ok: false, stdout: "", stderr: String(e), data: null }
  }
}

/**
 * Run a teamx CLI invocation via Bun.spawn and parse the JSON output.
 * When TEAMX_SERVER_URL is set, the invocation is sent as an RPC instead.
 * Non-zero exits are surfaced as `{ ok: false, stderr }` instead of throwing.
 * A 30s timeout guards against a hung `teamx` subprocess.
 */
export async function runCli(args: string[], opts?: { cwd?: string }): Promise<CliResult> {
  if (TEAMX_SERVER_URL) {
    // Network mode: strip the trailing `--json` that the plugin appends; the
    // RPC transport always speaks JSON.
    const clean = args.filter((a) => a !== "--json")
    return runRpc(clean)
  }
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
