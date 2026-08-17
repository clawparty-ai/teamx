// teamx_* custom tools registered by the opencode plugin.
// Each tool maps 1:1 to a `teamx` CLI subcommand and returns the JSON result.

import { tool } from "@opencode-ai/plugin"
import { instanceId, markMember, runCli, renderResult, sessionKey } from "./client"
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
      "Join a team via its invite_token. Creates a PENDING membership that the team owner must approve.",
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
      "Stores the mTLS material locally and, when the team DB is local, claims the pending member seat.",
    args: {
      letter: tool.schema.string().describe("the invitation letter (base64 or file path)"),
      name: tool.schema.string().optional().describe("display name (defaults to the letter's name_hint)"),
    },
    async execute(args, context: ToolCtx) {
      const r = await txResult(context.sessionID, ["team", "import", args.letter, ...opt("--name", args.name)])
      if (r.ok) markMember(context.sessionID, true)
      return renderResult(r)
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
      "Propose a custom role (key + label + job description). The team owner must approve it before it can be used. " +
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
}
