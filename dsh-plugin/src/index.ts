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
import type {} from '@deepseek-ai/dsh-agent'
import { registerTeamTools, registerGoalTools, registerMemberTools, registerInteractionTools } from './tools.js'
import { registerCommands } from './commands.js'
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

/** Event types that are directed task assignments (auto-execute candidates). */
function isDirectedTask(event: any): boolean {
  const data = event?.data || event?.payload || event
  return !!(data?.assignee_member_id || data?.assignee)
}

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
  registerCommands(ctx)
  console.log('[teamx-dsh] Plugin loaded, tools and commands registered')

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

    // Publish heartbeat (same as opencode-plugin's session.idle activity row)
    try {
      await runCli([
        'publish', 'activity',
        '--data', JSON.stringify({ kind: 'session.idle' }),
        '--session', sessionKey(instance, agentId),
      ])
    } catch {
      // heartbeat failure is non-critical
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
  // Start WS push (if network mode and enabled)
  // -----------------------------------------------------------------------
  const serverUrl = resolveServerUrl()
  const wsEnabled = config.wsEnabled !== false

  if (serverUrl && wsEnabled) {
    ctx.effect(() => {
      let disposed = false
      const start = async () => {
        if (disposed) return
        try {
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

          wsClient = createWsClient({ serverUrl })
          wsClient.on('event', () => {
            // Any new event → refresh digest + auto-execute for all members
            refreshAll().catch(() => {})
          })
          await wsClient.start()
          console.log('[teamx-dsh] WS push client started')
        } catch (err) {
          console.warn('[teamx-dsh] WS push failed:', err)
        }
      }
      start()
      return () => {
        disposed = true
        if (wsClient) {
          wsClient.stop()
          wsClient = null
        }
      }
    })
  }
}
