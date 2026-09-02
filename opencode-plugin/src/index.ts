// teamx opencode plugin (V1 + M2).
//
// - Registers the teamx_* tool family.
// - `event` hook mirrors session activity (session.idle) into the ledger.
// - M2 (poll-based, no server): a poller refreshes a per-session team digest,
//   `experimental.chat.system.transform` injects it into the next request, and
//   `client.tui` toasts/append-prompt notify on new events. Gate with
//   TEAMX_POLL_INTERVAL (ms, default 15000; 0 disables polling).

import type { Plugin } from "@opencode-ai/plugin"
import {
  getDigest,
  instanceId,
  knownMemberSessions,
  markMember,
  memberStatus,
  runCli,
  sessionKey,
  setDigest,
  TEAMX_SERVER_URL,
} from "./client"
import { tools } from "./tools"
import { connectWs } from "./ws"
import { t } from "./i18n"

const LOG_SERVICE = "teamx"
const POLL_INTERVAL = Number(process.env.TEAMX_POLL_INTERVAL ?? 15000)
// loopx-style auto-execute: when a task broadcast arrives, wake the member
// session and start working until the goal is done. Enabled by default; set
// TEAMX_AUTO_EXECUTE=0 to disable.
const AUTO_EXECUTE = process.env.TEAMX_AUTO_EXECUTE !== "0"

type MemberRow = { display_name?: string; role?: string | null; state?: string }
type QuestionRow = { state?: string; question?: string }
type TeamBlock = {
  team?: {
    id?: string
    name?: string
    state?: string
    my_role?: string
    my_member_id?: string
    my_is_lead?: boolean
  }
  goal?: { title?: string; state?: string } | null
  members?: MemberRow[]
  questions?: QuestionRow[]
}
type SyncEvent = {
  seq?: number
  type?: string
  member_id?: string
  team_id?: string
  payload?: Record<string, unknown>
}
type SyncData = { teams?: TeamBlock[]; new_events?: SyncEvent[] }

/** A session's per-team context (a session may belong to several teams). */
type TeamCtx = {
  id?: string
  memberId?: string
  role?: string
  isLead?: boolean
}

/** Resolve the session's context in EVERY team it belongs to. */
function myTeamCtxs(data: SyncData): TeamCtx[] {
  return (data?.teams ?? []).map((t) => ({
    id: t.team?.id,
    memberId: t.team?.my_member_id,
    role: t.team?.my_role,
    isLead: t.team?.my_is_lead === true || t.team?.my_role === "owner",
  }))
}

/**
 * The session's context in the team that emitted `e` (matched by `team_id`).
 * Falls back to the first team when the event carries no `team_id` (older sync
 * payloads / unit tests) so directed-task matching never misses events from a
 * non-first team membership.
 */
function myTeamForEvent(data: SyncData, e: SyncEvent): TeamCtx | undefined {
  const ctxs = myTeamCtxs(data)
  if (ctxs.length === 0) return undefined
  const tid = e.team_id
  if (tid) {
    const hit = ctxs.find((c) => c.id === tid)
    if (hit) return hit
  }
  return ctxs[0]
}

/** Truncate a string to a short single-line snippet. */
function shorten(s: string, max = 40): string {
  const flat = (s ?? "").replace(/\s+/g, " ").trim()
  return flat.length > max ? flat.slice(0, max - 1) + "…" : flat
}

/** Session idle mirrors are pure noise; filter them out of digests/toasts. */
function isHeartbeat(e: SyncEvent): boolean {
  return e.type === "progress.published" && (e.payload ?? {})["kind"] === "session.idle"
}

/** Return only events worth surfacing to the user (no heartbeats). */
function notableEvents(events: SyncEvent[]): SyncEvent[] {
  return events.filter((e) => !isHeartbeat(e))
}

