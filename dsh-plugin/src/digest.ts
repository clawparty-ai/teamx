/**
 * Per-agent digest cache and sync refresh.
 * Mirrors opencode-plugin's digest.ts.
 * @module @teamx/dsh-plugin/digest
 */

import { runCli } from './client.js'

const digestCache = new Map<string, string>()

/**
 * Get the current digest text for an agent.
 * Returns empty string if no digest has been fetched yet.
 */
export function getDigest(agentId: string): string {
  return digestCache.get(agentId) || ''
}

/**
 * Refresh the digest for an agent by running `teamx sync`.
 * Parses the sync output into a compact prompt text.
 * Returns the new digest text.
 */
export async function refreshDigest(agentId: string, sessionKey: string): Promise<string> {
  try {
    const result = await runCli(['sync', '--no-advance', '--session', sessionKey])
    const digest = formatDigest(result)
    digestCache.set(agentId, digest)
    return digest
  } catch (err) {
    console.error(`[teamx-dsh] digest refresh failed for ${agentId}:`, err)
    return digestCache.get(agentId) || ''
  }
}

/**
 * Clear the digest cache for an agent or all agents.
 */
export function clearDigest(agentId?: string): void {
  if (agentId) digestCache.delete(agentId)
  else digestCache.clear()
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

interface SyncResult {
  teams?: Array<{
    team?: {
      id: string
      name: string
      state: string
    }
    goal?: {
      title: string
      body?: string
      state: string
    } | null
    members?: Array<{
      id: string
      name: string
      role: string
      state: string
    }>
    recent_events?: Array<{
      seq: number
      type: string
      data?: any
    }>
    questions?: Array<{ state?: string; question?: string }>
  }>
  new_events?: Array<{
    seq: number
    type: string
    data?: any
  }>
}

/**
 * Format a `teamx sync --no-advance` response into a compact digest.
 * The CLI returns `{ teams: [{ team, goal, members, questions, recent_events }], new_events }`.
 */
function formatDigest(data: any): string {
  if (!data || typeof data !== 'object') return ''

  const teams = Array.isArray(data.teams) ? data.teams : []
  if (teams.length === 0) return ''

  const lines: string[] = []
  for (const teamBlock of teams) {
    const team = teamBlock.team || {}
    const teamName = team.name || 'Team'
    const teamState = team.state || 'unknown'

    lines.push(`📋 **${teamName}** (status: ${teamState})`)

    // Goal
    const goal = teamBlock.goal
    if (goal) {
      const goalState = goal.state || 'unknown'
      lines.push(`🎯 **Goal** (${goalState}): ${goal.title || 'Untitled'}`)
      if (goal.body) {
        lines.push(`   ${goal.body.slice(0, 200)}${goal.body.length > 200 ? '…' : ''}`)
      }
    }

    // Members
    const members = teamBlock.members
    if (Array.isArray(members) && members.length > 0) {
      lines.push('👥 **Members**:')
      for (const m of members) {
        const state = m.state || 'active'
        const emoji = state === 'idle' ? '😴' : state === 'active' ? '⚡' : '❓'
        lines.push(`  ${emoji} ${m.name} (${m.role || 'member'}) — ${state}`)
      }
    }

    // Open questions
    const openQuestions = (teamBlock.questions || []).filter((q: any) => q?.state === 'open')
    if (openQuestions.length > 0) {
      lines.push(`❓ **Open questions** (${openQuestions.length})`)
    }

    // Recent events (last 3)
    const events = teamBlock.recent_events
    if (Array.isArray(events) && events.length > 0) {
      const recent = events.slice(-3)
      lines.push('📝 **Recent**:')
      for (const e of recent) {
        const msg = e.data?.message || e.data?.kind || e.type || 'event'
        lines.push(`  • ${msg}`)
      }
    }
  }

  return lines.join('\n')
}
