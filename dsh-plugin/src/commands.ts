/**
 * /team-* flat slash commands for dsh.
 * Commands: team-create, team-join, team-status, team-sync, team-goal-set/share/close,
 *   team-approve, team-deny, team-invite, team-import, team-publish,
 *   team-role-*, team-state-*, team-ask, team-respond, team-help
 * CLI args mirror `crates/teamx/src/cli.rs` exactly (positional args stay positional).
 * @module @teamx/dsh-plugin/commands
 */

import type { Context } from '@deepseek-ai/cordis'
import type { Agent } from '@deepseek-ai/dsh-agent'
// Type-only: pulls in the dsh-commands Context augmentation (ctx.commands).
import type {} from '@deepseek-ai/dsh-commands'
import { runCli, sessionKey, instanceId } from './client.js'

interface CommandInvocation {
  readonly agent: Agent
  readonly rawInput: string
  readonly signal: AbortSignal
}

type CommandResult =
  | { kind: 'success'; text?: string }
  | { kind: 'error'; text: string }

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function getKey(agent: Agent): string {
  return sessionKey(instanceId(), agent.session.id)
}

/**
 * Split a command line into tokens, honoring double quotes so flag values like
 * `--message "hello world"` stay one token.
 */
function tokenize(input: string): string[] {
  const out: string[] = []
  let cur = ''
  let inQuote = false
  for (const ch of input) {
    if (ch === '"') {
      inQuote = !inQuote
    } else if (ch === ' ' && !inQuote) {
      if (cur) {
        out.push(cur)
        cur = ''
      }
    } else {
      cur += ch
    }
  }
  if (cur) out.push(cur)
  return out
}

function parseFlags(input: string): { positional: string[]; flags: Record<string, string> } {
  const parts = tokenize(input)
  const positional: string[] = []
  const flags: Record<string, string> = {}
  for (let i = 0; i < parts.length; i++) {
    if (parts[i].startsWith('--') && i + 1 < parts.length) {
      flags[parts[i].slice(2)] = parts[i + 1]
      i++
    } else {
      positional.push(parts[i])
    }
  }
  return { positional, flags }
}

function opt(name: string, value: string | undefined | null): string[] {
  return value ? [name, value] : []
}

// ---------------------------------------------------------------------------
// Register all /team-* commands
// ---------------------------------------------------------------------------

