/**
 * teamx deepseek-harness plugin: multi-agent team collaboration.
 *
 * Mirrors opencode-plugin's core functionality for dsh:
 * - Registers teamx_* tools via ctx.tools.register(defineTool(...))
 * - Registers /team-* flat slash commands via ctx.commands.register
 * - Provides per-agent system prompt injection (digest) via ctx.systemPrompt.variable
 * - Auto-execute directed tasks via agent.followup()
 * - M2 poller + WS push for real-time event notifications
 *
 * @module @teamx-ai/dsh-plugin
 */

import type { Context } from '@deepseek-ai/cordis'
// Type-only: pulls in the dsh Context augmentations (ctx.tools, ctx.systemPrompt, ctx.commands, agent events).
import type {} from '@deepseek-ai/dsh-tools'
import type {} from '@deepseek-ai/dsh-system-prompt'
import type {} from '@deepseek-ai/dsh-commands'
import type {} from '@deepseek-ai/dsh-skill'
import type {} from '@deepseek-ai/dsh-agent'
import { registerTeamTools, registerGoalTools, registerMemberTools, registerInteractionTools, registerDiscussionTools } from './tools.js'
import { registerTaskTools, registerGitTools, registerServeTools, registerConsoleTools, registerTunnelTools, registerMiscTools, registerNodeTools } from './tools-extra.js'
import { registerCommands } from './commands.js'
import { TEAMX_SKILL } from './skill.js'
import { TEAMX_GRILL_WITH_DOCS_SKILL } from './grill-with-docs.generated.js'
import { refreshDigest, clearDigest, getDigest } from './digest.js'
import { registerAgent, unregisterAgent, processEvents, agentSessionKey } from './auto-execute.js'
import { createWsClient } from './ws.js'
import { instanceId, sessionKey, runCli, markMember, memberStatus, knownMemberSessions, resolveServerUrl } from './client.js'

// ---------------------------------------------------------------------------
// Plugin config (loaded from cordis config tree)
// ---------------------------------------------------------------------------

export interface Config {
  /** Team name to auto-join on startup (optional). */
  team?: string
  /** Polling interval in ms for digest refresh (default: 15000; 0 disables). */
  pollIntervalMs?: number
  /** Enable WebSocket push (default: true when TEAMX_SERVER_URL is set). */
  wsEnabled?: boolean
}

// ---------------------------------------------------------------------------
// Plugin entry
// ---------------------------------------------------------------------------

const POLL_DEFAULT_MS = 15_000

/**
 * Minimum interval between `session.idle` heartbeat publishes, process-wide
 * (ms). Every session in one dsh process connects with the same mTLS client
 * cert — i.e. the SAME team member — so an unthrottled per-session heartbeat
 * writes N duplicate `session.idle` rows into the team ledger on every idle
 * wave (one per session). One process-wide heartbeat per window is enough
 * for presence; the digest refresh below stays per-idle and is NOT throttled.
 *
 * Override with TEAMX_IDLE_HEARTBEAT_MIN_MS; 0 restores the old per-session
 * behavior.
 */
const IDLE_HEARTBEAT_MIN_MS = (() => {
  const fallback = 10 * 60_000
  const raw = Number(process.env.TEAMX_IDLE_HEARTBEAT_MIN_MS ?? fallback)
  return Number.isFinite(raw) && raw >= 0 ? raw : fallback
})()

/** Last time a session.idle heartbeat was published by this process. */
let lastIdlePublishAt = 0

/** How often the WS watcher re-resolves the server URL (late-letter pickup).
 *  Override with TEAMX_WS_WATCH_INTERVAL (ms, must be > 0); default 10s —
 *  same env name as the opencode-plugin side. */
const WS_WATCH_INTERVAL_MS = (() => {
  const fallback = 10_000
  const raw = Number(process.env.TEAMX_WS_WATCH_INTERVAL ?? fallback)
  return Number.isFinite(raw) && raw > 0 ? raw : fallback
})()

/** Event types that are directed task assignments (auto-execute candidates). */
function isDirectedTask(event: any): boolean {
  const data = event?.data || event?.payload || event
  return !!(data?.assignee_member_id || data?.assignee)
}

/**
 * Cordis dependency declaration.
 *
 * MUST be a module-level export: the published `lib/index.js` is a pure ESM
 * bundle, and cordis's `unwrapExports()` reads `inject` off the module
 * namespace. Declaring it only on `apply` (as some CJS loaders allow) left
 * `namespace.inject` undefined, crashing startup with
 * "cannot get property tools without inject".
 *
 * Kept in sync with `apply.inject` below for loaders that read the function
 * property instead of the namespace.
 */
