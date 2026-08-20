/**
 * Auto-execute: detect directed tasks and call agent.followup().
 * Mirrors opencode-plugin's auto-execute logic.
 * @module @teamx/dsh-plugin/auto-execute
 */

import type { Agent } from '@deepseek-ai/dsh-agent'
import { createUserMessage } from '@deepseek-ai/dsh-llm'
import { sessionKey, instanceId, memberStatus } from './client.js'
import { getDigest, refreshDigest } from './digest.js'

export interface AutoExecuteState {
  /** agentId → last processed sequence number */
  lastSeq: Map<string, number>
  /** agentId → Agent handle */
  agents: Map<string, Agent>
  /** agentId → teamId */
  agentTeam: Map<string, string>
}

const state: AutoExecuteState = {
  lastSeq: new Map(),
  agents: new Map(),
  agentTeam: new Map(),
}

/**
 * Register an agent for auto-execute tracking.
 */
export function registerAgent(agentId: string, agent: Agent, teamId: string): void {
  state.agents.set(agentId, agent)
  state.agentTeam.set(agentId, teamId)
}

/**
 * Unregister an agent.
 */
export function unregisterAgent(agentId: string): void {
  state.agents.delete(agentId)
  state.agentTeam.delete(agentId)
  state.lastSeq.delete(agentId)
}

/**
 * Get the session key for a registered agent.
 */
export function agentSessionKey(agentId: string): string {
  return sessionKey(instanceId(), agentId)
}

/**
 * Process incoming events and trigger auto-execute for directed tasks.
 * Called by the event loop (poller or WS push).
 * @param agentId - the agent to process events for.
 * @param events - new events from `sync --no-advance`.
 * @returns the number of auto-execute prompts triggered.
 */
export async function processEvents(agentId: string, events: any[]): Promise<number> {
  const agent = state.agents.get(agentId)
  if (!agent) return 0

  const teamId = state.agentTeam.get(agentId) || ''
  let triggered = 0

  for (const event of events) {
    const seq = event.seq || event.id || 0
    const lastSeq = state.lastSeq.get(agentId) || 0
    if (seq <= lastSeq) continue

    // Check if this event is a directed task for this member
    const data = event.data || event.payload || event
    const assignee = data.assignee_member_id || data.assignee
    if (!assignee) continue

    // Check if I'm the assignee (compare against my teamx member id)
    const myStatus = memberStatus(agentId)
    const myMemberId = myStatus?.memberId
    if (!myMemberId) continue
    if (assignee !== myMemberId) continue

    // Refresh digest first (using the full session key)
    await refreshDigest(agentId, agentSessionKey(agentId))

    // Build auto-execute message
    const eventMsg = data.message || data.kind || event.type || 'new task'
    const autoMsg = buildAutoExecMessage(eventMsg, getDigest(agentId))

    // Wake the agent
    try {
      const userMessage = createUserMessage({
        content: [{ type: 'text', text: autoMsg }],
        source: { kind: 'user' },
      })
      await agent.followup(userMessage)
      triggered++
      state.lastSeq.set(agentId, seq)
    } catch (err) {
      console.error(`[teamx-dsh] auto-execute followup failed for ${agentId}:`, err)
    }
  }

  return triggered
}

function buildAutoExecMessage(eventMsg: string, digest: string): string {
  const lines: string[] = [
    '⚡ **AUTO-EXECUTE: TASK ASSIGNED**',
    '',
    `You have been assigned a new task: ${eventMsg}`,
    '',
    'Sync your state before proceeding.',
  ]

  if (digest) {
    lines.push('')
    lines.push('---')
    lines.push(digest)
  }

  lines.push('')
  lines.push('⚠️  IMPORTANT: You MUST call teamx_sync NOW.')
  lines.push('After syncing, execute the assigned task. Do NOT ask questions.')
  lines.push('If you have a question, call teamx_ask.')

  return lines.join('\n')
}

/**
 * Get the auto-execute state (for testing).
 */
export function getAutoExecuteState(): Readonly<AutoExecuteState> {
  return state
}
