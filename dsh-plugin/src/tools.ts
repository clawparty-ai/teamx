/**
 * teamx_* tools for dsh-plugin.
 * Each tool calls `runCli` to spawn the teamx binary (or HTTP RPC in network mode).
 * Session key is `${instanceId}:${agent.session.id}` (same format as opencode-plugin).
 * CLI args mirror `crates/teamx/src/cli.rs` exactly — positional args stay
 * positional, only real flags use `--flag value`.
 * @module @teamx/dsh-plugin/tools
 */

import type { Context } from '@deepseek-ai/cordis'
import { defineTool } from '@deepseek-ai/dsh-tools'
import type { ToolRunContext } from '@deepseek-ai/dsh-tools'
import { runCli, sessionKey, instanceId } from './client.js'
/** Get the teamx session key for a dsh tool execution context. */
function getKey(exec: ToolRunContext): string {
  return sessionKey(instanceId(), exec.agent.session.id)
}

/** Append `--flag value` when value is present. */
function opt(name: string, value: string | undefined | null): string[] {
  return value ? [name, value] : []
}

/** Append a positional value when present. */
function pos(value: string | undefined | null): string[] {
  return value ? [value] : []
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
        'team', 'create', (a as any).name,
        '--session', getKey(exec),
        ...opt('--goal-title', (a as any).goal_title),
        ...opt('--goal-body', (a as any).goal_body),
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
        'team', 'join', (a as any).token,
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
        ...opt('--team', (a as any).team),
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
        ...opt('--team', (a as any).team),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_sync',
    description:
      'Pull the latest team state + NEW events since the last sync for every team the current session belongs to, then advance the sync cursor. Call this at the start of every turn before acting.',
    parameters: {},
    output: {
      schema: { type: 'string' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      },
    },
    async execute(_a, exec) {
      const res = await runCli(['sync', '--session', getKey(exec)])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_archive',
    description: 'Archive a completed team (owner only). Archived teams accept no new members.',
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
        'team', 'archive',
        '--session', getKey(exec),
        ...opt('--team', (a as any).team),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_team_destroy',
    description: 'Destroy the team permanently (owner only). This action cannot be undone.',
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
        'team', 'destroy',
        '--session', getKey(exec),
        ...opt('--team', (a as any).team),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_team_invite',
    description:
      'Invite a member with a job role: issue a client cert + invitation letter (owner only). Pass role + description, e.g. "测试工程师: 负责测试并汇报缺陷".',
    parameters: {
      role_desc: { type: 'string', required: true, description: 'Job role + description, e.g. "测试工程师: 负责测试并汇报缺陷"' },
      name_hint: { type: 'string', description: 'Suggested display name (member may override at import)' },
    },
    output: {
      schema: { type: 'string' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      },
    },
    async execute(a, exec) {
      const res = await runCli([
        'team', 'invite', (a as any).role_desc,
        '--session', getKey(exec),
        ...opt('--name-hint', (a as any).name_hint),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_team_import',
    description:
      'Import an invitation letter: store the client cert/key and claim the pending seat. Pass the letter (single-line `teamx-inv:v1:<base64>` or a path to a .json letter).',
    parameters: {
      letter: { type: 'string', required: true, description: 'Invitation letter (teamx-inv:v1:<base64> or path to .json letter)' },
      name: { type: 'string', description: 'Display name (defaults to the letter name_hint)' },
    },
    output: {
      schema: { type: 'string' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      },
    },
    async execute(a, exec) {
      const res = await runCli([
        'team', 'import', (a as any).letter,
        '--session', getKey(exec),
        ...opt('--name', (a as any).name),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_team_invite_list',
    description: 'List issued invitation letters for the team (owner only), with their state (unused/used/revoked).',
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
        'team', 'invite-list',
        '--session', getKey(exec),
        ...opt('--team', (a as any).team),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_team_invite_revoke',
    description: 'Revoke an invitation letter (its cert is rejected at connect) (owner only).',
    parameters: {
      id: { type: 'string', required: true, description: 'Invitation id to revoke' },
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
        'team', 'invite-revoke', (a as any).id,
        '--session', getKey(exec),
        ...opt('--team', (a as any).team),
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
    description: 'Set (or update) the team goal (owner only).',
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
        'goal', 'set', (a as any).title,
        '--session', getKey(exec),
        ...opt('--body', (a as any).body),
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
    description: 'Approve a pending membership request (owner only). Pass team when the owner session belongs to several teams.',
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
        'team', 'approve', (a as any).member_id,
        '--session', getKey(exec),
        ...opt('--team', (a as any).team),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_deny',
    description: 'Deny a pending membership request (owner only). Pass team when the owner session belongs to several teams.',
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
        'team', 'deny', (a as any).member_id,
        '--session', getKey(exec),
        ...opt('--team', (a as any).team),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_set_role',
    description:
      'Set the current session role (member self-service); owner may specify on their behalf.',
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
        'role', 'set', (a as any).role,
        '--session', getKey(exec),
        ...opt('--member', (a as any).member),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_role_propose',
    description:
      'Propose a custom role (member self-service); the owner must approve it before it can be used.',
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
        'role', 'propose', (a as any).role, (a as any).label,
        '--session', getKey(exec),
        ...pos((a as any).description),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_role_approve',
    description: 'Approve a proposed custom role and grant it to the proposer (owner only).',
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
        'role', 'approve', (a as any).role,
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
        'role', 'deny', (a as any).role,
        '--session', getKey(exec),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_role_update',
    description: "Update a role's label/description (owner only).",
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
        'role', 'update', (a as any).role,
        '--session', getKey(exec),
        ...opt('--label', (a as any).label),
        ...opt('--description', (a as any).description),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_set_state',
    description:
      "Set a member's working state: idle (finished the current slice, no pending work) or active (resumed).",
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
        'member', 'set-state', (a as any).state,
        '--session', getKey(exec),
        ...opt('--member', (a as any).member),
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
        'ask', (a as any).member_id,
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
        'respond', (a as any).ask_id,
        '--answer', (a as any).answer,
        '--session', getKey(exec),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_publish',
    description:
      'Publish an event to the team ledger. Types: start, progress, decision, update, blocked, resumed, achieved, refine, activity.',
    parameters: {
      type: {
        type: 'string',
        required: true,
        enum: ['start', 'progress', 'decision', 'update', 'blocked', 'resumed', 'achieved', 'refine', 'activity'] as readonly string[],
        description: 'Event type',
      },
      data: { type: 'string', description: 'JSON payload for the event' },
      assignee: { type: 'string', description: 'Assign the task/event to a specific member (auto-execute on that member only)' },
    },
    output: {
      schema: { type: 'string' },
      render(_args, value) {
        return [{ type: 'text', text: String(value) }]
      },
    },
    async execute(a, exec) {
      const res = await runCli([
        'publish', (a as any).type,
        '--session', getKey(exec),
        ...opt('--data', (a as any).data),
        ...opt('--assignee', (a as any).assignee),
      ])
      return JSON.stringify(res)
    },
  }))
}
