// teamx_* custom tools registered by the opencode plugin.
// Each tool maps 1:1 to a `teamx` CLI subcommand and returns the JSON result.

import { tool } from "@opencode-ai/plugin"
import { instanceId, markMember, runCli, renderResult, sessionKey, TEAMX_SERVER_URL } from "./client"
import { serveStart, serveStatus, serveStop } from "./serve"

type ToolCtx = { sessionID: string; directory: string }

/** Run a teamx CLI command as the current session and render its output. */
async function tx(sessionID: string | undefined, args: string[]): Promise<string> {
  const r = await txResult(sessionID, args)
  return renderResult(r)
}

/** Run a teamx CLI command and return the raw result (for membership marking). */
async function txResult(sessionID: string | undefined, args: string[]): Promise<import("./client").CliResult> {
  const key = sessionKey(instanceId(), sessionID)
  return runCli([...args, "--session", key])
}

function opt(name: string, value: string | undefined): string[] {
  return value ? [name, value] : []
}

/** Suggest a nearby free local port starting from `base` (heuristic). */
function nextFreePort(base: number): number {
  return base + Math.floor(Math.random() * 500) + 1
}

export const tools = {
  teamx_create_team: tool({
    description:
      "Create a new teamx team. The current opencode session becomes the team OWNER. " +
      "Returns the team id and the invite_token to share with members.",
    args: {
      name: tool.schema.string().describe("team name"),
      goal_title: tool.schema.string().optional().describe("optional initial goal title"),
      goal_body: tool.schema.string().optional().describe("optional initial goal body"),
    },
    async execute(args, context: ToolCtx) {
      const r = await txResult(context.sessionID, [
        "team",
        "create",
        args.name,
        ...opt("--goal-title", args.goal_title),
        ...opt("--goal-body", args.goal_body),
      ])
      if (r.ok) markMember(context.sessionID, true)
      return renderResult(r)
    },
  }),

  teamx_set_goal: tool({
    description: "Set or update the team goal (owner only).",
    args: {
      title: tool.schema.string().describe("goal title"),
      body: tool.schema.string().optional().describe("goal body / detailed description"),
    },
    async execute(args, context: ToolCtx) {
      return tx(context.sessionID, ["goal", "set", args.title, ...opt("--body", args.body)])
    },
  }),

  teamx_share_goal: tool({
    description: "Share the goal with team members and move the team into the active state (owner only).",
    args: {},
    async execute(_args, context: ToolCtx) {
      return tx(context.sessionID, ["goal", "share"])
    },
  }),

  teamx_close_goal: tool({
    description:
      "Verify the achieved goal and close it; the team becomes completed (owner only). " +
      "Only call this after a member reported the goal achieved.",
    args: {},
    async execute(_args, context: ToolCtx) {
      return tx(context.sessionID, ["goal", "close"])
    },
  }),

  teamx_archive: tool({
    description: "Archive a completed team (owner only). Archived teams accept no new members.",
    args: {},
    async execute(_args, context: ToolCtx) {
      return tx(context.sessionID, ["team", "archive"])
    },
  }),

  teamx_team_destroy: tool({
    description:
      "Soft-destroy a team (owner only): mark it destroyed, hide it from all member lists, revoke its " +
      "outstanding invitations, and keep its data for audit. Members keep their rows but the team is gone.",
    args: {
      team: tool.schema.string().optional().describe("team id (optional when the session has one team)"),
    },
    async execute(args, context: ToolCtx) {
      return tx(context.sessionID, ["team", "destroy", ...opt("--team", args.team)])
    },
  }),

  teamx_tunnel_expose: tool({
    description:
      "Expose a local service to teammates through the teamx server (reverse tunnel, provider side). " +
      "The current machine's `port` becomes reachable by other team members. " +
      "Mode 'local' (default): the server binds no public port; teammates use teamx_tunnel_forward " +
      "to access it via a local port. Mode 'frp': the server binds a public port " +
      "(tcp://server:port) reachable by any TCP client. Requires network mode (TEAMX_SERVER_URL).",
    args: {
      name: tool.schema.string().describe("public tunnel name (unique per team), e.g. httpbin"),
      port: tool.schema.number().describe("local port to expose"),
      mode: tool.schema.enum(["local", "frp"]).optional().describe("exposure mode (default local)"),
      lan_ip: tool.schema.string().optional().describe("provider LAN IP for direct-connect hints (auto-detected if absent)"),
    },
    async execute(args, _context: ToolCtx) {
      const serverUrl = TEAMX_SERVER_URL
      if (!serverUrl) {
        return "teamx error: tunnel expose requires network mode; set TEAMX_SERVER_URL (or import an invitation letter)"
      }
      const { exposeTunnel } = await import("./tunnel")
      const { saveTunnel } = await import("./tunnels-store")
      const handle = exposeTunnel({
        serverUrl,
        name: args.name,
        port: args.port,
        mode: args.mode ?? "local",
        lanIp: args.lan_ip,
      })
      const pubPort = await handle.ready()
      if (pubPort === null) {
        handle.close()
        return `teamx error: failed to register tunnel "${args.name}"`
      }
      // persist so the tunnel is re-opened after an opencode restart
      saveTunnel({
        name: args.name,
        port: args.port,
        mode: args.mode ?? "local",
        lan_ip: args.lan_ip,
        server_url: serverUrl,
        created_at: new Date().toISOString(),
      })
      const mode = args.mode ?? "local"
      const access =
        mode === "frp"
          ? { public_port: pubPort, url: `tcp://<server>:${pubPort}` }
          : { note: "local mode: teammates use teamx_tunnel_forward to reach this service" }
      return JSON.stringify({ ok: true, name: args.name, mode, ...access, direct: args.lan_ip ?? null }, null, 2)
    },
  }),

  teamx_tunnel_list: tool({
    description:
      "List reverse tunnels exposed by members of the current team (network mode). " +
      "Each entry shows the public server port and the provider's LAN IP for direct access.",
    args: {
      team: tool.schema.string().optional().describe("team id (optional when the session has one team)"),
    },
    async execute(args, context: ToolCtx) {
      return tx(context.sessionID, ["tunnel", "list", ...opt("--team", args.team)])
    },
  }),

  teamx_tunnel_status: tool({
    description:
      "Show one reverse tunnel's status (network mode): public server port, provider LAN IP, and whether " +
      "the current member is on the same subnet as the provider (direct access possible).",
    args: {
      name: tool.schema.string().describe("tunnel name"),
      team: tool.schema.string().optional().describe("team id (optional when the session has one team)"),
    },
    async execute(args, context: ToolCtx) {
      return tx(context.sessionID, ["tunnel", "status", args.name, ...opt("--team", args.team)])
    },
  }),

  teamx_tunnel_close: tool({
    description:
      "Close a reverse tunnel (network mode): frees its public server port. Any team member can close a tunnel.",
    args: {
      name: tool.schema.string().describe("tunnel name"),
      team: tool.schema.string().optional().describe("team id (optional when the session has one team)"),
    },
    async execute(args, context: ToolCtx) {
      const r = await txResult(context.sessionID, ["tunnel", "close", args.name, ...opt("--team", args.team)])
      // forget the persisted tunnel so it is not re-opened after a restart
      if (r.ok) {
        const { removeTunnel } = await import("./tunnels-store")
        removeTunnel(args.name)
      }
      return renderResult(r)
    },
  }),

  teamx_tunnel_direct: tool({
    description:
      "Resolve the best access address for a reverse tunnel (network mode): if the current member is on the " +
      "same subnet as the provider (tunnel.status same_subnet=true), returns the provider's direct LAN address; " +
      "otherwise returns the server relay address. Consumers can then reach the service directly or via the relay.",
    args: {
      name: tool.schema.string().describe("tunnel name"),
      team: tool.schema.string().optional().describe("team id (optional when the session has one team)"),
    },
    async execute(args, context: ToolCtx) {
      const r = await txResult(context.sessionID, ["tunnel", "status", args.name, ...opt("--team", args.team)])
      if (!r.ok) return renderResult(r)
      const data = (r.data ?? {}) as Record<string, unknown>
      if (data.same_subnet === true && data.direct_addr) {
        return JSON.stringify(
          {
            ok: true,
            name: args.name,
            same_subnet: true,
            direct_addr: data.direct_addr,
            relay_addr: data.relay_addr ?? null,
            note: "same subnet: use direct_addr for low-latency access",
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
          note: "different subnet or unknown: use the server relay address",
        },
        null,
        2,
      )
    },
  }),

  teamx_tunnel_forward: tool({
    description:
      "Forward a teammate's exposed tunnel to a LOCAL port (consumer side, local-forward mode). " +
      "The local port behaves like a local service: bytes are bridged over a mTLS WS to the provider's " +
      "tunnel through the server. Default local port = the provider's target port; if that port is taken, " +
      "a random candidate is returned for the user to confirm. Requires network mode (TEAMX_SERVER_URL).",
    args: {
      name: tool.schema.string().describe("tunnel name exposed by the provider"),
      local_port: tool.schema.number().optional().describe("local port to listen on (default: provider target port)"),
      team: tool.schema.string().optional().describe("team id (optional when the session has one team)"),
    },
    async execute(args, context: ToolCtx) {
      const serverUrl = TEAMX_SERVER_URL
      if (!serverUrl) {
        return "teamx error: tunnel forward requires network mode; set TEAMX_SERVER_URL (or import an invitation letter)"
      }
      // Resolve the provider's target port for a natural default local port.
      let targetPort: number | undefined
      if (!args.local_port) {
        const st = await txResult(context.sessionID, ["tunnel", "status", args.name, ...opt("--team", args.team)])
        if (st.ok) {
          const d = (st.data ?? {}) as Record<string, unknown>
          if (typeof d.target_port === "number") targetPort = d.target_port
        }
      }
      const { forwardTunnel } = await import("./tunnel")
      const { saveForward } = await import("./tunnels-store")
      const handle = forwardTunnel({
        serverUrl,
        name: args.name,
        localPort: args.local_port,
        targetPort,
      })
      const bound = await handle.ready()
      if (bound === null) {
        handle.close()
        const candidate = targetPort ? nextFreePort(targetPort) : 0
        return (
          `teamx error: local port ${args.local_port ?? targetPort ?? "?"} is already in use. ` +
          (candidate ? `Try --local-port ${candidate} (or confirm to bind it).` : "Pick a free port.")
        )
      }
      saveForward({ name: args.name, local_port: bound, server_url: serverUrl, created_at: new Date().toISOString() })
      return JSON.stringify(
        { ok: true, name: args.name, local_port: bound, note: "access like a local service, e.g. http://127.0.0.1:" + bound },
        null,
        2,
      )
    },
  }),

  teamx_leave: tool({
    description:
      "Leave a team. The current session stops being a member and its membership cache is invalidated.",
    args: {
      team: tool.schema.string().optional().describe("team id (optional when the session has one team)"),
    },
    async execute(args, context: ToolCtx) {
      const r = await txResult(context.sessionID, ["team", "leave", ...opt("--team", args.team)])
      if (r.ok) markMember(context.sessionID, false)
      return renderResult(r)
    },
  }),

  teamx_set_state: tool({
    description:
      "Set a member's working state: idle (finished the current slice, no pending work) or active (resumed). " +
      "Self-service by default; owner may set another member via the member arg.",
    args: {
      state: tool.schema.enum(["idle", "active"]).describe("target member state"),
      member: tool.schema.string().optional().describe("target member id (owner only)"),
    },
    async execute(args, context: ToolCtx) {
      return tx(context.sessionID, ["member", "set-state", args.state, ...opt("--member", args.member)])
    },
  }),

  teamx_join: tool({
    description:
      "Join a team via its invite_token. Creates a PENDING membership that the team lead must approve.",
    args: {
      token: tool.schema.string().describe("team invite token"),
      name: tool.schema.string().describe("display name chosen at join time"),
      loopx_project: tool.schema.string().optional().describe("loopx project directory for stage-progress reports"),
    },
    async execute(args, context: ToolCtx) {
      const r = await txResult(context.sessionID, [
        "team",
        "join",
        args.token,
        "--name",
        args.name,
        ...opt("--loopx-project", args.loopx_project),
      ])
      if (r.ok) markMember(context.sessionID, true)
      return renderResult(r)
    },
  }),

  teamx_approve: tool({
    description: "Approve a pending membership request (owner only). Pass team when the owner session belongs to several teams.",
    args: {
      member_id: tool.schema.string().describe("the pending member id"),
      team: tool.schema.string().optional().describe("team id (optional when the owner has one team)"),
    },
    async execute(args, context: ToolCtx) {
      return tx(context.sessionID, ["team", "approve", args.member_id, ...opt("--team", args.team)])
    },
  }),

  teamx_deny: tool({
    description: "Deny a pending membership request (owner only). Pass team when the owner session belongs to several teams.",
    args: {
      member_id: tool.schema.string().describe("the pending member id"),
      team: tool.schema.string().optional().describe("team id (optional when the owner has one team)"),
    },
    async execute(args, context: ToolCtx) {
      return tx(context.sessionID, ["team", "deny", args.member_id, ...opt("--team", args.team)])
    },
  }),

  teamx_team_invite: tool({
    description:
      "Invite a member with a job role (owner only). Issues a client certificate + a self-contained " +
      "invitation letter (base64 `teamx-inv:v1:...`) to share with the member. The member imports it and " +
      "connects over mTLS; you still approve them before they can work.",
    args: {
      role_desc: tool.schema.string().describe('job role + description, e.g. "测试工程师: 负责测试并汇报缺陷"'),
      name_hint: tool.schema.string().optional().describe("suggested display name (member may override at import)"),
      server_url: tool.schema.string().optional().describe("server URL to embed in the letter (default https://127.0.0.1:5781)"),
    },
    async execute(args, context: ToolCtx) {
      return tx(context.sessionID, [
        "team",
        "invite",
        args.role_desc,
        ...opt("--name-hint", args.name_hint),
        ...opt("--server-url", args.server_url),
      ])
    },
  }),

  teamx_team_invite_list: tool({
    description: "List issued invitation letters for the team (owner only), with their state (unused/used/revoked).",
    args: {},
    async execute(_args, context: ToolCtx) {
      return tx(context.sessionID, ["team", "invite-list"])
    },
  }),

  teamx_team_invite_revoke: tool({
    description: "Revoke an invitation letter (owner only); its certificate is rejected at connect.",
    args: {
      id: tool.schema.string().describe("invitation id to revoke"),
    },
    async execute(args, context: ToolCtx) {
      return tx(context.sessionID, ["team", "invite-revoke", args.id])
    },
  }),

  teamx_team_import: tool({
    description:
      "Import an invitation letter (single-line `teamx-inv:v1:<base64>` or a path to a .json letter). " +
      "Stores the mTLS material locally and, when the team DB is local, claims the pending member seat. " +
      "After import, stock `git` is configured for the teamx server (mTLS certs from the letter), " +
      "so once the owner approves you can `git clone https://<server>/git/<team_id>/<repo>`.",
    args: {
      letter: tool.schema.string().describe("the invitation letter (base64 or file path)"),
      name: tool.schema.string().optional().describe("display name (defaults to the letter's name_hint)"),
    },
    async execute(args, context: ToolCtx) {
      const r = await txResult(context.sessionID, ["team", "import", args.letter, ...opt("--name", args.name)])
      if (r.ok) markMember(context.sessionID, true)
      const rendered = renderResult(r)
      // After a successful import, configure stock git with the letter's mTLS
      // certs so the user can `git clone` directly (Smart HTTP endpoint).
      if (r.ok) {
        const data = (r.data ?? {}) as Record<string, unknown>
        const serverUrl = (data.server_url as string) || TEAMX_SERVER_URL
        if (serverUrl) {
          const setup = await txResult(context.sessionID, ["git", "setup", "--server", serverUrl])
          if (setup.ok) {
            return rendered + "\n\n[teamx] stock git configured for mTLS: `git clone https://" +
              serverUrl.replace(/^https?:\/\//, "") + "/git/<team_id>/<repo>`" +
              (Array.isArray(data.git_repos) && (data.git_repos as string[]).length > 0
                ? ` (repos: ${(data.git_repos as string[]).join(", ")})`
                : "")
          }
        }
      }
      return rendered
    },
  }),

  teamx_set_role: tool({
    description:
      "Choose a role for the current session (member self-service) or assign one to another member (owner only, via member). " +
      "Only approved roles are usable: built-in roles (owner, observer, supervisor, contributor, subtask-implementer, reviewer) plus any custom roles the owner approved.",
    args: {
      role: tool.schema.string().describe("role key from the team catalog"),
      member: tool.schema.string().optional().describe("target member id when assigning on someone's behalf (owner only)"),
    },
    async execute(args, context: ToolCtx) {
      return tx(context.sessionID, ["role", "set", args.role, ...opt("--member", args.member)])
    },
  }),

  teamx_role_propose: tool({
    description:
      "Propose a custom role (key + label + job description). The team lead must approve it before it can be used. " +
      "Any team member (including the owner) may propose. Role key must not conflict with a built-in role.",
    args: {
      role: tool.schema.string().describe("unique role key, e.g. devops"),
      label: tool.schema.string().describe("human-readable role label, e.g. DevOps 工程师"),
      description: tool.schema.string().optional().describe("job-role description / responsibilities"),
    },
    async execute(args, context: ToolCtx) {
      return tx(context.sessionID, ["role", "propose", args.role, args.label, ...(args.description ? [args.description] : [])])
    },
  }),

  teamx_role_approve: tool({
    description:
      "Approve a proposed custom role (owner only). The role becomes usable and is automatically granted to the member who proposed it.",
    args: {
      role: tool.schema.string().describe("role key to approve"),
    },
    async execute(args, context: ToolCtx) {
      return tx(context.sessionID, ["role", "approve", args.role])
    },
  }),

  teamx_role_deny: tool({
    description:
      "Deny a proposed custom role and remove the proposal (owner only). The proposer does not get the role.",
    args: {
      role: tool.schema.string().describe("role key to deny"),
    },
    async execute(args, context: ToolCtx) {
      return tx(context.sessionID, ["role", "deny", args.role])
    },
  }),

  teamx_role_update: tool({
    description:
      "Update a role's label and/or description (owner only). Pass only the fields to change; the other is preserved.",
    args: {
      role: tool.schema.string().describe("role key to update"),
      label: tool.schema.string().optional().describe("new label (optional)"),
      description: tool.schema.string().optional().describe("new description (optional)"),
    },
    async execute(args, context: ToolCtx) {
      const parts = ["role", "update", args.role]
      if (args.label) parts.push("--label", args.label)
      if (args.description) parts.push("--description", args.description)
      return tx(context.sessionID, parts)
    },
  }),

  teamx_list_teams: tool({
    description: "List the teams the current session belongs to, with goal and role overview.",
    args: {},
    async execute(_args, context: ToolCtx) {
      return tx(context.sessionID, ["team", "list"])
    },
  }),

  teamx_status: tool({
    description:
      "Show full team status: team/goal state, members, roles, open questions, recent events. " +
      "Pass team if the session belongs to several teams.",
    args: {
      team: tool.schema.string().optional().describe("team id (optional when the session has one team)"),
    },
    async execute(args, context: ToolCtx) {
      const key = sessionKey(instanceId(), context.sessionID)
      const r = await runCli(["team", "status", ...opt("--team", args.team), "--session", key])
      return renderResult(r)
    },
  }),

  teamx_sync: tool({
    description:
      "Pull the latest team state + NEW events since the last sync for every team the current session belongs to, " +
      "then advance the sync cursor. Call this at the start of every turn before acting.",
    args: {},
    async execute(_args, context: ToolCtx) {
      return tx(context.sessionID, ["sync"])
    },
  }),

  teamx_publish: tool({
    description:
      "Publish an event to the team ledger. Types:\n" +
      "- start: owner starts execution (goal -> in_progress)\n" +
      "- progress: member reports progress (goal -> in_progress)\n" +
      "- decision: owner broadcasts a decision to the team\n" +
      "- update: owner broadcasts a status update\n" +
      "- blocked: work is blocked (team/goal -> blocked)\n" +
      "- resumed: unblocked (-> in_progress/active)\n" +
      "- achieved: member reports the goal as achieved (candidate)\n" +
      "- refine: owner asks members to refine scope/roles (goal -> refining)\n" +
      "Pass data as a JSON string with extra fields (e.g. {\"message\": \"...\"}).\n" +
      "When assignee is set, the event becomes a DIRECTED task for that member " +
      "(auto-execute fires on that member only); others receive it as a broadcast.",
    args: {
      type: tool.schema
        .enum(["start", "progress", "decision", "update", "blocked", "resumed", "achieved", "refine"])
        .describe("event type"),
      data: tool.schema.string().optional().describe('JSON string payload, e.g. {"message":"..."}'),
      assignee: tool.schema.string().optional().describe("member id the task/event is directed to (auto-execute target)"),
    },
    async execute(args, context: ToolCtx) {
      return tx(context.sessionID, ["publish", args.type, ...opt("--data", args.data), ...opt("--assignee", args.assignee)])
    },
  }),

  teamx_ask: tool({
    description: "Ask a team member a clarifying question; the member enters the waiting state until they respond.",
    args: {
      member_id: tool.schema.string().describe("target member id"),
      question: tool.schema.string().describe("the clarifying question"),
    },
    async execute(args, context: ToolCtx) {
      return tx(context.sessionID, ["ask", args.member_id, "--question", args.question])
    },
  }),

  teamx_respond: tool({
    description: "Answer an open question directed at the current session; returns the session to the active state.",
    args: {
      ask_id: tool.schema.string().describe("the question id"),
      answer: tool.schema.string().describe("your answer"),
    },
    async execute(args, context: ToolCtx) {
      return tx(context.sessionID, ["respond", args.ask_id, "--answer", args.answer])
    },
  }),

  teamx_loopx_report: tool({
    description:
      "Snapshot the loopx stage progress for a bound project and publish it to the team ledger as a loopx.progress event. " +
      "If project is omitted, the session's bound loopx_project (set at join) is used.",
    args: {
      project: tool.schema.string().optional().describe("loopx project directory"),
    },
    async execute(args, context: ToolCtx) {
      const key = sessionKey(instanceId(), context.sessionID)
      if (args.project) {
        const r = await runCli(["loopx", "report", args.project, "--session", key])
        return renderResult(r)
      }
      // fall back to the session's bound project: read from status
      const status = await runCli(["team", "status", "--session", key])
      const data = status.data as { teams?: { members?: { display_name?: string; loopx_project?: string }[] }[] } | null
      const project = data?.teams?.[0]?.members?.find((m) => m.display_name)?.loopx_project
      if (!project) {
        return "teamx: no loopx project bound. Set loopx_project when joining (teamx_join) or pass project explicitly."
      }
      const r = await runCli(["loopx", "report", project, "--session", key])
      return renderResult(r)
    },
  }),

  teamx_serve_start: tool({
    description:
      "Start the embedded teamx network-mode server (spawns a local `teamx serve` subprocess). " +
      "Idempotent: if already running, returns the current status. Returns the server URL to share with members.",
    args: {
      addr: tool.schema.string().optional().describe("bind address (default 0.0.0.0)"),
      port: tool.schema.number().optional().describe("bind port (default 5781)"),
      db: tool.schema.string().optional().describe("database path (default TEAMX_DB or ~/.teamx/teamx.db)"),
    },
    async execute(args, context: ToolCtx) {
      const st = await serveStart({ addr: args.addr, port: args.port, db: args.db })
      return JSON.stringify(st, null, 2)
    },
  }),

  teamx_serve_status: tool({
    description: "Show whether the embedded teamx server is running, plus its URL and PID.",
    args: {},
    async execute() {
      return JSON.stringify(serveStatus(), null, 2)
    },
  }),

  teamx_serve_stop: tool({
    description: "Stop the embedded teamx server subprocess, if running.",
    args: {},
    async execute() {
      return JSON.stringify(await serveStop(), null, 2)
    },
  }),

  teamx_serve_token: tool({
    description:
      "Generate or rotate a connection token for a member so they can connect to the network-mode server. " +
      "Note: with I1, member identity comes from mTLS client certificates issued via `teamx_team_invite`; " +
      "token auth is superseded.",
    args: {
      member: tool.schema.string().describe("member id"),
    },
    async execute(_args, context: ToolCtx) {
      // Superseded by mTLS invitation letters (I1) — kept for compatibility.
      return "teamx: identity is now mTLS client certificates (see teamx_team_invite); token auth is deprecated."
    },
  }),

  // Git repository management tools
  teamx_git_create: tool({
    description:
      "Create a new git repository on the teamx server (owner/admin only). " +
      "The repository will be accessible to team members based on permissions.",
    args: {
      name: tool.schema.string().describe("repository name"),
      description: tool.schema.string().optional().describe("repository description"),
      team: tool.schema.string().optional().describe("team id (optional when the session has one team)"),
    },
    async execute(args, context: ToolCtx) {
      return tx(context.sessionID, ["git", "create", args.name, ...opt("--description", args.description), ...opt("--team", args.team)])
    },
  }),

  teamx_git_delete: tool({
    description:
      "Delete a git repository from the teamx server (owner/admin only). " +
      "This will permanently remove the repository and all its data.",
    args: {
      name: tool.schema.string().describe("repository name"),
      team: tool.schema.string().optional().describe("team id (optional when the session has one team)"),
    },
    async execute(args, context: ToolCtx) {
      return tx(context.sessionID, ["git", "delete", args.name, ...opt("--team", args.team)])
    },
  }),

  teamx_git_list: tool({
    description:
      "List git repositories accessible to the current member. " +
      "Returns repositories that the member has at least read access to.",
    args: {
      team: tool.schema.string().optional().describe("team id (optional when the session has one team)"),
    },
    async execute(args, context: ToolCtx) {
      return tx(context.sessionID, ["git", "list", ...opt("--team", args.team)])
    },
  }),

  teamx_git_clone: tool({
    description:
      "Clone a git repository from the teamx server to the local machine. " +
      "The repository must be accessible to the current member (at least read permission).",
    args: {
      repo: tool.schema.string().describe("repository name"),
      directory: tool.schema.string().optional().describe("local directory to clone into (default: repo name)"),
      team: tool.schema.string().optional().describe("team id (optional when the session has one team)"),
    },
    async execute(args, context: ToolCtx) {
      return tx(context.sessionID, ["git", "clone", args.repo, ...opt("--directory", args.directory), ...opt("--team", args.team)])
    },
  }),

  teamx_git_pull: tool({
    description:
      "Pull (fetch + merge) changes from a git repository on the teamx server. " +
      "Must be run from within a cloned repository.",
    args: {
      repo: tool.schema.string().describe("repository name"),
      branch: tool.schema.string().optional().describe("branch to pull (default: current branch)"),
      dir: tool.schema.string().optional().describe("working directory (default: current dir)"),
      team: tool.schema.string().optional().describe("team id (optional when the session has one team)"),
    },
    async execute(args, context: ToolCtx) {
      return tx(context.sessionID, ["git", "pull", args.repo, ...opt("--branch", args.branch), ...opt("--dir", args.dir), ...opt("--team", args.team)])
    },
  }),

  teamx_git_push: tool({
    description:
      "Push local changes to a git repository on the teamx server. " +
      "Requires write permission on the repository.",
    args: {
      repo: tool.schema.string().describe("repository name"),
      branch: tool.schema.string().optional().describe("branch to push (default: current branch)"),
      dir: tool.schema.string().optional().describe("working directory (default: current dir)"),
      team: tool.schema.string().optional().describe("team id (optional when the session has one team)"),
    },
    async execute(args, context: ToolCtx) {
      return tx(context.sessionID, ["git", "push", args.repo, ...opt("--branch", args.branch), ...opt("--dir", args.dir), ...opt("--team", args.team)])
    },
  }),

  teamx_git_setup: tool({
    description:
      "Configure stock `git` to talk to the teamx server over mTLS using the invitation " +
      "letter's client certificate (stored in the member's private directory). " +
      "Writes per-URL git config (http.<server>/sslCert/sslKey/sslCAInfo) into ~/.gitconfig, " +
      "so after this the user can run plain `git clone/pull/push` against " +
      "https://server/git/<team_id>/<repo> with no extra flags.",
    args: {
      server: tool.schema.string().optional().describe("server URL (default: TEAMX_SERVER_URL or discovered letter)"),
      local: tool.schema.boolean().optional().describe("write to the current repo's config instead of ~/.gitconfig"),
    },
    async execute(args, context: ToolCtx) {
      const parts = ["git", "setup"]
      if (args.server) parts.push("--server", args.server)
      if (args.local) parts.push("--local")
      return tx(context.sessionID, parts)
    },
  }),

  teamx_git_commit: tool({
    description:
      "Commit local changes (git add -A + git commit) inside a teamx-cloned repository. " +
      "This is a local operation (no network). Use with teamx_git_push to upload.",
    args: {
      message: tool.schema.string().describe("commit message"),
      dir: tool.schema.string().optional().describe("working directory (default: current dir)"),
    },
    async execute(args, context: ToolCtx) {
      return tx(context.sessionID, ["git", "commit", "--message", args.message, ...opt("--dir", args.dir)])
    },
  }),

  teamx_git_commit_push: tool({
    description:
      "Commit local changes and push them to a teamx git repository in one step. " +
      "Runs `git add -A` + `git commit` locally, then uploads the bundle over mTLS. " +
      "Requires write permission on the repository.",
    args: {
      message: tool.schema.string().describe("commit message"),
      repo: tool.schema.string().optional().describe("repository name (default: the cloned repo)"),
      branch: tool.schema.string().optional().describe("branch to push (default: current branch)"),
      dir: tool.schema.string().optional().describe("working directory (default: current dir)"),
      team: tool.schema.string().optional().describe("team id (optional when the session has one team)"),
    },
    async execute(args, context: ToolCtx) {
      return tx(context.sessionID, ["git", "commit-push", "--message", args.message, ...opt("--repo", args.repo), ...opt("--branch", args.branch), ...opt("--dir", args.dir), ...opt("--team", args.team)])
    },
  }),

  teamx_git_grant: tool({
    description:
      "Grant a team member access to a git repository (owner/admin only). " +
      "Permission levels: read (clone/pull), write (+push), admin (+manage).",
    args: {
      name: tool.schema.string().describe("repository name"),
      member_id: tool.schema.string().describe("member id to grant access to"),
      permission: tool.schema.enum(["read", "write", "admin"]).optional().describe("permission level (default read)"),
      team: tool.schema.string().optional().describe("team id (optional when the session has one team)"),
    },
    async execute(args, context: ToolCtx) {
      return tx(context.sessionID, ["git", "grant", args.name, args.member_id, ...opt("--permission", args.permission), ...opt("--team", args.team)])
    },
  }),

  teamx_git_permissions: tool({
    description:
      "Show access permissions of a git repository (owner/admin only).",
    args: {
      name: tool.schema.string().describe("repository name"),
      team: tool.schema.string().optional().describe("team id (optional when the session has one team)"),
    },
    async execute(args, context: ToolCtx) {
      return tx(context.sessionID, ["git", "permissions", args.name, ...opt("--team", args.team)])
    },
  }),
}