/** Build a short human-readable summary line for a single ledger event. */
function summarizeEvent(e: SyncEvent): string {
  const seq = e.seq ?? "?"
  const t_ = e.type ?? "?"
  const p = e.payload ?? {}
  const s = (k: string) => (p[k] == null ? "" : String(p[k]))
  const msg = s("message")
  switch (t_) {
    case "team.created":
      return t("toast.team_created", { seq: String(seq), name: s("name") })
    case "goal.set":
      return t("toast.goal_set", { seq: String(seq), title: s("title") })
    case "goal.updated":
      return t("toast.goal_updated", { seq: String(seq), title: s("title") })
    case "goal.shared":
      return t("toast.goal_shared", { seq: String(seq), title: s("title") })
    case "goal.achieved":
      return t("toast.goal_achieved", { seq: String(seq) }) + (msg ? `: ${shorten(msg)}` : "")
    case "goal.state_changed":
      return s("kind")
        ? t("toast.goal_state_changed_kind", { seq: String(seq), from: s("from"), to: s("to"), kind: s("kind") })
        : t("toast.goal_state_changed", { seq: String(seq), from: s("from"), to: s("to") })
    case "team.state_changed":
      return t("toast.team_state_changed", { seq: String(seq), from: s("from"), to: s("to") })
    case "team.completed":
      return t("toast.team_completed", { seq: String(seq) })
    case "membership.pending":
      return p["rejoined"]
        ? t("toast.membership_rejoined", { seq: String(seq), name: s("display_name") })
        : t("toast.membership_pending", { seq: String(seq), name: s("display_name") })
    case "membership.approved":
      return t("toast.membership_approved", { seq: String(seq), name: s("display_name") })
    case "membership.denied":
      return t("toast.membership_denied", { seq: String(seq), name: s("display_name") })
    case "member.promoted_lead":
      return t("toast.promoted_lead", { seq: String(seq), name: s("display_name") })
    case "member.demoted_lead":
      return t("toast.demoted_lead", { seq: String(seq), name: s("display_name") })
    case "clarification.asked":
      return t("toast.clarification_asked", { seq: String(seq), target: s("target"), question: shorten(s("question")) })
    case "clarification.responded":
      return t("toast.clarification_responded", { seq: String(seq), answer: shorten(s("answer")) })
    case "progress.published":
      if (p["kind"] === "session.idle") return t("toast.heartbeat", { seq: String(seq) })
      return msg
        ? t("toast.progress_with_msg", { seq: String(seq), message: shorten(msg) })
        : t("toast.progress_no_msg", { seq: String(seq) })
    case "decision.broadcast": {
      const assignee = s("assignee_name")
      const body = msg ? shorten(msg) : ""
      return assignee
        ? t("toast.broadcast_assigned", { seq: String(seq), assignee, body })
        : t("toast.broadcast_unassigned", { seq: String(seq), body })
    }
    case "role.proposed":
      return t("toast.role_proposed", { seq: String(seq), proposer: s("proposer"), label: s("label"), key: s("key") })
    case "role.approved":
      return t("toast.role_approved", { seq: String(seq), label: s("label"), approver: s("approver") })
    case "role.denied":
      return t("toast.role_denied", { seq: String(seq), label: s("label"), denier: s("denier") })
    case "role.updated":
      return t("toast.role_updated", { seq: String(seq), label: s("label"), updated_by: s("updated_by") })
    case "loopx.progress":
      return t("toast.loopx_progress", { seq: String(seq) })
    // taskx (built-in task doc type): assigned / acked / done / help / verified /
    // task.nudge. `doc.created` with doc=taskx is a task assignment.
    case "doc.created":
    case "doc.claimed":
    case "doc.acknowledged":
    case "doc.done":
    case "doc.verified":
    case "doc.rejected":
    case "doc.help_requested":
    case "doc.retracted":
    case "doc.rebid": {
      if (p["doc"] !== "taskx") {
        // fall through to the default renderer for other doc types
        return msg
          ? t("toast.default", { seq: String(seq), type: t_, message: shorten(msg) })
          : t("toast.default_no_msg", { seq: String(seq), type: t_ })
      }
      const title = s("title")
      const tid = s("id")
      const kind = t_.replace("doc.", "")
      if (kind === "created") {
        return t("toast.task_assigned", {
          seq: String(seq),
          title: title || tid,
          id: tid,
          executor: String(p["executor"] ?? "agent"),
        })
      }
      if (kind === "done") return t("toast.task_done", { seq: String(seq), title: title || tid })
      if (kind === "help_requested") {
        return t("toast.task_help_requested", { seq: String(seq), title: title || tid, reason: shorten(s("note")) })
      }
      if (kind === "verified") return t("toast.task_verified", { seq: String(seq), title: title || tid })
      if (kind === "acknowledged") return t("toast.task_acknowledged", { seq: String(seq), title: title || tid })
      if (kind === "retracted" || kind === "rebid") {
        return t("toast.task_rebid", { seq: String(seq), title: title || tid, role: s("assignee_role") })
      }
      return msg
        ? t("toast.default", { seq: String(seq), type: t_, message: shorten(msg) })
        : t("toast.default_no_msg", { seq: String(seq), type: t_ })
    }
    case "task.nudge": {
      const body = msg ? shorten(msg) : ""
      return t("toast.task_nudge", { seq: String(seq), message: body })
    }
    case "system.nudge": {
      // Server-side idle-team reminder: "your task isn't done — finish or
      // submit". Surfaced loudly so the member knows the team is waiting.
      const body = msg ? shorten(msg) : ""
      return t("toast.nudge", { seq: String(seq), message: body })
    }
    case "system.nudge_lead": {
      // Server-side idle-team reminder aimed at the team lead(s): the team is
      // silent and someone needs to coordinate. Surfaced loudly for leads.
      const body = msg ? shorten(msg) : ""
      return t("toast.nudge_lead", { seq: String(seq), message: body })
    }
    default:
      return msg
        ? t("toast.default", { seq: String(seq), type: t_, message: shorten(msg) })
        : t("toast.default_no_msg", { seq: String(seq), type: t_ })
  }
}