export const inject = ['tools', 'commands', 'systemPrompt', 'skills']

export function apply(ctx: Context, config: Config = {}): void {
  const pollIntervalMs = config.pollIntervalMs || POLL_DEFAULT_MS
  let wsClient: ReturnType<typeof createWsClient> | null = null
  const instance = instanceId()

  // -----------------------------------------------------------------------
  // Bootstrap: register tools and commands (runs immediately in apply())
  // -----------------------------------------------------------------------
  registerTeamTools(ctx)
  registerGoalTools(ctx)
  registerMemberTools(ctx)
  registerInteractionTools(ctx)
  registerDiscussionTools(ctx)
  // Tool alignment batch 2 (task-9): tasks/git/serve/console/tunnels/misc —
  // brings dsh-plugin to parity with opencode-plugin's tool surface.
  registerTaskTools(ctx)
  registerGitTools(ctx)
  registerServeTools(ctx)
  registerConsoleTools(ctx)
  registerTunnelTools(ctx)
  registerMiscTools(ctx)
  registerNodeTools(ctx)
  registerCommands(ctx)
  ctx.effect(() => ctx.skills.register(TEAMX_SKILL))
  ctx.effect(() => ctx.skills.register(TEAMX_GRILL_WITH_DOCS_SKILL))
  console.log('[teamx-dsh] Plugin loaded, tools, commands and skills registered')

  // -----------------------------------------------------------------------
  // Per-agent lifecycle: check membership, inject digest
  // -----------------------------------------------------------------------
  ctx.on('agent/session-start', async (event) => {
    const agent = event.agent
    const agentId = agent.session.id
    const key = sessionKey(instance, agentId)

    // Check team membership via `team list` (returns { teams: [...] })
    try {
      const result = await runCli(['team', 'list', '--session', key])
      const teams = result?.teams
      if (Array.isArray(teams)) {
        if (teams.length > 0) {
          const first = teams[0]
          // team_id, name, state, my_role, my_state, goal, invite_token
          markMember(agentId, true, {
            teamId: first.team_id,
            name: first.name,
            role: first.my_role,
          })
          registerAgent(agentId, agent, first.team_id)
          console.log(`[teamx-dsh] Agent ${agentId} is a member of team ${first.name} (${first.team_id})`)
        } else {
          markMember(agentId, false)
        }
      }
    } catch {
      // Not in any team yet — that's fine
      markMember(agentId, false)
    }

    // Register system prompt variable for digest (scoped to this agent)
    agent.ctx.systemPrompt.variable('teamx_digest', () => getDigest(agentId))
    agent.ctx.systemPrompt.section({
      name: 'teamx:digest',
      order: 150,
      text: '{{teamx_digest}}',
    })

    // Initial digest refresh
    await refreshDigest(agentId, key)

    console.log(`[teamx-dsh] Agent ${agentId} started, digest injected`)
  })

  // -----------------------------------------------------------------------
  // Agent status changes: heartbeat + digest refresh on idle
  // (dsh emits agent/status with status 'idle' | 'running')
  // -----------------------------------------------------------------------
  ctx.on('agent/status', async (event) => {
    const agent = event.agent
    const agentId = agent.session.id
    if (event.status !== 'idle') return

    // Only members heartbeat (same guard as opencode-plugin's session.idle)
    const isMember = memberStatus(agentId)?.isMember === true
    if (!isMember) return

    // Publish heartbeat (same as opencode-plugin's session.idle activity row).
    // Throttled process-wide (IDLE_HEARTBEAT_MIN_MS): all sessions in this
    // process share one mTLS member identity, so per-session heartbeats are
    // N duplicate ledger rows on every idle wave.
    if (Date.now() - lastIdlePublishAt >= IDLE_HEARTBEAT_MIN_MS) {
      try {
        await runCli([
          'publish', 'activity',
          '--data', JSON.stringify({ kind: 'session.idle' }),
          '--session', sessionKey(instance, agentId),
        ])
        lastIdlePublishAt = Date.now()
      } catch {
        // heartbeat failure is non-critical
      }
    }

    // Refresh digest on idle
    try {
      await refreshDigest(agentId, sessionKey(instance, agentId))
    } catch {
      // digest refresh failure is non-critical
    }
  })

  // -----------------------------------------------------------------------
  // Agent teardown (event name is `agent/disposed`)
  // -----------------------------------------------------------------------
  ctx.on('agent/disposed', (event) => {
    const agentId = event.agent?.session?.id
    if (agentId) {
      unregisterAgent(agentId)
      clearDigest(agentId)
      markMember(agentId, false)
    }
  })

  // -----------------------------------------------------------------------
  // Refresh digest + process new events + auto-execute for one member agent
  // -----------------------------------------------------------------------
  async function refreshAgent(agentId: string): Promise<void> {
    const key = agentSessionKey(agentId)
    const result = await runCli(['sync', '--no-advance', '--session', key])
    if (!result) return
    // Update member id from sync (teams[0].team.my_member_id) for auto-execute matching
    const teams = result.teams
    if (Array.isArray(teams) && teams.length > 0) {
      const teamBlock = teams[0]
      const myMemberId = teamBlock?.team?.my_member_id
      if (myMemberId) {
        markMember(agentId, true, {
          teamId: teamBlock.team.id,
          memberId: myMemberId,
        })
      }
    }
    // Refresh digest
    await refreshDigest(agentId, key)
    // Process new events (directed tasks → auto-execute)
    const newEvents = Array.isArray(result.new_events) ? result.new_events : []
    if (newEvents.length > 0) {
      const tasks = newEvents.filter(isDirectedTask)
      if (tasks.length > 0) {
        await processEvents(agentId, tasks)
      }
    }
  }

  async function refreshAll(): Promise<void> {
    for (const agentId of knownMemberSessions()) {
      try {
        await refreshAgent(agentId)
      } catch {
        // per-agent refresh failure is non-critical
      }
    }
  }

  // -----------------------------------------------------------------------
  // M2 poller: refresh digest + auto-execute for every member session.
  // Registered as a ctx.effect so it is cleaned up automatically on dispose.
  // -----------------------------------------------------------------------
  ctx.effect(() => {
    if (pollIntervalMs <= 0) return
    const poller = setInterval(() => {
      refreshAll().catch(() => {})
    }, pollIntervalMs)
    return () => clearInterval(poller)
  })

  // -----------------------------------------------------------------------
  // WS push: always-on watcher (Issue #6).
  //
  // The server URL is re-resolved every few seconds instead of once at
  // startup, so an invitation letter imported into a RUNNING dsh process
  // (the common cross-machine onboarding path) establishes /ws without a
  // restart; a changed URL (newer letter) rebuilds the client, and losing
  // all letters stops it. In-connection resilience (transient drops, cert
  // not provisioned yet, seat pending approval) lives in WsClient itself:
  // exponential backoff + jitter, retryable auth-not-ready errors.
  // -----------------------------------------------------------------------
  const wsEnabled = config.wsEnabled !== false

  ctx.effect(() => {
    if (!wsEnabled) return
    let disposed = false
    let starting = false
    let currentUrl = ''

    const ensureWs = async () => {
      if (disposed || starting) return
      const url = resolveServerUrl()
      if (!url) {
        if (wsClient) {
          try {
            wsClient.stop()
          } catch {
            // ignore
          }
          wsClient = null
          console.warn('[teamx-dsh] WS push stopped: no server URL (no invitation letter)')
        }
        currentUrl = ''
        return
      }
      if (wsClient && url === currentUrl) return
      starting = true
      try {
        if (wsClient) {
          try {
            wsClient.stop()
          } catch {
            // ignore
          }
          wsClient = null
        }
        currentUrl = url
        // WS identity comes from the mTLS client cert CN, so no team/session
        // params are needed. We still sync once to warm the member cache
        // (so the poller/auto-execute has member ids) before connecting.
        for (const agentId of knownMemberSessions()) {
          try {
            await refreshAgent(agentId)
          } catch {
            // ignore
          }
        }
        if (disposed) return
        const client = createWsClient({ serverUrl: url })
        wsClient = client
        client.on('event', () => {
          // Any new event → refresh digest + auto-execute for all members
          refreshAll().catch(() => {})
        })
        await client.start()
        console.log(`[teamx-dsh] WS push client started (${url})`)
      } catch (err) {
        console.warn('[teamx-dsh] WS push failed:', err)
      } finally {
        starting = false
      }
    }

    void ensureWs()
    const watcher = setInterval(() => {
      void ensureWs()
    }, WS_WATCH_INTERVAL_MS)
    return () => {
      disposed = true
      clearInterval(watcher)
      if (wsClient) {
        wsClient.stop()
        wsClient = null
      }
    }
  })
}

// Back-compat: expose the same declaration on `apply` for loaders that read
// the static property off the plugin function rather than the ESM namespace.
;(apply as unknown as { inject?: readonly string[] }).inject = inject
