/**
 * teamx_* tools for dsh-plugin — batch 2 (tool alignment with opencode-plugin).
 *
 * Adds the 38 tools that opencode-plugin had and dsh-plugin lacked:
 * - Tasks (10):      taskx docs lifecycle (create/ack/claim/update/help/done/
 *                    verify/reject/list/log)
 * - Git (11):        ledger repo management (create/delete/list/clone/pull/
 *                    push/setup/commit/commit_push/grant/permissions)
 * - Serve (4):       embedded network-mode server lifecycle (+deprecated token)
 * - Console (3):     member user-console lifecycle
 * - Tunnels (6):     reverse tunnels (list/status/close/direct CLI; expose/
 *                    forward via the tunnel runtime)
 * - Misc (4):        inbox, loopx_report, team promote/demote lead
 *
 * CLI args mirror `crates/teamx/src/cli.rs` exactly. Session key is
 * `${instanceId}:${agent.session.id}` (same format as opencode-plugin).
 * @module @teamx-ai/dsh-plugin/tools-extra
 */

import type { Context } from '@deepseek-ai/cordis'
import { defineTool } from '@deepseek-ai/dsh-tools'
import type { ToolRunContext } from '@deepseek-ai/dsh-tools'
import { runCli, runCliLocal, sessionKey, instanceId, resolveServerUrl } from './client.js'
import { serveStart, serveStatus, serveStop, consoleStart, consoleStatus, consoleStop, openBrowser } from './serve-lifecycle.js'
import { exposeTunnel, forwardTunnel, tunnelServerUrl } from './tunnel.js'
import { saveTunnel, removeTunnel, saveForward, removeForward } from './tunnels-store.js'

/** Get the teamx session key for a dsh tool execution context. */
function getKey(exec: ToolRunContext): string {
  return sessionKey(instanceId(), exec.agent.session.id)
}

/** Append `--flag value` when value is present. */
function opt(name: string, value: string | undefined | null): string[] {
  return value ? [name, value] : []
}

function out(): { schema: { type: string }; render: (args: unknown, value: unknown) => { type: string; text: string }[] } {
  return {
    schema: { type: 'string' },
    render(_args, value) {
      return [{ type: 'text', text: String(value) }]
    },
  }
}

// ---------------------------------------------------------------------------
// Tasks (taskx docs lifecycle)
// ---------------------------------------------------------------------------