/** Build a compact, human-readable digest of the sync payload. */
function summarize(data: SyncData): string {
  if (!data || typeof data !== "object") return ""
  const parts: string[] = []
  for (const tt of data.teams ?? []) {
    const team = tt.team
    const goal = tt.goal
    let line = t("digest.team_header", { name: team?.name ?? "-", state: team?.state ?? "-" })
    if (goal) line += t("digest.goal", { title: goal.title ?? "-", state: goal.state ?? "-" })
    parts.push(line)
    const members = (tt.members ?? []).map((m) => `${m.display_name ?? "-"}(${m.role ?? "-"}/${m.state ?? "-"})`)
    parts.push(t("digest.members", { list: members.join(", ") || "-" }))
    const open = (tt.questions ?? []).filter((q) => q.state === "open")
    if (open.length > 0) parts.push(t("digest.open_questions", { list: open.map((q) => q.question ?? "-").join(" | ") }))
  }
  const events = notableEvents(data?.new_events ?? [])
  if (events.length > 0) {
    parts.push(t("digest.new_events", { count: String(events.length), list: events.map(summarizeEvent).join(" | ") }))
  }
  return parts.join("\n")
}

/** True if the sync data shows this session is a team lead in ANY team (auto-execute excluded). */
export function isOwnerSession(data: SyncData): boolean {
  return (data?.teams ?? []).some((t) => t.team?.my_role === "owner" || t.team?.my_is_lead === true)
}

/** All member ids the session has across its teams. */
export function myMemberIds(data: SyncData): string[] {
  return myTeamCtxs(data)
    .map((c) => c.memberId)
    .filter((x): x is string => !!x)
}

/**
 * The member id of the current session. When the session belongs to several
 * teams this returns the FIRST team's id for backward compatibility; use
 * `myMemberIds` / `myTeamForEvent` for team-aware matching.
 */
export function myMemberId(data: SyncData): string | undefined {
  return data?.teams?.[0]?.team?.my_member_id
}

/**
 * Whether an event is a DIRECTED task assignment to the current session. A
 * publish carrying `assignee_member_id` that matches OUR member id in the
 * event's team is a task for us (regardless of publish type); everything else
 * (unassigned broadcasts, other people's tasks) is informational only.
 */
