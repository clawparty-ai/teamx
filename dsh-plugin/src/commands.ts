/**
 * /team-* flat slash commands for dsh.
 * Commands: team-create, team-join, team-status, team-sync, team-goal-set/share/close,
 *   team-approve, team-deny, team-invite, team-import, team-publish,
 *   team-role-*, team-state-*, team-ask, team-respond, team-help
 * @module @teamx/dsh-plugin/commands
 */

import type { Context } from '@deepseek-ai/cordis'
import type { Agent } from '@deepseek-ai/dsh-agent'
import { runCli, sessionKey } from './client.js'

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
  return agent.session.id
}

function parseFlags(input: string): { positional: string[]; flags: Record<string, string> } {
  const parts = input.trim().split(/\s+/)
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
          '  /team-create <name> — Create a new team',
          '  /team-join <token> <name> — Join a team via invite token',
          '  /team-status — Show team status',
          '  /team-sync — Pull latest state',
          '  /team-goal-set <title> [body] — Set team goal',
          '  /team-goal-share — Share goal with members',
          '  /team-goal-close — Close achieved goal',
          '  /team-approve <member_id> — Approve pending member',
          '  /team-deny <member_id> — Deny pending member',
          '  /team-invite <role> [name_hint] — Generate invite',
          '  /team-import <path_or_json> — Import invite letter',
          '  /team-publish <type> [message] — Publish event',
          '  /team-role-set <role> [member] — Set/assign role',
          '  /team-state <idle|active> — Set working state',
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
        'team', 'create', '--name', name,
        '--session', getKey(invocation.agent),
        ...(flags['goal-title'] ? ['--goal-title', flags['goal-title']] : []),
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
        'team', 'join', '--token', token, '--name', name,
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
      return runCli(['team', 'status', '--session', getKey(invocation.agent)]).then(
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
      return runCli(['team', 'sync', '--session', getKey(invocation.agent)]).then(
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
        'goal', 'set', '--title', title,
        '--session', getKey(invocation.agent),
        ...(flags.body ? ['--body', flags.body] : []),
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
    input: { hint: '<member_id>' },
    handler(invocation): CommandResult | Promise<CommandResult> {
      const { positional } = parseFlags(invocation.rawInput)
      const memberId = positional[0]
      if (!memberId) return { kind: 'error', text: 'Usage: /team-approve <member_id>' }
      return runCli([
        'team', 'approve', '--member', memberId,
        '--session', getKey(invocation.agent),
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
    input: { hint: '<member_id>' },
    handler(invocation): CommandResult | Promise<CommandResult> {
      const { positional } = parseFlags(invocation.rawInput)
      const memberId = positional[0]
      if (!memberId) return { kind: 'error', text: 'Usage: /team-deny <member_id>' }
      return runCli([
        'team', 'deny', '--member', memberId,
        '--session', getKey(invocation.agent),
      ]).then(
        (res) => ({ kind: 'success' as const, text: JSON.stringify(res) }),
        (err) => ({ kind: 'error' as const, text: String(err) }),
      )
    },
  })

  // /team-invite
  ctx.commands.register({
    name: 'team-invite',
    description: 'Generate an invite link for a new member.',
    input: { hint: '<role> [--name-hint <name>]' },
    handler(invocation): CommandResult | Promise<CommandResult> {
      const { positional, flags } = parseFlags(invocation.rawInput)
      const role = positional[0]
      if (!role) return { kind: 'error', text: 'Usage: /team-invite <role> [--name-hint <name>]' }
      return runCli([
        'team', 'invite', '--role', role,
        '--session', getKey(invocation.agent),
        ...(flags['name-hint'] ? ['--name-hint', flags['name-hint']] : []),
      ]).then(
        (res) => ({ kind: 'success' as const, text: JSON.stringify(res) }),
        (err) => ({ kind: 'error' as const, text: String(err) }),
      )
    },
  })

  // /team-import
  ctx.commands.register({
    name: 'team-import',
    description: 'Import a team invite letter from a file or JSON string.',
    input: { hint: '<path_or_json>' },
    handler(invocation): CommandResult | Promise<CommandResult> {
      const { positional } = parseFlags(invocation.rawInput)
      const val = positional[0]
      if (!val) return { kind: 'error', text: 'Usage: /team-import <path_or_json>' }
      let cliArgs: string[]
      try {
        JSON.parse(val)
        cliArgs = ['team', 'import', '--json', val, '--session', getKey(invocation.agent)]
      } catch {
        cliArgs = ['team', 'import', '--path', val, '--session', getKey(invocation.agent)]
      }
      return runCli(cliArgs).then(
        (res) => ({ kind: 'success' as const, text: JSON.stringify(res) }),
        (err) => ({ kind: 'error' as const, text: String(err) }),
      )
    },
  })

  // /team-publish
  ctx.commands.register({
    name: 'team-publish',
    description: 'Publish an event to the team ledger.',
    input: { hint: '<type> [--message <msg>] [--assignee <member_id>]' },
    handler(invocation): CommandResult | Promise<CommandResult> {
      const { positional, flags } = parseFlags(invocation.rawInput)
      const type = positional[0]
      if (!type) return { kind: 'error', text: 'Usage: /team-publish <type> [--message <msg>] [--assignee <id>]' }
      const data = flags.message ? JSON.stringify({ message: flags.message }) : undefined
      return runCli([
        'publish', type,
        '--session', getKey(invocation.agent),
        ...(data ? ['--data', data] : []),
        ...(flags.assignee ? ['--assignee', flags.assignee] : []),
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
        'role', 'set', '--role', role,
        '--session', getKey(invocation.agent),
        ...(flags.member ? ['--member', flags.member] : []),
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
    input: { hint: '<idle|active>' },
    handler(invocation): CommandResult | Promise<CommandResult> {
      const { positional } = parseFlags(invocation.rawInput)
      const state = positional[0]
      if (state !== 'idle' && state !== 'active') {
        return { kind: 'error', text: 'Usage: /team-state <idle|active>' }
      }
      return runCli([
        'member', 'set-state', '--state', state,
        '--session', getKey(invocation.agent),
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
        'ask', '--member', memberId, '--question', question,
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
        'respond', '--ask-id', askId, '--answer', answer,
        '--session', getKey(invocation.agent),
      ]).then(
        (res) => ({ kind: 'success' as const, text: JSON.stringify(res) }),
        (err) => ({ kind: 'error' as const, text: String(err) }),
      )
    },
  })
}