export function registerTaskTools(ctx: Context): void {
  ctx.tools.register(defineTool({
    name: 'teamx_task_create',
    description:
      'Create a team task (team lead). Tasks are teamx documents (taskx type): ' +
      'content in git, state machine in .meta.json. Assign to a specific member ' +
      '(--assignee) or a role (--role). executor=either (default) means a human or ' +
      'agent may do it (agent auto-executes, user can take over); executor=agent ' +
      'means an AI must do it; executor=human means a person must do it (AI is ' +
      'instructed not to take over). Optionally set --priority.',
    parameters: {
      title: { type: 'string', required: true, description: 'task title' },
      assignee: { type: 'string', description: 'assignee member id' },
      role: { type: 'string', description: 'assignee role key (e.g. reviewer)' },
      executor: { type: 'string', description: 'executor kind: either | agent | human (default either)' },
      priority: { type: 'string', description: 'priority: high | medium | low (default medium)' },
      id: { type: 'string', description: 'task id (default: slug of title)' },
      detail: { type: 'string', description: 'task detail / background' },
      no_push: { type: 'string', description: 'set to "1"/"true" to not auto git commit+push' },
    },
    output: out(),
    async execute(a, exec) {
      const args = a as Record<string, string>
      const res = await runCli([
        'task', 'create', args.title,
        '--session', getKey(exec),
        ...opt('--assignee', args.assignee),
        ...opt('--role', args.role),
        ...(args.executor ? ['--executor', args.executor] : []),
        ...(args.priority ? ['--priority', args.priority] : []),
        ...opt('--id', args.id),
        ...opt('--detail', args.detail),
        ...(args.no_push && ['1', 'true'].includes(args.no_push.toLowerCase()) ? ['--no-push'] : []),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_task_ack',
    description:
      'Acknowledge a task assigned to you (auto-issued by the plugin on assignee sessions; ' +
      'call manually only when needed). Records doc.acknowledged.',
    parameters: { id: { type: 'string', required: true, description: 'task id' } },
    output: out(),
    async execute(a, exec) {
      const res = await runCli(['task', 'ack', (a as any).id, '--session', getKey(exec)])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_task_claim',
    description: 'Claim a task (optional for small tasks). Records doc.claimed.',
    parameters: { id: { type: 'string', required: true, description: 'task id' } },
    output: out(),
    async execute(a, exec) {
      const res = await runCli(['task', 'claim', (a as any).id, '--session', getKey(exec)])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_task_update',
    description: 'Record progress on a task. Records doc.updated.',
    parameters: {
      id: { type: 'string', required: true, description: 'task id' },
      progress: { type: 'string', required: true, description: 'progress note' },
    },
    output: out(),
    async execute(a, exec) {
      const args = a as Record<string, string>
      const res = await runCli(['task', 'update', args.id, '--progress', args.progress, '--session', getKey(exec)])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_task_help',
    description: 'Request help from the team lead (notification event; task stays in_progress).',
    parameters: { id: { type: 'string', required: true, description: 'task id' } },
    output: out(),
    async execute(a, exec) {
      const res = await runCli(['task', 'help', (a as any).id, '--session', getKey(exec)])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_task_done',
    description: 'Mark a task done (completion candidate; the lead verifies). Records doc.done.',
    parameters: {
      id: { type: 'string', required: true, description: 'task id' },
      result: { type: 'string', description: 'result / deliverable summary' },
    },
    output: out(),
    async execute(a, exec) {
      const args = a as Record<string, string>
      const res = await runCli(['task', 'done', args.id, '--session', getKey(exec), ...opt('--result', args.result)])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_task_verify',
    description: 'Verify a completed task (team lead) — closes the loop. Records doc.verified.',
    parameters: { id: { type: 'string', required: true, description: 'task id' } },
    output: out(),
    async execute(a, exec) {
      const res = await runCli(['task', 'verify', (a as any).id, '--session', getKey(exec)])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_task_reject',
    description: 'Reject a task back to work (team lead), with a reason. Records doc.rejected.',
    parameters: {
      id: { type: 'string', required: true, description: 'task id' },
      reason: { type: 'string', required: true, description: 'rejection reason' },
    },
    output: out(),
    async execute(a, exec) {
      const args = a as Record<string, string>
      const res = await runCli(['task', 'reject', args.id, '--reason', args.reason, '--session', getKey(exec)])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_task_list',
    description:
      'List tasks (all, or filtered). --mine shows tasks assigned to the current member. ' +
      'Useful for the agent to see its open tasks / TODO.',
    parameters: {
      mine: { type: 'string', description: 'set to "1"/"true" to only show tasks assigned to the current member' },
      state: { type: 'string', description: 'filter by state (assigned/acked/claimed/in_progress/done/verified)' },
      executor: { type: 'string', description: 'filter by executor kind (either/agent/human)' },
    },
    output: out(),
    async execute(a, exec) {
      const args = a as Record<string, string>
      const res = await runCli([
        'task', 'list', '--session', getKey(exec),
        ...(args.mine && ['1', 'true'].includes(args.mine.toLowerCase()) ? ['--mine'] : []),
        ...opt('--state', args.state),
        ...opt('--executor', args.executor),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_task_log',
    description: "Show a task's full audit history (ledger events + meta transitions).",
    parameters: { id: { type: 'string', required: true, description: 'task id' } },
    output: out(),
    async execute(a, exec) {
      const res = await runCli(['task', 'log', (a as any).id, '--session', getKey(exec)])
      return JSON.stringify(res)
    },
  }))
}

// ---------------------------------------------------------------------------
// Git (ledger repo management)
// ---------------------------------------------------------------------------

export function registerGitTools(ctx: Context): void {
  ctx.tools.register(defineTool({
    name: 'teamx_git_create',
    description:
      'Create a new git repository on the teamx server (owner/admin only). ' +
      'The repository will be accessible to team members based on permissions.',
    parameters: {
      name: { type: 'string', required: true, description: 'repository name' },
      description: { type: 'string', description: 'repository description' },
      team: { type: 'string', description: 'team id (optional when the session has one team)' },
    },
    output: out(),
    async execute(a, exec) {
      const args = a as Record<string, string>
      const res = await runCli([
        'git', 'create', args.name, '--session', getKey(exec),
        ...opt('--description', args.description),
        ...opt('--team', args.team),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_git_delete',
    description:
      'Delete a git repository from the teamx server (owner/admin only). ' +
      'This will permanently remove the repository and all its data.',
    parameters: {
      name: { type: 'string', required: true, description: 'repository name' },
      team: { type: 'string', description: 'team id (optional when the session has one team)' },
    },
    output: out(),
    async execute(a, exec) {
      const args = a as Record<string, string>
      const res = await runCli(['git', 'delete', args.name, '--session', getKey(exec), ...opt('--team', args.team)])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_git_list',
    description:
      'List git repositories accessible to the current member. ' +
      'Returns repositories that the member has at least read access to.',
    parameters: { team: { type: 'string', description: 'team id (optional when the session has one team)' } },
    output: out(),
    async execute(a, exec) {
      const args = a as Record<string, string>
      const res = await runCli(['git', 'list', '--session', getKey(exec), ...opt('--team', args.team)])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_git_clone',
    description:
      'Clone a git repository from the teamx server to the local machine. ' +
      'The repository must be accessible to the current member (at least read permission). ' +
      'Local operation (mTLS from the invitation letter).',
    parameters: {
      repo: { type: 'string', required: true, description: 'repository name' },
      directory: { type: 'string', description: 'local directory to clone into (default: repo name)' },
      team: { type: 'string', description: 'team id (optional when the session has one team)' },
    },
    output: out(),
    async execute(a, exec) {
      const args = a as Record<string, string>
      const res = await runCliLocal([
        'git', 'clone', args.repo,
        '--session', getKey(exec),
        ...opt('--directory', args.directory),
        ...opt('--team', args.team),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_git_pull',
    description:
      'Pull (fetch + merge) changes from a git repository on the teamx server. ' +
      'Must be run from within a cloned repository. Local operation.',
    parameters: {
      repo: { type: 'string', required: true, description: 'repository name' },
      branch: { type: 'string', description: 'branch to pull (default: current branch)' },
      dir: { type: 'string', description: 'working directory (default: current dir)' },
      team: { type: 'string', description: 'team id (optional when the session has one team)' },
    },
    output: out(),
    async execute(a, exec) {
      const args = a as Record<string, string>
      const res = await runCliLocal([
        'git', 'pull', args.repo,
        '--session', getKey(exec),
        ...opt('--branch', args.branch),
        ...opt('--dir', args.dir),
        ...opt('--team', args.team),
      ], { cwd: args.dir })
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_git_push',
    description:
      'Push local changes to a git repository on the teamx server. ' +
      'Requires write permission on the repository. Local operation.',
    parameters: {
      repo: { type: 'string', required: true, description: 'repository name' },
      branch: { type: 'string', description: 'branch to push (default: current branch)' },
      dir: { type: 'string', description: 'working directory (default: current dir)' },
      team: { type: 'string', description: 'team id (optional when the session has one team)' },
    },
    output: out(),
    async execute(a, exec) {
      const args = a as Record<string, string>
      const res = await runCliLocal([
        'git', 'push', args.repo,
        '--session', getKey(exec),
        ...opt('--branch', args.branch),
        ...opt('--dir', args.dir),
        ...opt('--team', args.team),
      ], { cwd: args.dir })
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_git_setup',
    description:
      "Configure stock `git` to talk to the teamx server over mTLS using the invitation " +
      "letter's client certificate. Writes per-URL git config (http.<server>/sslCert/" +
      'sslKey/sslCAInfo), so after this the user can run plain `git clone/pull/push` against ' +
      'https://server/git/<team_id>/<repo> with no extra flags.',
    parameters: {
      server: { type: 'string', description: 'server URL (default: TEAMX_SERVER_URL or discovered letter)' },
      local: { type: 'string', description: 'set to "1"/"true" to write to the current repo config instead of ~/.gitconfig' },
    },
    output: out(),
    async execute(a, exec) {
      const args = a as Record<string, string>
      const res = await runCliLocal([
        'git', 'setup', '--session', getKey(exec),
        ...opt('--server', args.server),
        ...(args.local && ['1', 'true'].includes(args.local.toLowerCase()) ? ['--local'] : []),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_git_commit',
    description:
      'Commit local changes (git add -A + git commit) inside a teamx-cloned repository. ' +
      'This is a local operation (no network). Use with teamx_git_push to upload.',
    parameters: {
      message: { type: 'string', required: true, description: 'commit message' },
      dir: { type: 'string', description: 'working directory (default: current dir)' },
    },
    output: out(),
    async execute(a, exec) {
      const args = a as Record<string, string>
      const res = await runCliLocal(['git', 'commit', '--message', args.message, '--session', getKey(exec), ...opt('--dir', args.dir)], { cwd: args.dir })
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_git_commit_push',
    description:
      'Commit local changes and push them to a teamx git repository in one step. ' +
      'Runs `git add -A` + `git commit` locally, then uploads the bundle over mTLS. ' +
      'Requires write permission on the repository.',
    parameters: {
      message: { type: 'string', required: true, description: 'commit message' },
      repo: { type: 'string', description: 'repository name (default: the cloned repo)' },
      branch: { type: 'string', description: 'branch to push (default: current branch)' },
      dir: { type: 'string', description: 'working directory (default: current dir)' },
      team: { type: 'string', description: 'team id (optional when the session has one team)' },
    },
    output: out(),
    async execute(a, exec) {
      const args = a as Record<string, string>
      const res = await runCliLocal([
        'git', 'commit-push', '--message', args.message,
        '--session', getKey(exec),
        ...opt('--repo', args.repo),
        ...opt('--branch', args.branch),
        ...opt('--dir', args.dir),
        ...opt('--team', args.team),
      ], { cwd: args.dir })
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_git_grant',
    description:
      'Grant a team member access to a git repository (owner/admin only). ' +
      'Permission levels: read (clone/pull), write (+push), admin (+manage).',
    parameters: {
      name: { type: 'string', required: true, description: 'repository name' },
      member_id: { type: 'string', required: true, description: 'member id to grant access to' },
      permission: { type: 'string', description: 'permission level: read | write | admin (default read)' },
      team: { type: 'string', description: 'team id (optional when the session has one team)' },
    },
    output: out(),
    async execute(a, exec) {
      const args = a as Record<string, string>
      const res = await runCli([
        'git', 'grant', args.name, args.member_id, '--session', getKey(exec),
        ...opt('--permission', args.permission),
        ...opt('--team', args.team),
      ])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_git_permissions',
    description: 'Show access permissions of a git repository (owner/admin only).',
    parameters: {
      name: { type: 'string', required: true, description: 'repository name' },
      team: { type: 'string', description: 'team id (optional when the session has one team)' },
    },
    output: out(),
    async execute(a, exec) {
      const args = a as Record<string, string>
      const res = await runCli(['git', 'permissions', args.name, '--session', getKey(exec), ...opt('--team', args.team)])
      return JSON.stringify(res)
    },
  }))
}

// ---------------------------------------------------------------------------
// Serve / Console lifecycle
// ---------------------------------------------------------------------------

export function registerServeTools(ctx: Context): void {
  ctx.tools.register(defineTool({
    name: 'teamx_serve_start',
    description:
      'Start the embedded teamx network-mode server (spawns a local `teamx serve` subprocess). ' +
      'Idempotent: if already running, returns the current status. Returns the server URL to share with members.',
    parameters: {
      addr: { type: 'string', description: 'bind address (default 127.0.0.1)' },
      port: { type: 'string', description: 'bind port (default 5781)' },
      db: { type: 'string', description: 'database path (default TEAMX_DB or TEAMX_HOME/teamx.db)' },
    },
    output: out(),
    async execute(a) {
      const args = a as Record<string, string>
      const st = await serveStart({
        addr: args.addr,
        port: args.port ? Number(args.port) : undefined,
        db: args.db,
      })
      return JSON.stringify(st, null, 2)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_serve_status',
    description: 'Show whether the embedded teamx server is running, plus its URL and PID.',
    parameters: {},
    output: out(),
    async execute() {
      return JSON.stringify(serveStatus(), null, 2)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_serve_stop',
    description: 'Stop the embedded teamx server subprocess, if running.',
    parameters: {},
    output: out(),
    async execute() {
      return JSON.stringify(await serveStop(), null, 2)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_serve_token',
    description:
      'Generate or rotate a connection token for a member so they can connect to the network-mode server. ' +
      'Note: member identity now comes from mTLS client certificates issued via `teamx_team_invite`; ' +
      'token auth is superseded.',
    parameters: { member: { type: 'string', required: true, description: 'member id' } },
    output: out(),
    async execute() {
      // Superseded by mTLS invitation letters (I1) — kept for compatibility.
      return 'teamx: identity is now mTLS client certificates (see teamx_team_invite); token auth is deprecated.'
    },
  }))
}

export function registerConsoleTools(ctx: Context): void {
  ctx.tools.register(defineTool({
    name: 'teamx_console',
    description:
      'Start the member user-console (`teamx console`) for the current workspace and open it in the browser. ' +
      'Idempotent: if already running it just re-opens the existing URL. Returns the https://127.0.0.1:<port>/?token=… address.',
    parameters: {
      port: { type: 'string', description: 'bind port (default 9528)' },
      open: { type: 'string', description: 'set to "false" to not open the browser (default true)' },
    },
    output: out(),
    async execute(a) {
      const args = a as Record<string, string>
      const st = await consoleStart({ port: args.port ? Number(args.port) : undefined })
      if (st.running && st.url && args.open !== 'false') openBrowser(st.url)
      return JSON.stringify(st, null, 2)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_console_status',
    description: 'Show whether the workspace member user-console (`teamx console`) is running, plus its URL and PID.',
    parameters: {},
    output: out(),
    async execute() {
      return JSON.stringify(consoleStatus(), null, 2)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_console_stop',
    description: 'Stop the workspace member user-console (`teamx console`) subprocess, if running.',
    parameters: {},
    output: out(),
    async execute() {
      return JSON.stringify(await consoleStop(), null, 2)
    },
  }))
}

// ---------------------------------------------------------------------------
// Tunnels (reverse tunnels; expose/forward via the tunnel runtime)
// ---------------------------------------------------------------------------

export function registerTunnelTools(ctx: Context): void {
  ctx.tools.register(defineTool({
    name: 'teamx_tunnel_expose',
    description:
      'Expose a local service to teammates through the teamx server (reverse tunnel, provider side). ' +
      "The current machine's `port` becomes reachable by other team members. " +
      "Mode 'local' (default): the server binds no public port; teammates use teamx_tunnel_forward " +
      "to access it via a local port. Mode 'frp': the server binds a public port " +
      '(tcp://server:port) reachable by any TCP client. Requires network mode (TEAMX_SERVER_URL).',
    parameters: {
      name: { type: 'string', required: true, description: 'public tunnel name (unique per team), e.g. httpbin' },
      port: { type: 'string', required: true, description: 'local port to expose' },
      mode: { type: 'string', description: 'exposure mode: local | frp (default local)' },
      lan_ip: { type: 'string', description: 'provider LAN IP for direct-connect hints (auto-detected if absent)' },
    },
    output: out(),
    async execute(a) {
      const args = a as Record<string, string>
      const serverUrl = tunnelServerUrl() ?? resolveServerUrl()
      if (!serverUrl) {
        return 'teamx error: tunnel expose requires network mode; import an invitation letter (or set TEAMX_SERVER_URL)'
      }
      const { exposeTunnel } = await import('./tunnel.js')
      const handle = exposeTunnel({
        serverUrl,
        name: args.name,
        port: Number(args.port),
        mode: (args.mode as 'local' | 'frp') ?? 'local',
        lanIp: args.lan_ip,
      })
      const pubPort = await handle.ready()
      if (pubPort === null) {
        handle.close()
        return `teamx error: failed to register tunnel "${args.name}"`
      }
      // persist so the tunnel is re-opened after a restart
      saveTunnel(args.name, Number(args.port), args.mode ?? 'local', args.lan_ip, serverUrl)
      const mode = args.mode ?? 'local'
      const access =
        mode === 'frp'
          ? { public_port: pubPort, url: `tcp://<server>:${pubPort}` }
          : { note: 'local mode: teammates use teamx_tunnel_forward to reach this service' }
      return JSON.stringify({ ok: true, name: args.name, mode, ...access, direct: args.lan_ip ?? null }, null, 2)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_tunnel_list',
    description:
      'List reverse tunnels exposed by members of the current team (network mode). ' +
      'Each entry shows the public server port and the provider LAN IP for direct access.',
    parameters: { team: { type: 'string', description: 'team id (optional when the session has one team)' } },
    output: out(),
    async execute(a, exec) {
      const args = a as Record<string, string>
      const res = await runCli(['tunnel', 'list', '--session', getKey(exec), ...opt('--team', args.team)])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_tunnel_status',
    description:
      "Show one reverse tunnel's status (network mode): public server port, provider LAN IP, and whether " +
      'the current member is on the same subnet as the provider (direct access possible).',
    parameters: {
      name: { type: 'string', required: true, description: 'tunnel name' },
      team: { type: 'string', description: 'team id (optional when the session has one team)' },
    },
    output: out(),
    async execute(a, exec) {
      const args = a as Record<string, string>
      const res = await runCli(['tunnel', 'status', args.name, '--session', getKey(exec), ...opt('--team', args.team)])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_tunnel_close',
    description:
      'Close a reverse tunnel (network mode): frees its public server port. Any team member can close a tunnel.',
    parameters: {
      name: { type: 'string', required: true, description: 'tunnel name' },
      team: { type: 'string', description: 'team id (optional when the session has one team)' },
    },
    output: out(),
    async execute(a, exec) {
      const args = a as Record<string, string>
      const res = await runCli(['tunnel', 'close', args.name, '--session', getKey(exec), ...opt('--team', args.team)])
      // forget the persisted tunnel so it is not re-opened after a restart
      if ((res as { ok?: boolean })?.ok !== false) removeTunnel(args.name)
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_tunnel_direct',
    description:
      'Resolve the best access address for a reverse tunnel (network mode): if the current member is on the ' +
      'same subnet as the provider (tunnel.status same_subnet=true), returns the provider direct LAN address; ' +
      'otherwise returns the server relay address. Consumers can then reach the service directly or via the relay.',
    parameters: {
      name: { type: 'string', required: true, description: 'tunnel name' },
      team: { type: 'string', description: 'team id (optional when the session has one team)' },
    },
    output: out(),
    async execute(a, exec) {
      const args = a as Record<string, string>
      const res = await runCli(['tunnel', 'status', args.name, '--session', getKey(exec), ...opt('--team', args.team)])
      const data = ((res as { data?: Record<string, unknown> })?.data ?? {}) as Record<string, unknown>
      if (data.same_subnet === true && data.direct_addr) {
        return JSON.stringify(
          {
            ok: true,
            name: args.name,
            same_subnet: true,
            direct_addr: data.direct_addr,
            relay_addr: data.relay_addr ?? null,
            note: 'same subnet: use direct_addr for low-latency access',
          },
          null,
          2,
        )
      }
      return JSON.stringify(
        {
          ok: true,
          name: args.name,
          same_subnet: false,
          direct_addr: data.direct_addr ?? null,
          relay_addr: data.relay_addr ?? null,
          note: 'different subnet or unknown: use the server relay address',
        },
        null,
        2,
      )
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_tunnel_forward',
    description:
      'Forward a teammate exposed tunnel to a LOCAL port (consumer side, local-forward mode). ' +
      'The local port behaves like a local service: bytes are bridged over a mTLS WS to the provider ' +
      'tunnel through the server. Default local port = the provider target port; if that port is taken, ' +
      'a random candidate is returned for the user to confirm. Requires network mode (TEAMX_SERVER_URL).',
    parameters: {
      name: { type: 'string', required: true, description: 'tunnel name exposed by the provider' },
      local_port: { type: 'string', description: 'local port to listen on (default: provider target port)' },
      team: { type: 'string', description: 'team id (optional when the session has one team)' },
    },
    output: out(),
    async execute(a, exec) {
      const args = a as Record<string, string>
      const serverUrl = tunnelServerUrl() ?? resolveServerUrl()
      if (!serverUrl) {
        return 'teamx error: tunnel forward requires network mode; import an invitation letter (or set TEAMX_SERVER_URL)'
      }
      // Query the tunnel status first so we can default the local port to the
      // provider's target port (same behavior as opencode-plugin).
      let targetPort: number | undefined
      try {
        const st = await runCli(['tunnel', 'status', args.name, '--session', getKey(exec), ...opt('--team', args.team)])
        const data = ((st as { data?: Record<string, unknown> })?.data ?? {}) as Record<string, unknown>
        if (typeof data.port === 'number') targetPort = data.port
      } catch {
        // status lookup is best-effort
      }
      const { forwardTunnel } = await import('./tunnel.js')
      const handle = forwardTunnel({
        serverUrl,
        name: args.name,
        localPort: args.local_port ? Number(args.local_port) : undefined,
        targetPort,
      })
      const port = await handle.ready()
      if (port === null) {
        handle.close()
        return `teamx error: failed to bind a local port for tunnel "${args.name}"`
      }
      saveForward(args.name, port, serverUrl)
      return JSON.stringify({ ok: true, name: args.name, local_port: port, note: 'Ctrl+C / teamx restart closes it' }, null, 2)
    },
  }))
}

// ---------------------------------------------------------------------------
// Misc (inbox, loopx report, lead promotion)
// ---------------------------------------------------------------------------

export function registerMiscTools(ctx: Context): void {
  ctx.tools.register(defineTool({
    name: 'teamx_inbox',
    description:
      "Show the member's unified inbox for the current team: open tasks assigned to me, " +
      'open questions directed at me, and unread team nudges. Reading the inbox (ack, default) ' +
      'marks nudges as seen so they are not repeated on the next session.',
    parameters: {
      team: { type: 'string', description: 'team id (optional when the session has one team)' },
      ack: { type: 'string', description: 'set to "false" to not mark nudges as read (default true)' },
    },
    output: out(),
    async execute(a, exec) {
      const args = a as Record<string, string>
      const parts = ['team', 'inbox', '--session', getKey(exec)]
      if (args.team) parts.push('--team', args.team)
      if (args.ack !== 'false') parts.push('--ack')
      const res = await runCli(parts)
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_loopx_report',
    description:
      'Snapshot the loopx stage progress for a bound project and publish it to the team ledger as a ' +
      'loopx.progress event. If project is omitted, the session bound loopx_project (set at join) is used.',
    parameters: { project: { type: 'string', description: 'loopx project directory' } },
    output: out(),
    async execute(a, exec) {
      const args = a as Record<string, string>
      if (args.project) {
        const r = await runCli(['loopx', 'report', args.project, '--session', getKey(exec)])
        return JSON.stringify(r)
      }
      // fall back to the session's bound project: read from status
      const status = await runCli(['team', 'status', '--session', getKey(exec)])
      const data = (status as { data?: { teams?: { members?: { display_name?: string; loopx_project?: string }[] }[] } })?.data
      const project = data?.teams?.[0]?.members?.find((m) => m.display_name)?.loopx_project
      if (!project) {
        return 'teamx: no loopx project bound. Set loopx_project when joining (teamx_join) or pass project explicitly.'
      }
      const r = await runCli(['loopx', 'report', project, '--session', getKey(exec)])
      return JSON.stringify(r)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_team_promote_lead',
    description:
      'Promote a member to a backup team lead / co-lead (team lead only). A co-lead has full team-lead ' +
      'authorization (approve members, share/close goals, invite, etc.) so they can back up the TL.',
    parameters: {
      member_id: { type: 'string', required: true, description: 'member id to promote' },
      team: { type: 'string', description: 'team id (optional when the session has one team)' },
    },
    output: out(),
    async execute(a, exec) {
      const args = a as Record<string, string>
      const res = await runCli(['team', 'promote-lead', args.member_id, '--session', getKey(exec), ...opt('--team', args.team)])
      return JSON.stringify(res)
    },
  }))

  ctx.tools.register(defineTool({
    name: 'teamx_team_demote_lead',
    description: 'Remove a member backup team lead status (team lead only). The primary owner cannot be demoted.',
    parameters: {
      member_id: { type: 'string', required: true, description: 'member id to demote' },
      team: { type: 'string', description: 'team id (optional when the session has one team)' },
    },
    output: out(),
    async execute(a, exec) {
      const args = a as Record<string, string>
      const res = await runCli(['team', 'demote-lead', args.member_id, '--session', getKey(exec), ...opt('--team', args.team)])
      return JSON.stringify(res)
    },
  }))
}