export function registerCommands(ctx: Context): void {
  // /team-help
  ctx.commands.register({
    name: 'team-help',
    description: 'Show available teamx commands',
    handler(_invocation): CommandResult {
      return {
        kind: 'success',
        text: [
          'Available teamx commands:',
          '  /team-create <name> [--goal-title <title>] — Create a new team',
          '  /team-join <token> <name> — Join a team via invite token',
          '  /team-status [--team <id>] — Show team status',
          '  /team-sync — Pull latest state',
          '  /team-goal-set <title> [--body <description>] — Set team goal',
          '  /team-goal-share — Share goal with members',
          '  /team-goal-close — Close achieved goal',
          '  /team-approve <member_id> [--team <id>] — Approve pending member',
          '  /team-deny <member_id> [--team <id>] — Deny pending member',
          '  /team-invite "<role: description>" [--name-hint <name>] — Generate invite',
          '  /team-import <letter_or_path> [--name <name>] — Import invite letter',
          '  /team-publish <type> [--data <json>] [--assignee <member_id>] — Publish event',
          '  /team-role-set <role> [--member <id>] — Set/assign role',
          '  /team-role-propose <key> <label> [description] — Propose custom role',
          '  /team-role-approve <key> — Approve custom role',
          '  /team-role-deny <key> — Deny custom role',
          '  /team-role-update <key> [--label <l>] [--description <d>] — Update role',
          '  /team-state <idle|active> [--member <id>] — Set working state',
          '  /team-ask <member_id> <question> — Ask a question',
          '  /team-respond <ask_id> <answer> — Answer a question',
        ].join('\n'),
      }
    },
  })

  // /team-create
  ctx.commands.register({
    name: 'team-create',
    description: 'Create a new team. The current session becomes the OWNER.',
    input: { hint: '<team_name> [--goal-title <title>]' },
    handler(invocation): CommandResult | Promise<CommandResult> {
      const { positional, flags } = parseFlags(invocation.rawInput)
      const name = positional[0]
      if (!name) return { kind: 'error', text: 'Usage: /team-create <name> [--goal-title <title>]' }
      return runCli([
        'team', 'create', name,
        '--session', getKey(invocation.agent),
        ...opt('--goal-title', flags['goal-title']),
      ]).then(
        (res) => ({ kind: 'success' as const, text: JSON.stringify(res) }),
        (err) => ({ kind: 'error' as const, text: String(err) }),
      )
    },
  })

  // /team-join
  ctx.commands.register({
    name: 'team-join',
    description: 'Join a team via invite token.',
    input: { hint: '<invite_token> <display_name>' },
    handler(invocation): CommandResult | Promise<CommandResult> {
      const { positional } = parseFlags(invocation.rawInput)
      const token = positional[0]
      const name = positional[1]
      if (!token || !name) return { kind: 'error', text: 'Usage: /team-join <token> <name>' }
      return runCli([
        'team', 'join', token,
        '--name', name,
        '--session', getKey(invocation.agent),
      ]).then(
        (res) => ({ kind: 'success' as const, text: JSON.stringify(res) }),
        (err) => ({ kind: 'error' as const, text: String(err) }),
      )
    },
  })

  // /team-status
  ctx.commands.register({
    name: 'team-status',
    description: 'Show full team status.',
    handler(invocation): CommandResult | Promise<CommandResult> {
      const { flags } = parseFlags(invocation.rawInput)
      return runCli([
        'team', 'status',
        '--session', getKey(invocation.agent),
        ...opt('--team', flags.team),
      ]).then(
        (res) => ({ kind: 'success' as const, text: JSON.stringify(res) }),
        (err) => ({ kind: 'error' as const, text: String(err) }),
      )
    },
  })

  // /team-sync
  ctx.commands.register({
    name: 'team-sync',
    description: 'Pull the latest team state + new events.',
    handler(invocation): CommandResult | Promise<CommandResult> {
      return runCli(['sync', '--session', getKey(invocation.agent)]).then(
        (res) => ({ kind: 'success' as const, text: JSON.stringify(res) }),
        (err) => ({ kind: 'error' as const, text: String(err) }),
      )
    },
  })

  // /team-goal-set
  ctx.commands.register({
    name: 'team-goal-set',
    description: 'Set or update the team goal (owner only).',
    input: { hint: '<title> [--body <description>]' },
    handler(invocation): CommandResult | Promise<CommandResult> {
      const { positional, flags } = parseFlags(invocation.rawInput)
      const title = positional[0]
      if (!title) return { kind: 'error', text: 'Usage: /team-goal-set <title> [--body <description>]' }
      return runCli([
        'goal', 'set', title,
        '--session', getKey(invocation.agent),
        ...opt('--body', flags.body),
      ]).then(
        (res) => ({ kind: 'success' as const, text: JSON.stringify(res) }),
        (err) => ({ kind: 'error' as const, text: String(err) }),
      )
    },
  })

  // /team-goal-share
  ctx.commands.register({
    name: 'team-goal-share',
    description: 'Share the goal with team members (owner only).',
    handler(invocation): CommandResult | Promise<CommandResult> {
      return runCli(['goal', 'share', '--session', getKey(invocation.agent)]).then(
        (res) => ({ kind: 'success' as const, text: JSON.stringify(res) }),
        (err) => ({ kind: 'error' as const, text: String(err) }),
      )
    },
  })

  // /team-goal-close
  ctx.commands.register({
    name: 'team-goal-close',
    description: 'Close the achieved goal (owner only).',
    handler(invocation): CommandResult | Promise<CommandResult> {
      return runCli(['goal', 'close', '--session', getKey(invocation.agent)]).then(
        (res) => ({ kind: 'success' as const, text: JSON.stringify(res) }),
        (err) => ({ kind: 'error' as const, text: String(err) }),
      )
    },
  })

  // /team-approve
  ctx.commands.register({
    name: 'team-approve',
    description: 'Approve a pending membership request (owner only).',
    input: { hint: '<member_id> [--team <id>]' },
    handler(invocation): CommandResult | Promise<CommandResult> {
      const { positional, flags } = parseFlags(invocation.rawInput)
      const memberId = positional[0]
      if (!memberId) return { kind: 'error', text: 'Usage: /team-approve <member_id>' }
      return runCli([
        'team', 'approve', memberId,
        '--session', getKey(invocation.agent),
        ...opt('--team', flags.team),
      ]).then(
        (res) => ({ kind: 'success' as const, text: JSON.stringify(res) }),
        (err) => ({ kind: 'error' as const, text: String(err) }),
      )
    },
  })

  // /team-deny
  ctx.commands.register({
    name: 'team-deny',
    description: 'Deny a pending membership request (owner only).',
    input: { hint: '<member_id> [--team <id>]' },
    handler(invocation): CommandResult | Promise<CommandResult> {
      const { positional, flags } = parseFlags(invocation.rawInput)
      const memberId = positional[0]
      if (!memberId) return { kind: 'error', text: 'Usage: /team-deny <member_id>' }
      return runCli([
        'team', 'deny', memberId,
        '--session', getKey(invocation.agent),
        ...opt('--team', flags.team),
      ]).then(
        (res) => ({ kind: 'success' as const, text: JSON.stringify(res) }),
        (err) => ({ kind: 'error' as const, text: String(err) }),
      )
    },
  })

  // /team-invite
  ctx.commands.register({
    name: 'team-invite',
    description: 'Invite a member with a job role (owner only).',
    input: { hint: '"<role: description>" [--name-hint <name>]' },
    handler(invocation): CommandResult | Promise<CommandResult> {
      const { positional, flags } = parseFlags(invocation.rawInput)
      const roleDesc = positional[0]
      if (!roleDesc) return { kind: 'error', text: 'Usage: /team-invite "<role: description>" [--name-hint <name>]' }
      return runCli([
        'team', 'invite', roleDesc,
        '--session', getKey(invocation.agent),
        ...opt('--name-hint', flags['name-hint']),
      ]).then(
        (res) => ({ kind: 'success' as const, text: JSON.stringify(res) }),
        (err) => ({ kind: 'error' as const, text: String(err) }),
      )
    },
  })

  // /team-import
  ctx.commands.register({
    name: 'team-import',
    description: 'Import a team invite letter.',
    input: { hint: '<letter_or_path> [--name <name>]' },
    handler(invocation): CommandResult | Promise<CommandResult> {
      const { positional, flags } = parseFlags(invocation.rawInput)
      const letter = positional[0]
      if (!letter) return { kind: 'error', text: 'Usage: /team-import <letter_or_path>' }
      return runCli([
        'team', 'import', letter,
        '--session', getKey(invocation.agent),
        ...opt('--name', flags.name),
      ]).then(
        (res) => ({ kind: 'success' as const, text: JSON.stringify(res) }),
        (err) => ({ kind: 'error' as const, text: String(err) }),
      )
    },
  })

  // /team-publish
  ctx.commands.register({
    name: 'team-publish',
    description: 'Publish an event to the team ledger.',
    input: { hint: '<type> [--data <json>] [--assignee <member_id>]' },
    handler(invocation): CommandResult | Promise<CommandResult> {
      const { positional, flags } = parseFlags(invocation.rawInput)
      const type = positional[0]
      if (!type) return { kind: 'error', text: 'Usage: /team-publish <type> [--data <json>] [--assignee <id>]' }
      return runCli([
        'publish', type,
        '--session', getKey(invocation.agent),
        ...opt('--data', flags.data),
        ...opt('--assignee', flags.assignee),
      ]).then(
        (res) => ({ kind: 'success' as const, text: JSON.stringify(res) }),
        (err) => ({ kind: 'error' as const, text: String(err) }),
      )
    },
  })

  // /team-role-set
  ctx.commands.register({
    name: 'team-role-set',
    description: 'Choose or assign a role.',
    input: { hint: '<role> [--member <member_id>]' },
    handler(invocation): CommandResult | Promise<CommandResult> {
      const { positional, flags } = parseFlags(invocation.rawInput)
      const role = positional[0]
      if (!role) return { kind: 'error', text: 'Usage: /team-role-set <role> [--member <member_id>]' }
      return runCli([
        'role', 'set', role,
        '--session', getKey(invocation.agent),
        ...opt('--member', flags.member),
      ]).then(
        (res) => ({ kind: 'success' as const, text: JSON.stringify(res) }),
        (err) => ({ kind: 'error' as const, text: String(err) }),
      )
    },
  })

  // /team-role-propose
  ctx.commands.register({
    name: 'team-role-propose',
    description: 'Propose a custom role.',
    input: { hint: '<key> <label> [description]' },
    handler(invocation): CommandResult | Promise<CommandResult> {
      const { positional } = parseFlags(invocation.rawInput)
      const role = positional[0]
      const label = positional[1]
      const description = positional[2]
      if (!role || !label) return { kind: 'error', text: 'Usage: /team-role-propose <key> <label> [description]' }
      return runCli([
        'role', 'propose', role, label,
        '--session', getKey(invocation.agent),
        ...(description ? [description] : []),
      ]).then(
        (res) => ({ kind: 'success' as const, text: JSON.stringify(res) }),
        (err) => ({ kind: 'error' as const, text: String(err) }),
      )
    },
  })

  // /team-role-approve
  ctx.commands.register({
    name: 'team-role-approve',
    description: 'Approve a proposed custom role (owner only).',
    input: { hint: '<key>' },
    handler(invocation): CommandResult | Promise<CommandResult> {
      const { positional } = parseFlags(invocation.rawInput)
      const role = positional[0]
      if (!role) return { kind: 'error', text: 'Usage: /team-role-approve <key>' }
      return runCli(['role', 'approve', role, '--session', getKey(invocation.agent)]).then(
        (res) => ({ kind: 'success' as const, text: JSON.stringify(res) }),
        (err) => ({ kind: 'error' as const, text: String(err) }),
      )
    },
  })

  // /team-role-deny
  ctx.commands.register({
    name: 'team-role-deny',
    description: 'Deny a proposed custom role (owner only).',
    input: { hint: '<key>' },
    handler(invocation): CommandResult | Promise<CommandResult> {
      const { positional } = parseFlags(invocation.rawInput)
      const role = positional[0]
      if (!role) return { kind: 'error', text: 'Usage: /team-role-deny <key>' }
      return runCli(['role', 'deny', role, '--session', getKey(invocation.agent)]).then(
        (res) => ({ kind: 'success' as const, text: JSON.stringify(res) }),
        (err) => ({ kind: 'error' as const, text: String(err) }),
      )
    },
  })

  // /team-role-update
  ctx.commands.register({
    name: 'team-role-update',
    description: "Update a role's label/description (owner only).",
    input: { hint: '<key> [--label <l>] [--description <d>]' },
    handler(invocation): CommandResult | Promise<CommandResult> {
      const { positional, flags } = parseFlags(invocation.rawInput)
      const role = positional[0]
      if (!role) return { kind: 'error', text: 'Usage: /team-role-update <key> [--label <l>] [--description <d>]' }
      return runCli([
        'role', 'update', role,
        '--session', getKey(invocation.agent),
        ...opt('--label', flags.label),
        ...opt('--description', flags.description),
      ]).then(
        (res) => ({ kind: 'success' as const, text: JSON.stringify(res) }),
        (err) => ({ kind: 'error' as const, text: String(err) }),
      )
    },
  })

  // /team-state
  ctx.commands.register({
    name: 'team-state',
    description: 'Set working state: idle or active.',
    input: { hint: '<idle|active> [--member <id>]' },
    handler(invocation): CommandResult | Promise<CommandResult> {
      const { positional, flags } = parseFlags(invocation.rawInput)
      const state = positional[0]
      if (state !== 'idle' && state !== 'active') {
        return { kind: 'error', text: 'Usage: /team-state <idle|active> [--member <id>]' }
      }
      return runCli([
        'member', 'set-state', state,
        '--session', getKey(invocation.agent),
        ...opt('--member', flags.member),
      ]).then(
        (res) => ({ kind: 'success' as const, text: JSON.stringify(res) }),
        (err) => ({ kind: 'error' as const, text: String(err) }),
      )
    },
  })

  // /team-ask
  ctx.commands.register({
    name: 'team-ask',
    description: 'Ask a team member a clarifying question.',
    input: { hint: '<member_id> <question>' },
    handler(invocation): CommandResult | Promise<CommandResult> {
      const { positional } = parseFlags(invocation.rawInput)
      const memberId = positional[0]
      const question = positional.slice(1).join(' ')
      if (!memberId || !question) return { kind: 'error', text: 'Usage: /team-ask <member_id> <question>' }
      return runCli([
        'ask', memberId,
        '--question', question,
        '--session', getKey(invocation.agent),
      ]).then(
        (res) => ({ kind: 'success' as const, text: JSON.stringify(res) }),
        (err) => ({ kind: 'error' as const, text: String(err) }),
      )
    },
  })

  // /team-respond
  ctx.commands.register({
    name: 'team-respond',
    description: 'Answer an open question.',
    input: { hint: '<ask_id> <answer>' },
    handler(invocation): CommandResult | Promise<CommandResult> {
      const { positional } = parseFlags(invocation.rawInput)
      const askId = positional[0]
      const answer = positional.slice(1).join(' ')
      if (!askId || !answer) return { kind: 'error', text: 'Usage: /team-respond <ask_id> <answer>' }
      return runCli([
        'respond', askId,
        '--answer', answer,
        '--session', getKey(invocation.agent),
      ]).then(
        (res) => ({ kind: 'success' as const, text: JSON.stringify(res) }),
        (err) => ({ kind: 'error' as const, text: String(err) }),
      )
    },
  })
}