export function assignedToMe(data: SyncData, events: SyncEvent[]): boolean {
  if ((data?.teams?.length ?? 0) === 0) return false
  return events.some((e) => {
    const ctx = myTeamForEvent(data, e)
    const aid = e.payload?.assignee_member_id
    return !!ctx?.memberId && typeof aid === "string" && aid === ctx.memberId
  })
}

/** Whether an auto-execute should fire for the current refresh. */
export function shouldAutoExecute(opts: {
  data: SyncData
  events: SyncEvent[]
  lastExecutedSeq: number
}): boolean {
  if (!AUTO_EXECUTE) return false
  if ((opts.data?.teams?.length ?? 0) === 0) return false
  // A directed task NEWER than the last seq we already executed on triggers a
  // fresh auto-execute (so a member can be re-woken for later tasks). The
  // owner/lead exemption and the assignee match are both evaluated per team
  // (via the event's team_id), so a session that leads one team but works as a
  // regular member in another still auto-executes the latter's tasks.
  return opts.events.some((e) => {
    if ((e.seq ?? 0) <= opts.lastExecutedSeq) return false
    const ctx = myTeamForEvent(opts.data, e)
    if (!ctx?.memberId || ctx.isLead) return false
    const aid = e.payload?.assignee_member_id
    return typeof aid === "string" && aid === ctx.memberId
  })
}

