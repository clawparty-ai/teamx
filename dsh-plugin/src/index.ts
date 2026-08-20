/**
 * teamx deepseek-harness plugin: multi-agent team collaboration.
 *
 * This plugin mirrors opencode-plugin's core functionality for dsh:
 * - Registers teamx_* tools via ctx.tools.register(defineTool(...))
 * - Registers /team-* flat slash commands via ctx.commands.register
 * - Provides per-agent system prompt injection (digest) via ctx.systemPrompt.variable
 * - Auto-execute directed tasks via agent.followup()
 * - WS push + poller for real-time event notifications
 *
 * @module @teamx/dsh-plugin
 */

import type { Context } from '@deepseek-ai/cordis'
import { registerTeamTools, registerGoalTools, registerMemberTools, registerInteractionTools } from './tools.js'
import { registerCommands } from './commands.js'
import { refreshDigest, clearDigest, getDigest } from './digest.js'
import { registerAgent, unregisterAgent, processEvents } from './auto-execute.js'
import { createWsClient } from './ws.js'
import { instanceId, sessionKey, runCli, markMember, knownMemberSessions } from './client.js'

// ---------------------------------------------------------------------------
// Plugin config (loaded from cordis config tree)
// ---------------------------------------------------------------------------

export interface Config {
  /** Team name to auto-join on startup (optional). */
  team?: string
  /** Polling interval in ms for digest refresh (default: 15000). */
  pollIntervalMs?: number
  /** Enable WebSocket push (default: true when TEAMX_SERVER_URL is set). */
  wsEnabled?: boolean
}

// ---------------------------------------------------------------------------
// Plugin entry
// ---------------------------------------------------------------------------

const POLL_DEFAULT_MS = 15_000

export function apply(ctx: Context, config: Config = {}): void {
  const pollIntervalMs = config.pollIntervalMs || POLL_DEFAULT_MS
  let poller: ReturnType<typeof setInterval> | null = null
  let wsClient: ReturnType<typeof createWsClient> | null = null
  let teamxInstance = ''

  // -----------------------------------------------------------------------
  // Bootstrap: discover instance ID and register tools/commands
  // -----------------------------------------------------------------------
  ctx.on('ready', async () => {
    try {
      teamxInstance = await instanceId()
    } catch {
      console.warn('[teamx-dsh] Could not read teamx instance ID')
    }

    // Register all tools
    registerTeamTools(ctx)
    registerGoalTools(ctx)
    registerMemberTools(ctx)
    registerInteractionTools(ctx)

    // Register all slash commands
    registerCommands(ctx)

    console.log('[teamx-dsh] Plugin loaded, tools and commands registered')
  })

  // -----------------------------------------------------------------------
  // Per-agent lifecycle: check membership, inject digest, start poller
  // -----------------------------------------------------------------------
  ctx.on('agent/session-start', async (event) => {
    const agent = event.agent
    const agentId = agent.session.id
    const key = sessionKey(teamxInstance, agentId)

    // Check team membership
    try {
      const result = await runCli(['team', 'list', '--session', key])
      if (Array.isArray(result)) {
        for (const team of result) {
          markMember(team.id, agentId, team.name, team.role || 'member')
          registerAgent(agentId, agent, team.id)
        }
      }
    } catch {
      // Not in any team yet — that's fine
    }

    // Register system prompt variable for digest
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
  // Agent status changes: heartbeat on idle
  // -----------------------------------------------------------------------
  ctx.on('agent/status', async (event) => {
    const agent = event.agent
    const agentId = agent.session.id
    const status = event.status

    if (status === 'idle') {
      // Publish heartbeat (same as opencode-plugin's session.idle)
      try {
        const key = sessionKey(teamxInstance, agentId)
        await runCli([
          'publish', 'activity',
          '--data', JSON.stringify({ kind: 'session.idle' }),
          '--session', key,
        ])
      } catch {
        // heartbeat failure is non-critical
      }

      // Refresh digest on idle
      try {
        await refreshDigest(agentId, sessionKey(teamxInstance, agentId))
      } catch {
        // digest refresh failure is non-critical
      }
    }
  })

  // -----------------------------------------------------------------------
  // Agent teardown
  // -----------------------------------------------------------------------
  ctx.on('agent/created', (event) => {
    // no-op, session-start handles setup
  })

  ctx.on('agent/dispose', (event) => {
    const agentId = event.agent?.session?.id
    if (agentId) {
      unregisterAgent(agentId)
      clearDigest(agentId)
    }
  })

  // -----------------------------------------------------------------------
  // Start poller for digest refresh + event processing
  // -----------------------------------------------------------------------
  ctx.on('ready', () => {
    poller = setInterval(async () => {
      const sessions = knownMemberSessions('')
      for (const sessKey of sessions) {
        try {
          const events = await runCli(['events', '--session', sessKey, '--since', '0'])
          if (Array.isArray(events) && events.length > 0) {
            // Process auto-execute for all agents
            for (const [agentId] of (globalThis as any).__teamxAgents || []) {
              await processEvents(agentId, events)
            }
          }
        } catch {
          // poller failure is non-critical
        }
      }
    }, pollIntervalMs)
  })

  // -----------------------------------------------------------------------
  // Start WS push (if network mode)
  // -----------------------------------------------------------------------
  if (process.env.TEAMX_SERVER_URL) {
    ctx.on('ready', async () => {
      try {
        const sessions = knownMemberSessions('')
        if (sessions.length > 0) {
          const firstSession = sessions[0]
          wsClient = createWsClient({
            serverUrl: process.env.TEAMX_SERVER_URL!,
            team: '',
            session: firstSession,
          })
          wsClient.on('event', async (event: any) => {
            // Refresh digest and process auto-execute on any event
            for (const [agentId] of (globalThis as any).__teamxAgents || []) {
              await refreshDigest(agentId, sessionKey(teamxInstance, agentId))
              await processEvents(agentId, [event])
            }
          })
          await wsClient.start()
          console.log('[teamx-dsh] WS push connected')
        }
      } catch (err) {
        console.warn('[teamx-dsh] WS push failed:', err)
      }
    })
  }

  // -----------------------------------------------------------------------
  // Cleanup on context dispose
  // -----------------------------------------------------------------------
  ctx.on('dispose', () => {
    if (poller) {
      clearInterval(poller)
      poller = null
    }
    if (wsClient) {
      wsClient.stop()
      wsClient = null
    }
    clearDigest()
  })
}
