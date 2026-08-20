/**
 * teamx_* tools for dsh-plugin.
 * Each tool calls `runCli` to spawn the teamx binary (or HTTP RPC in network mode).
 * Session key is derived from `exec.agent.session.id` (dsh agent identity).
 * @module @teamx/dsh-plugin/tools
 */

import type { Context } from '@deepseek-ai/cordis'
import type { Agent } from '@deepseek-ai/dsh-agent'
import { defineTool } from '@deepseek-ai/dsh-tools'
import type { ToolRunContext } from '@deepseek-ai/dsh-tools'
import { runCli, sessionKey, instanceId, markMember, knownMemberSessions } from './client.js'

/** Get the teamx session key from a dsh tool execution context. */
function getKey(exec: ToolRunContext): string {
  return `${exec.agent.session.id}`
}

/** Build CLI args array from named options, filtering undefined values. */
function args(...pairs: (string | undefined | null)[]): string[] {
  const out: string[] = []
  for (let i = 0; i < pairs.length; i++) {
    const v = pairs[i]
    if (v != null && v !== '') {
      out.push(v)
      // next is the value for this flag
      if (i + 1 < pairs.length && pairs[i + 1] != null) {
        out.push(pairs[i + 1]!)
        i++
      }
    } else {
      i++ // skip the value too
    }
  }
  return out
}

// ---------------------------------------------------------------------------
// Team management
// ---------------------------------------------------------------------------