export const Teamx: Plugin = async ({ client }) => {
  const instance = instanceId()

  const log = (
    level: "debug" | "info" | "warn" | "error",
    message: string,
    extra?: Record<string, unknown>,
  ) => {
    client.app.log({ body: { service: LOG_SERVICE, level, message, extra } }).catch(() => {})
  }

  // Per-session watermark of the highest seq we have already toasted, so the
  // M2 poller (which uses `sync --no-advance`) does not re-toast the same
  // events on every 15s poll. -1 = never notified yet (first refresh records
  // the watermark without notifying, so pre-existing backlog is not spammed).
  const notifiedSeq = new Map<string, number>()
  // Highest seq for which an auto-execute prompt has already been triggered
  // (per session), so we never double-wake a member for the same broadcast.
  const autoExecutedSeq = new Map<string, number>()
  // Whether a session is a team lead (owner sessions don't auto-execute on
  // broadcasts they themselves emit, and generally drive, not execute).
  const ownerSessions = new Map<string, boolean>()

  /**
   * Wake a member session (loopx-style): prompt it to pick up the assigned
   * task, set an opencode goal, and keep working until it is complete.
   * Uses the opencode SDK `session.promptAsync` to enqueue a user message.
   */
  async function triggerAutoExecute(sessionID: string, directiveSummary: string): Promise<void> {
    const message =
      `${t("auto_execute.trigger", { summary: directiveSummary || "See latest team broadcast" })}\n` +
      `${t("auto_execute.guard")}\n` +
      `${t("auto_execute.action")}`
    try {
      await client.session.promptAsync({
        path: { id: sessionID },
        body: {
          parts: [{ type: "text", text: message }],
        },
      })
      log("info", "auto-execute prompt sent", { sessionID, seq: autoExecutedSeq.get(sessionID) })
    } catch (e) {
      log("warn", "auto-execute prompt failed", { sessionID, error: String(e) })
    }
  }

  async function refreshDigest(sessionID: string): Promise<void> {
    const key = sessionKey(instance, sessionID)
    const r = await runCli(["sync", "--no-advance", "--session", key])
    if (!r.ok || !r.data) return
    const data = r.data as unknown as SyncData
    // My open tasks (taskx) — injected into the digest so the agent sees its
    // TODO without an explicit `task list` call. Human tasks are flagged so the
    // model knows to surface them to the user rather than auto-execute.
    let digest = summarize(data)
    const tasks = await runCli(["task", "list", "--mine", "--json", "--session", key])
    if (tasks.ok && tasks.data?.tasks && Array.isArray((tasks.data as { tasks?: unknown[] }).tasks)) {
      const open = ((tasks.data as { tasks: { id: string; title: string; state: string; executor?: string }[] }).tasks).filter(
        (tt) => tt.state !== "done" && tt.state !== "verified",
      )
      if (open.length > 0) {
        const lines = open.map((tt) => {
          // executor icons: 👤 human-only, 🤖 agent-only, 👤🤖 either (both may do it)
          const icon = tt.executor === "human" ? "👤" : tt.executor === "agent" ? "🤖" : "👤🤖"
          return `  ${icon} [${tt.state}] ${tt.title} (task ${tt.id})`
        })
        digest += `\n${t("digest.tasks_header", { count: String(open.length) })}\n${lines.join("\n")}`
      }
    }
    setDigest(sessionID, digest)
    const events = Array.isArray(data.new_events) ? data.new_events : []
    if (events.length === 0) return
    const seqs = events.map((e) => e.seq ?? 0).filter((s) => s > 0)
    const maxSeq = seqs.length > 0 ? Math.max(...seqs) : 0
    const lastNotified = notifiedSeq.get(sessionID) ?? -1
    // Advance the watermark across ALL events (including heartbeats) so
    // heartbeats never block real events, but only toast notable ones.
    if (maxSeq > lastNotified) notifiedSeq.set(sessionID, maxSeq)
    const fresh = notableEvents(events).filter((e) => (e.seq ?? 0) > lastNotified)
    if (lastNotified < 0 || fresh.length === 0) return
    const first = fresh[0]
    const last = fresh[fresh.length - 1]
    const lines = fresh.map(summarizeEvent).slice(0, 3)
    const more = fresh.length > lines.length ? `\n${t("toast_more", { count: String(fresh.length) })}` : ""
    await client.tui
      .showToast({
        body: {
          title: t("toast_title"),
          message: `${t("toast_new_events", { count: String(fresh.length), first: String(first?.seq ?? "?"), last: String(last?.seq ?? "?") })}\n${lines.join("\n")}${more}`,
          variant: "info",
        },
      })
      .catch(() => {})
    const hasQuestion = fresh.some((e) => e.type === "clarification.asked")
    if (hasQuestion) {
      await client.tui
        .appendPrompt({ body: { text: t("question_prompt") } })
        .catch(() => {})
    }
    // Idle-team nudge: the server reminded us the goal is still unfinished.
    // Append a prompt so the member (human or agent) is actually woken and
    // prompted to either finish or submit what they have.
    const hasNudge = fresh.some((e) => e.type === "system.nudge")
    if (hasNudge) {
      await client.tui
        .appendPrompt({ body: { text: t("nudge_prompt") } })
        .catch(() => {})
    }
    // Lead nudge: the team is silent and needs the team lead to coordinate.
    // A lead (owner/co-lead) gets a coordination prompt, not a "submit" one.
    const hasLeadNudge = fresh.some((e) => e.type === "system.nudge_lead")
    if (hasLeadNudge) {
      await client.tui
        .appendPrompt({ body: { text: t("nudge_lead_prompt") } })
        .catch(() => {})
    }
    // Auto-acknowledge taskx assignments to this member (automatic receipt).
    // A `doc.created` with doc=taskx and assignee_member_id == me (IN THAT
    // EVENT'S TEAM) means the lead assigned us a task; record receipt
    // immediately. Matching is team-aware so a multi-team session never misses
    // a task from a non-first team.
    const newTasks = fresh.filter((e) => {
      if (e.type !== "doc.created" || e.payload?.doc !== "taskx") return false
      const ctx = myTeamForEvent(data, e)
      return !!ctx?.memberId && e.payload?.assignee_member_id === ctx.memberId
    })
    for (const t of newTasks) {
      const tid = t.payload?.id as string | undefined
      if (!tid) continue
      await runCli(["task", "ack", tid, "--session", sessionKey(instance, sessionID)]).catch(() => {})
      log("info", "auto-acked task", { sessionID, task: tid })
    }

    // Bid tasks: auto-claim when the task is dispatched to OUR ROLE IN THE
    // EVENT'S TEAM and no one has claimed it yet. This covers fresh
    // `doc.created` (bid, assignee empty) and re-opened tasks (`doc.retracted`
    // / `doc.rebid`). The agent represents the user session, so it claims on
    // their behalf; the user can retract.
    const bidOpen = fresh.filter((e) => {
      if (e.payload?.doc !== "taskx") return false
      if (!(e.type === "doc.created" || e.type === "doc.retracted" || e.type === "doc.rebid")) return false
      if (e.payload?.assignee_member_id != null) return false
      const ctx = myTeamForEvent(data, e)
      return !!ctx?.role && e.payload?.assignee_role != null && e.payload?.assignee_role === ctx.role
    })
    for (const t of bidOpen) {
      const tid = t.payload?.id as string | undefined
      if (!tid) continue
      await runCli(["task", "claim", tid, "--session", sessionKey(instance, sessionID)]).catch(() => {})
      log("info", "auto-claimed bid task", { sessionID, task: tid, role: myTeamForEvent(data, t)?.role })
    }

    // Auto-execute ONLY for directed tasks assigned to this member. A publish
    // carrying `assignee_member_id == my_member_id` is a task for us; every
    // other broadcast (unassigned, or another member's task) is informational.
    const lastExecuted = autoExecutedSeq.get(sessionID) ?? 0
    const shouldRun = shouldAutoExecute({
      data,
      events: fresh,
      lastExecutedSeq: lastExecuted,
    })
    if (shouldRun) {
      autoExecutedSeq.set(sessionID, maxSeq)
      const directive = fresh.find((e) => {
        if ((e.seq ?? 0) <= lastExecuted) return false
        const ctx = myTeamForEvent(data, e)
        return !!ctx?.memberId && e.payload?.assignee_member_id === ctx.memberId
      })
      const isTask = directive?.payload?.doc === "taskx"
      const taskExecutor = isTask ? (directive?.payload?.executor as string | undefined) ?? "either" : undefined
      // A human-executor task must NOT auto-execute: it needs a person. Surface
      // it to the user instead of letting the agent run it.
      if (isTask && taskExecutor === "human") {
        await client.tui.appendPrompt({ body: { text: t("task_prompt_human") } }).catch(() => {})
      } else {
        // either / agent: the agent starts; for either, tell the user it can
        // take over at any time.
        if (isTask && taskExecutor === "either") {
          await client.tui.appendPrompt({ body: { text: t("task_prompt_either") } }).catch(() => {})
        }
        const summary = directive ? summarizeEvent(directive) : ""
        await triggerAutoExecute(sessionID, summary).catch(() => {})
      }
    }
  }

  // Refresh the digest for every known member session (shared by the poller
  // and the WS push path).
  function refreshAll() {
    for (const sid of knownMemberSessions()) {
      refreshDigest(sid).catch(() => {})
    }
  }

  // Debounce rapid WS event bursts into a single refresh.
  let refreshTimer: ReturnType<typeof setTimeout> | undefined
  function scheduleRefresh() {
    if (refreshTimer) return
    refreshTimer = setTimeout(() => {
      refreshTimer = undefined
      refreshAll()
    }, 200)
  }

  // Whether the live WS push is currently connected (when true, polling is idle).
  let wsConnected = false

  // M2 poller — fallback path only. When a live push connection is active the
  // poller stays idle (N3: zero polling while WS is up), and resumes when the
  // WS drops.
  let pollTimer: ReturnType<typeof setInterval> | undefined
  if (POLL_INTERVAL > 0) {
    pollTimer = setInterval(() => {
      if (wsConnected) return
      refreshAll()
    }, POLL_INTERVAL)
  }

  // N1/N3: live push. Open a WS connection when a network-mode server is
  // configured and refresh the digest in real time on each incoming event.
  // The per-session seq watermark (in refreshDigest) keeps toasts from
  // duplicating between the push and poll paths.
  let wsHandle: ReturnType<typeof connectWs> | undefined
  if (TEAMX_SERVER_URL) {
    wsHandle = connectWs({
      onEvent: () => scheduleRefresh(),
      onStatus: (connected) => {
        wsConnected = connected
        // Catch up immediately on (re)connect and on disconnect (before the
        // poller resumes), so nothing is missed across the switch.
        scheduleRefresh()
      },
      log: (level, message) => log(level === "debug" ? "debug" : "warn", message),
    })
  }

  // Reverse-tunnel auto-restore: re-open tunnels that were exposed before an
  // opencode restart (persisted in ~/.teamx/tunnels.json). Only the provider
  // machine that recorded them re-opens them; each is idempotent (a tunnel
  // that already exists on the server is reported and skipped by the server).
  const restoredTunnelHandles: { close(): void }[] = []
  if (TEAMX_SERVER_URL) {
    const { listTunnels } = await import("./tunnels-store")
    const { exposeTunnel } = await import("./tunnel")
    for (const t of listTunnels()) {
      if (t.server_url !== TEAMX_SERVER_URL) continue
      const handle = exposeTunnel({
        serverUrl: t.server_url,
        name: t.name,
        port: t.port,
        mode: t.mode ?? "local",
        lanIp: t.lan_ip,
        log: (level, message) => log(level === "info" ? "debug" : "warn", message),
      })
      restoredTunnelHandles.push(handle)
      handle.ready().then((pubPort) => {
        if (pubPort !== null) {
          log("info", `restored reverse tunnel "${t.name}" on public port ${pubPort}`)
        }
      })
    }
    // Re-open persisted consumer-side forwards (T2).
    const { listForwards } = await import("./tunnels-store")
    const { forwardTunnel } = await import("./tunnel")
    for (const f of listForwards()) {
      if (f.server_url !== TEAMX_SERVER_URL) continue
      const handle = forwardTunnel({
        serverUrl: f.server_url,
        name: f.name,
        localPort: f.local_port,
        log: (level, message) => log(level === "info" ? "debug" : "warn", message),
      })
      restoredTunnelHandles.push(handle)
      handle.ready().then((bound) => {
        if (bound !== null) {
          log("info", `restored local forward "${f.name}" on 127.0.0.1:${bound}`)
        }
      })
    }
  }

  return {
    tool: tools,

    event: async ({ event }) => {
      // Mirror session activity into the team ledger. Membership is checked
      // once per session and cached so non-member sessions never trigger a
      // `teamx` subprocess again.
      if (event.type !== "session.idle") return
      const props = event.properties as Record<string, unknown> | undefined
      const sessionID = props?.sessionID as string | undefined
      if (!sessionID) return

      let isMember = memberStatus(sessionID)
      // Resolve owner status independently of the membership cache. The
      // membership cache may already be populated (e.g. after using a teamx
      // tool), which used to skip this block and leave ownerSessions empty,
      // letting the owner's own broadcasts auto-execute on itself.
      if (isMember === undefined || !ownerSessions.has(sessionID)) {
        const key = sessionKey(instance, sessionID)
        const r = await runCli(["team", "list", "--session", key])
        const teams = r.data?.teams as { my_role?: string; my_is_lead?: boolean }[] | undefined
        isMember = r.ok && Array.isArray(teams) && teams.length > 0
        markMember(sessionID, isMember)
        // An owner's / co-lead's own broadcasts shouldn't auto-execute on itself.
        const isOwner = r.ok && Array.isArray(teams) && teams.some((t) => t.my_role === "owner" || t.my_is_lead === true)
        ownerSessions.set(sessionID, isOwner)
        if (isMember) refreshDigest(sessionID).catch(() => {})
      }
      if (!isMember) return

      const r = await runCli([
        "publish",
        "activity",
        "--data",
        JSON.stringify({ kind: "session.idle" }),
        "--session",
        sessionKey(instance, sessionID),
      ])
      if (!r.ok) {
        log("debug", "activity publish failed", { sessionID, stderr: r.stderr })
      }
    },

    "experimental.chat.system.transform": async ({ sessionID }, { system }) => {
      if (!sessionID) return
      const digest = getDigest(sessionID)
      if (!digest) return
      system.push(t("system_prompt_prefix") + "\n" + digest)
    },

    dispose: async () => {
      if (pollTimer) clearInterval(pollTimer)
      if (refreshTimer) clearTimeout(refreshTimer)
      wsHandle?.close()
      for (const h of restoredTunnelHandles) h.close()
    },
  }
}

export default {
  id: "teamx",
  server: Teamx,
}