export function registerTeamTools(ctx: Context): void {
  ctx.tools.register(defineTool({
    name: 'teamx_create_team',
    description:
      'Create a new team. The current session becomes the team OWNER. Returns the team id and the invite token to share with members.',
    parameters: {
      name: { type: 'string', required: true, description: 'Team name' },
      goal_title: { type: 'string', description: 'Optional initial goal title' },
      goal_body: { type: 'string', description: 'Optional initial goal body' },
    },
    output: {
      schema: { type: 'string' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      },
    },
    async execute(a, exec) {
      const res = await runCli([
        'team', 'create',
        '--name', (a as any).name,
        '--session', getKey(exec),
        ...((a as any).goal_title ? ['--goal-title', (a as any).goal_title] : []),
        ...((a as any).goal_body ? ['--goal-body', (a as any).goal_body] : []),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_join',
    description:
      'Join a team via its invite_token. Creates a PENDING membership that the team owner must approve.',
    parameters: {
      token: { type: 'string', required: true, description: 'Team invite token' },
      name: { type: 'string', required: true, description: 'Display name chosen at join time' },
    },
    output: {
      schema: { type: 'string' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      },
    },
    async execute(a, exec) {
      const res = await runCli([
        'team', 'join',
        '--token', (a as any).token,
        '--name', (a as any).name,
        '--session', getKey(exec),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_leave',
    description: 'Leave the current team.',
    parameters: {
      team: { type: 'string', description: 'Team ID (optional if only one team)' },
    },
    output: {
      schema: { type: 'string' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      },
    },
    async execute(a, exec) {
      const res = await runCli([
        'team', 'leave',
        '--session', getKey(exec),
        ...((a as any).team ? ['--team', (a as any).team] : []),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_list_teams',
    description:
      'List the teams the current session belongs to, with goal and role overview.',
    parameters: {},
    output: {
      schema: { type: 'string' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      },
    },
    async execute(_a, exec) {
      const res = await runCli(['team', 'list', '--session', getKey(exec)])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_status',
    description:
      'Show full team status: team/goal state, members, roles, open questions, recent events.',
    parameters: {
      team: { type: 'string', description: 'Team ID (optional)' },
    },
    output: {
      schema: { type: 'string' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      },
    },
    async execute(a, exec) {
      const res = await runCli([
        'team', 'status',
        '--session', getKey(exec),
        ...((a as any).team ? ['--team', (a as any).team] : []),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_sync',
    description:
      'Pull the latest team state + NEW events since last sync. Call at the start of every turn before acting.',
    parameters: {},
    output: {
      schema: { type: 'string' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      },
    },
    async execute(_a, exec) {
      const res = await runCli(['team', 'sync', '--session', getKey(exec)])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_archive',
    description: 'Archive a completed team (owner only). Archived teams accept no new members.',
    parameters: {},
    output: {
      schema: { type: 'string' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      },
    },
    async execute(_a, exec) {
      const res = await runCli(['team', 'archive', '--session', getKey(exec)])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_team_destroy',
    description: 'Destroy the team permanently (owner only). This action cannot be undone.',
    parameters: {},
    output: {
      schema: { type: 'string' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      },
    },
    async execute(_a, exec) {
      const res = await runCli(['team', 'destroy', '--session', getKey(exec)])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_team_invite',
    description:
      'Generate an invite link for a new member with a specific role. Returns the invite token.',
    parameters: {
      role: { type: 'string', required: true, description: 'Role key (owner, supervisor, contributor, reviewer, subtask-implementer)' },
      name_hint: { type: 'string', description: 'Suggested display name for the invitee' },
    },
    output: {
      schema: { type: 'string' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      },
    },
    async execute(a, exec) {
      const res = await runCli([
        'team', 'invite',
        '--role', (a as any).role,
        '--session', getKey(exec),
        ...((a as any).name_hint ? ['--name-hint', (a as any).name_hint] : []),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_team_import',
    description: 'Import a team invite letter from a letter file or JSON string.',
    parameters: {
      path_or_json: { type: 'string', required: true, description: 'Path to letter file or JSON string' },
    },
    output: {
      schema: { type: 'string' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      },
    },
    async execute(a, exec) {
      const val = (a as any).path_or_json
      // Try as file path first, then as JSON
      let args: string[]
      try {
        JSON.parse(val)
        args = ['team', 'import', '--json', val, '--session', getKey(exec)]
      } catch {
        args = ['team', 'import', '--path', val, '--session', getKey(exec)]
      }
      const res = await runCli(args)
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_team_invite_list',
    description: 'List pending invites for the team.',
    parameters: {},
    output: {
      schema: { type: 'string' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      },
    },
    async execute(_a, exec) {
      const res = await runCli(['team', 'invite-list', '--session', getKey(exec)])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_team_invite_revoke',
    description: 'Revoke a pending invite by token.',
    parameters: {
      token: { type: 'string', required: true, description: 'Invite token to revoke' },
    },
    output: {
      schema: { type: 'string' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      },
    },
    async execute(a, exec) {
      const res = await runCli([
        'team', 'invite-revoke',
        '--token', (a as any).token,
        '--session', getKey(exec),
      ])
      return JSON.stringify(res)
    },
  }))
}

// ---------------------------------------------------------------------------
// Goal management
// ---------------------------------------------------------------------------

export function registerGoalTools(ctx: Context): void {
  ctx.tools.register(defineTool({
    name: 'teamx_set_goal',
    description: 'Set or update the team goal (owner only).',
    parameters: {
      title: { type: 'string', required: true, description: 'Goal title' },
      body: { type: 'string', description: 'Goal body / detailed description' },
    },
    output: {
      schema: { type: 'string' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      },
    },
    async execute(a, exec) {
      const res = await runCli([
        'goal', 'set',
        '--title', (a as any).title,
        '--session', getKey(exec),
        ...((a as any).body ? ['--body', (a as any).body] : []),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_share_goal',
    description:
      'Share the goal with team members and move the team into the active state (owner only).',
    parameters: {},
    output: {
      schema: { type: 'string' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      },
    },
    async execute(_a, exec) {
      const res = await runCli(['goal', 'share', '--session', getKey(exec)])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_close_goal',
    description:
      'Verify the achieved goal and close it; the team becomes completed (owner only). Only call this after a member reported the goal achieved.',
    parameters: {},
    output: {
      schema: { type: 'string' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      },
    },
    async execute(_a, exec) {
      const res = await runCli(['goal', 'close', '--session', getKey(exec)])
      return JSON.stringify(res)
    },
  }))
}

// ---------------------------------------------------------------------------
// Member/role management
// ---------------------------------------------------------------------------

export function registerMemberTools(ctx: Context): void {
  ctx.tools.register(defineTool({
    name: 'teamx_approve',
    description: 'Approve a pending membership request (owner only).',
    parameters: {
      member_id: { type: 'string', required: true, description: 'The pending member id' },
      team: { type: 'string', description: 'Team ID (optional)' },
    },
    output: {
      schema: { type: 'string' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      },
    },
    async execute(a, exec) {
      const res = await runCli([
        'team', 'approve',
        '--member', (a as any).member_id,
        '--session', getKey(exec),
        ...((a as any).team ? ['--team', (a as any).team] : []),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_deny',
    description: 'Deny a pending membership request (owner only).',
    parameters: {
      member_id: { type: 'string', required: true, description: 'The pending member id' },
      team: { type: 'string', description: 'Team ID (optional)' },
    },
    output: {
      schema: { type: 'string' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      },
    },
    async execute(a, exec) {
      const res = await runCli([
        'team', 'deny',
        '--member', (a as any).member_id,
        '--session', getKey(exec),
        ...((a as any).team ? ['--team', (a as any).team] : []),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_set_role',
    description:
      'Choose a role for the current session (member self-service) or assign one to another member (owner only).',
    parameters: {
      role: { type: 'string', required: true, description: 'Role key from the team catalog' },
      member: { type: 'string', description: 'Target member id when assigning on someone behalf (owner only)' },
    },
    output: {
      schema: { type: 'string' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      },
    },
    async execute(a, exec) {
      const res = await runCli([
        'role', 'set',
        '--role', (a as any).role,
        '--session', getKey(exec),
        ...((a as any).member ? ['--member', (a as any).member] : []),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_role_propose',
    description:
      'Propose a custom role (key + label + job description). The team owner must approve it before it can be used.',
    parameters: {
      role: { type: 'string', required: true, description: 'Unique role key, e.g. devops' },
      label: { type: 'string', required: true, description: 'Human-readable role label, e.g. DevOps 工程师' },
      description: { type: 'string', description: 'Job-role description / responsibilities' },
    },
    output: {
      schema: { type: 'string' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      },
    },
    async execute(a, exec) {
      const res = await runCli([
        'role', 'propose',
        '--role', (a as any).role,
        '--label', (a as any).label,
        '--session', getKey(exec),
        ...((a as any).description ? ['--description', (a as any).description] : []),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_role_approve',
    description: 'Approve a proposed custom role (owner only).',
    parameters: {
      role: { type: 'string', required: true, description: 'Role key to approve' },
    },
    output: {
      schema: { type: 'string' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      },
    },
    async execute(a, exec) {
      const res = await runCli([
        'role', 'approve',
        '--role', (a as any).role,
        '--session', getKey(exec),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_role_deny',
    description: 'Deny a proposed custom role and remove the proposal (owner only).',
    parameters: {
      role: { type: 'string', required: true, description: 'Role key to deny' },
    },
    output: {
      schema: { type: 'string' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      },
    },
    async execute(a, exec) {
      const res = await runCli([
        'role', 'deny',
        '--role', (a as any).role,
        '--session', getKey(exec),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_role_update',
    description: "Update a role's label and/or description (owner only).",
    parameters: {
      role: { type: 'string', required: true, description: 'Role key to update' },
      label: { type: 'string', description: 'New label (optional)' },
      description: { type: 'string', description: 'New description (optional)' },
    },
    output: {
      schema: { type: 'string' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      },
    },
    async execute(a, exec) {
      const res = await runCli([
        'role', 'update',
        '--role', (a as any).role,
        '--session', getKey(exec),
        ...((a as any).label ? ['--label', (a as any).label] : []),
        ...((a as any).description ? ['--description', (a as any).description] : []),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_set_state',
    description:
      'Set a member working state: idle (finished the current slice, no pending work) or active (resumed).',
    parameters: {
      state: { type: 'string', required: true, enum: ['idle', 'active'] as readonly string[], description: 'Target member state' },
      member: { type: 'string', description: 'Target member id (owner only)' },
    },
    output: {
      schema: { type: 'string' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      },
    },
    async execute(a, exec) {
      const res = await runCli([
        'member', 'set-state',
        '--state', (a as any).state,
        '--session', getKey(exec),
        ...((a as any).member ? ['--member', (a as any).member] : []),
      ])
      return JSON.stringify(res)
    },
  }))
}

// ---------------------------------------------------------------------------
// Interaction (ask/respond/publish)
// ---------------------------------------------------------------------------

export function registerInteractionTools(ctx: Context): void {
  ctx.tools.register(defineTool({
    name: 'teamx_ask',
    description:
      'Ask a team member a clarifying question; the member enters the waiting state until they respond.',
    parameters: {
      member_id: { type: 'string', required: true, description: 'Target member id' },
      question: { type: 'string', required: true, description: 'The clarifying question' },
    },
    output: {
      schema: { type: 'string' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      },
    },
    async execute(a, exec) {
      const res = await runCli([
        'ask',
        '--member', (a as any).member_id,
        '--question', (a as any).question,
        '--session', getKey(exec),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_respond',
    description: 'Answer an open question directed at the current session; returns the session to the active state.',
    parameters: {
      ask_id: { type: 'string', required: true, description: 'The question id' },
      answer: { type: 'string', required: true, description: 'Your answer' },
    },
    output: {
      schema: { type: 'string' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      },
    },
    async execute(a, exec) {
      const res = await runCli([
        'respond',
        '--ask-id', (a as any).ask_id,
        '--answer', (a as any).answer,
        '--session', getKey(exec),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_publish',
    description:
      'Publish an event to the team ledger. Types: start, progress, decision, update, blocked, resumed, achieved, refine.',
    parameters: {
      type: {
        type: 'string',
        required: true,
        enum: ['start', 'progress', 'decision', 'update', 'blocked', 'resumed', 'achieved', 'refine'] as readonly string[],
        description: 'Event type',
      },
      data: { type: 'string', description: 'JSON string payload, e.g. {"message":"..."}' },
      assignee: { type: 'string', description: 'Member id the task/event is directed to' },
    },
    output: {
      schema: { type: 'string' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      },
    },
    async execute(a, exec) {
      const res = await runCli([
        'publish',
        (a as any).type,
        '--session', getKey(exec),
        ...((a as any).data ? ['--data', (a as any).data] : []),
        ...((a as any).assignee ? ['--assignee', (a as any).assignee] : []),
      ])
      return JSON.stringify(res)
    },
  }))
}
