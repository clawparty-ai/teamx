// teamx opencode plugin (V1 + M2).
//
// - Registers the teamx_* tool family.
// - `event` hook mirrors session activity (session.idle) into the ledger.
// - M2 (poll-based, no server): a poller refreshes a per-session team digest,
//   `experimental.chat.system.transform` injects it into the next request, and
//   `client.tui` toasts/append-prompt notify on new events. Gate with
//   TEAMX_POLL_INTERVAL (ms, default 15000; 0 disables polling).

import type { Plugin } from "@opencode-ai/plugin"
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import {
  dirOfSession,
  getDigest,
  instanceId,
  knownMemberSessions,
  markMember,
  memberStatus,
  noteSessionDir,
  runCli,
  serverUrlFor,
  sessionKey,
  setDigest,
  teamxDir,
} from "./client"
import { tools } from "./tools"
import { maybeAutoClone } from "./autoclone"
import { connectWs } from "./ws"
import { collect, initActivity } from "./activity"
import { t } from "./i18n"

const LOG_SERVICE = "teamx"
const POLL_INTERVAL = Number(process.env.TEAMX_POLL_INTERVAL ?? 15000)
/** How often the WS watcher re-resolves the server URL (late-letter pickup). */
const WS_WATCH_INTERVAL = Number(process.env.TEAMX_WS_WATCH_INTERVAL ?? 10_000)
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

/** Current UTC time as an RFC3339 string. */
function nowIso(): string {
  return new Date().toISOString()
}

// ---------------------------------------------------------------------------
// A3 work_session tracking: the idle→idle gap is one "work segment". A segment
// is closed when the session goes idle; if any human activity (input, approval,
// command) happened inside it, has_human is set. Uses a module-level map so
// multiple sessions are tracked independently.
// ---------------------------------------------------------------------------
type WorkSegment = {
  startedAt: string
  hasHuman: boolean
  task: string
  cost: number
  tokensInput: number
  tokensOutput: number
  tokensReasoning: number
  tokensCacheRead: number
  tokensCacheWrite: number
}
const workSegments = new Map<string, WorkSegment>()

/** Per-session model attribution (sessionID -> modelID/providerID), filled by
 * the `message.updated` hook and consumed by step_finish + work_session rows. */
const sessionModel = new Map<string, { modelID: string; providerID: string }>()

// Presence counters (real-time): tool part ids currently running + open pty ids.
const runningTools = new Set<string>()
const openTerminals = new Set<string>()

/** Record human activity inside the current work segment (if any). */
function markHuman(sessionID: string | undefined, text?: string): void {
  if (!sessionID) return
  const seg = workSegments.get(sessionID)
  if (!seg) return
  seg.hasHuman = true
  if (text && !seg.task) seg.task = text.replace(/\s+/g, " ").trim().slice(0, 140)
}

/** Accumulate a step's cost/tokens into the current work segment. */
function accumulateSegment(
  sessionID: string | undefined,
  step: {
    cost?: number
    tokensInput?: number
    tokensOutput?: number
    tokensReasoning?: number
    tokensCacheRead?: number
    tokensCacheWrite?: number
  },
): void {
  if (!sessionID) return
  const seg = workSegments.get(sessionID)
  if (!seg) return
  seg.cost += step.cost ?? 0
  seg.tokensInput += step.tokensInput ?? 0
  seg.tokensOutput += step.tokensOutput ?? 0
  seg.tokensReasoning += step.tokensReasoning ?? 0
  seg.tokensCacheRead += step.tokensCacheRead ?? 0
  seg.tokensCacheWrite += step.tokensCacheWrite ?? 0
}

/**
 * Close the current work segment for a session and emit a `work_session`
 * activity row (started_at = segment start, ended_at = now, duration_ms =
 * elapsed). If no segment is open (e.g. idle fired without a prior busy turn),
 * nothing is emitted.
 */
function closeWorkSegment(sessionID: string, now: string): void {
  const seg = workSegments.get(sessionID)
  workSegments.delete(sessionID)
  if (!seg) return
  const startMs = Date.parse(seg.startedAt)
  const endMs = Date.parse(now)
  if (!Number.isFinite(startMs) || !Number.isFinite(endMs) || endMs <= startMs) return
  const model = sessionModel.get(sessionID)
  collect({
    node_id: instanceId(dirOfSession(sessionID)),
    started_at: seg.startedAt,
    ended_at: now,
    duration_ms: endMs - startMs,
    kind: "work_session",
    detail: { sessionID, task: seg.task || undefined },
    has_human: seg.hasHuman,
    // Cost/tokens aggregated from the steps that ran inside this segment.
    cost: seg.cost > 0 ? seg.cost : undefined,
    tokens_input: seg.tokensInput > 0 ? seg.tokensInput : undefined,
    tokens_output: seg.tokensOutput > 0 ? seg.tokensOutput : undefined,
    tokens_reasoning: seg.tokensReasoning > 0 ? seg.tokensReasoning : undefined,
    tokens_cache_read: seg.tokensCacheRead > 0 ? seg.tokensCacheRead : undefined,
    tokens_cache_write: seg.tokensCacheWrite > 0 ? seg.tokensCacheWrite : undefined,
    model_id: model?.modelID || undefined,
    provider_id: model?.providerID || undefined,
  })
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
    case "membership.auto_approved":
      return t("toast.membership_auto_approved", { seq: String(seq), name: s("display_name") })
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
    case "discussion.created": {
      const author = s("author") || ""
      const title = s("title") || "讨论"
      return `💬 ${author} 创建讨论「${title}」`
    }
    case "discussion.message": {
      const author = s("author") || ""
      const body = shorten(msg || s("body") || "")
      return `💬 ${author} 在讨论中回复：${body}`
    }
    case "discussion.closed": {
      const title = s("title") || "讨论"
      return `🔒 讨论「${title}」已关闭`
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
  // assignee match is evaluated per team (via the event's team_id), so a session
  // that leads one team but works as a regular member in another still
  // auto-executes the latter's tasks.
  //
  // Broadcasts (no assignee) never wake a session — that is the owner/lead
  // "drive, don't execute" rule. But an event explicitly DIRECTED at us
  // (`assignee_member_id == our member id`) is a real task even for a lead
  // session: a lead-role machine (e.g. a build node that also leads a team) must
  // be wakeable by work assigned to it. To avoid self-wake loops we still ignore
  // a lead's OWN directed publish (the event it emitted itself).
  return opts.events.some((e) => {
    if ((e.seq ?? 0) <= opts.lastExecutedSeq) return false
    const ctx = myTeamForEvent(opts.data, e)
    if (!ctx?.memberId) return false
    const aid = e.payload?.assignee_member_id
    if (typeof aid !== "string" || aid !== ctx.memberId) return false
    if (ctx.isLead && e.member_id === ctx.memberId) return false
    return true
  })
}

export const Teamx: Plugin = async ({ client }) => {
  // Per-session workspace: the teamx space is the session's directory `.teamx/`.
  // `sk` builds a session key from that workspace's instance id.
  const sk = (sid: string | undefined): string => sessionKey(instanceId(dirOfSession(sid)), sid)

  // Enable the enterprise activity collector. The flush path needs a session
  // key to attribute rows in V1 local mode; network mode uses the mTLS cert.
  // The resolver must be safe in headless `opencode run` (worker), where no
  // interactive session id exists: return undefined instead of throwing.
  initActivity({
    sessionKey: () => (currentSessionID ? sk(currentSessionID) : undefined),
    log: (level, message, extra) => log(level, message, extra),
  })

  // The most recent session id seen by the event hook, used as the fallback
  // session for activity flushes (V1 local mode).
  let currentSessionID: string | undefined

  const log = (
    level: "debug" | "info" | "warn" | "error",
    message: string,
    extra?: Record<string, unknown>,
  ) => {
    client.app.log({ body: { service: LOG_SERVICE, level, message, extra } }).catch(() => {})
  }

  /**
   * Best-effort: report opencode's live model pricing to the server
   * (`model.pricing_sync`, source = 'opencode') so cost fallback uses real
   * prices instead of only the built-in seed. Reads the opencode config's
   * per-model `cost` (USD / 1M tokens). Network mode only.
   */
  async function syncModelPricing(): Promise<void> {
    try {
      const resp = (await client.config.get()) as unknown as {
        data?: unknown
        provider?: Record<string, unknown>
      }
      const providers = (resp?.data ?? resp ?? {}) as { provider?: Record<string, unknown> }
      const rows: Array<{
        provider_id: string
        model_id: string
        price_input: number
        price_output: number
        price_cache_read: number
        price_cache_write: number
        over200k_input?: number
        over200k_output?: number
        over200k_cache_read?: number
        over200k_cache_write?: number
      }> = []
      for (const [providerID, provider] of Object.entries(providers.provider ?? {})) {
        const models = (provider as { models?: Record<string, unknown> })?.models ?? {}
        for (const [modelKey, m] of Object.entries(models)) {
          const cost = (m as { cost?: Record<string, unknown> })?.cost
          if (!cost) continue
          const modelID = ((m as { id?: string })?.id ?? modelKey) as string
          const over = cost.context_over_200k as Record<string, unknown> | undefined
          rows.push({
            provider_id: providerID,
            model_id: modelID,
            price_input: (cost.input as number) ?? 0,
            price_output: (cost.output as number) ?? 0,
            price_cache_read: (cost.cache_read as number) ?? 0,
            price_cache_write: (cost.cache_write as number) ?? 0,
            over200k_input: (over?.input as number) ?? undefined,
            over200k_output: (over?.output as number) ?? undefined,
            over200k_cache_read: (over?.cache_read as number) ?? undefined,
            over200k_cache_write: (over?.cache_write as number) ?? undefined,
          })
        }
      }
      if (rows.length === 0) return
      const r = await runCli(["model", "pricing-sync", "--data", JSON.stringify(rows)], { cwd: dirOfSession(undefined) })
      if (!r.ok) log("debug", "model pricing sync failed", { stderr: r.stderr })
    } catch (e) {
      log("debug", "model pricing sync error", { error: String(e) })
    }
  }

  // Report model pricing once at startup (network mode only).
  if (serverUrlFor(dirOfSession(undefined))) {
    void syncModelPricing()
  }

  // ---- Enterprise presence: push a live snapshot to the server (network mode).
  // `active_sessions` = busy work segments, `running_tools` = running tool parts,
  // `open_terminals` = open ptys. Pushed on change (debounced) + a heartbeat
  // keepalive so the server's TTL does not expire an idle-but-online member.
  const PRESENCE_INTERVAL = Number(process.env.TEAMX_PRESENCE_INTERVAL ?? 10000)
  let presenceTimer: ReturnType<typeof setTimeout> | undefined
  function presenceSnapshot() {
    return {
      active_sessions: workSegments.size,
      running_tools: runningTools.size,
      open_terminals: openTerminals.size,
      // 上报本会话的工作区目录，供 team status / console 展示每个成员在哪工作。
      workspace: dirOfSession(undefined),
    }
  }
  async function pushPresence(): Promise<void> {
    if (!serverUrlFor(dirOfSession(undefined))) return
    const r = await runCli(["presence", "push", "--data", JSON.stringify(presenceSnapshot())], { cwd: dirOfSession(undefined) })
    if (!r.ok) log("debug", "presence push failed", { stderr: r.stderr })
  }
  function schedulePresencePush(): void {
    if (presenceTimer) return
    presenceTimer = setTimeout(() => {
      presenceTimer = undefined
      void pushPresence()
    }, 500)
  }
  if (PRESENCE_INTERVAL > 0) {
    setInterval(() => {
      void pushPresence()
    }, PRESENCE_INTERVAL)
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

  // ---------------------------------------------------------------------------
// Persisted "last surfaced nudge" cursor (per workspace). Survives opencode
// restarts so offline/restart nudges are surfaced exactly once.
// ---------------------------------------------------------------------------
function nudgeCursorFile(cwd: string): string {
  return join(teamxDir(cwd), "nudge-cursor")
}

function readNudgeCursor(cwd: string): number {
  try {
    const raw = readFileSync(nudgeCursorFile(cwd), "utf8").trim()
    const n = Number(raw)
    return Number.isFinite(n) && n > 0 ? n : 0
  } catch {
    return 0
  }
}

function writeNudgeCursor(cwd: string, seq: number): void {
  try {
    mkdirSync(teamxDir(cwd), { recursive: true })
    writeFileSync(nudgeCursorFile(cwd), String(seq))
  } catch {
    // best-effort
  }
}

let lastLeadAutoRun = 0

async function autoRunLeadNudge(sessionID: string): Promise<void> {
  if (process.env.TEAMX_AUTO_REPLY === "0") return
  const now = Date.now()
  if (now - lastLeadAutoRun < 300_000) return
  lastLeadAutoRun = now
  const cwd = dirOfSession(sessionID)
  const prompt =
    "你是 team lead。团队静默提醒已到：请先用 teamx_sync / teamx_status 查看详情，然后自主协调——检查成员进度、审批待办入队、广播下一步安排或调整目标；用 teamx 工具直接完成，不要询问用户。"
  try {
    const bin = process.env.OPENCODE_BIN ?? "opencode"
    Bun.spawn([bin, "run", "-c", prompt], { cwd, stdout: "ignore", stderr: "ignore" })
    log("info", "auto-run lead nudge", { sessionID })
  } catch (e) {
    log("warn", "auto-run lead nudge failed", { sessionID, error: String(e) })
  }
}

async function refreshDigest(sessionID: string): Promise<void> {
    const key = sk(sessionID)
    const cwd = dirOfSession(sessionID)
    const r = await runCli(["sync", "--no-advance", "--session", key], { cwd })
    if (!r.ok || !r.data) return
    // First-login convenience: auto-clone the team's default git repo into a
    // fresh network workspace (one-time, opt-out via TEAMX_NO_AUTO_CLONE=1).
    try {
      await maybeAutoClone(cwd)
    } catch {
      // never block the digest on an optional clone
    }
    const data = r.data as unknown as SyncData
    // My open tasks (taskx) — injected into the digest so the agent sees its
    // TODO without an explicit `task list` call. Human tasks are flagged so the
    // model knows to surface them to the user rather than auto-execute.
    let digest = summarize(data)
    const tasks = await runCli(["task", "list", "--mine", "--json", "--session", key], { cwd: dirOfSession(sessionID) })
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

    // Persisted nudge cursor: nudges that arrived while this member was
    // offline / while opencode was (re)started are surfaced once — appended to
    // the digest as a TODO reminder AND appended as a prompt. The cursor is
    // stored per workspace, so a restart never loses "you have an unread team
    // nudge".
    const cursor = readNudgeCursor(cwd)
    const unreadNudges = notableEvents(events).filter(
      (e) =>
        (e.type === "system.nudge" || e.type === "system.nudge_lead") &&
        (e.seq ?? 0) > cursor,
    )
    if (unreadNudges.length > 0) {
      const newest = unreadNudges[unreadNudges.length - 1]
      digest = digest + "\n\n" + t("digest.unread_nudge") + "\n" + summarizeEvent(newest)
      setDigest(sessionID, digest)
      const hasLeadNudge = unreadNudges.some((e) => e.type === "system.nudge_lead")
      if (hasLeadNudge && isOwnerSession(data)) {
        // The lead auto-coordinates: run a headless turn that continues THIS
        // session (opencode run -c), so the TUI reflects it without Enter.
        void autoRunLeadNudge(sessionID)
      } else {
        const promptText = newest.type === "system.nudge_lead" ? t("nudge_lead_prompt") : t("nudge_prompt")
        await client.tui.appendPrompt({ body: { text: promptText } }).catch(() => {})
      }
      writeNudgeCursor(cwd, newest.seq ?? maxSeq)
    }

    // First refresh after the process (re)started: skip the toast backlog
    // (anti-spam) — unread nudges were already handled above.
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
    // (nudge / nudge_lead prompts are handled by the persisted cursor above,
    // covering both realtime arrivals and offline/restart backlog.)
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
      await runCli(["task", "ack", tid, "--session", sk(sessionID)], { cwd: dirOfSession(sessionID) }).catch(() => {})
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
      await runCli(["task", "claim", tid, "--session", sk(sessionID)], { cwd: dirOfSession(sessionID) }).catch(() => {})
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

  // N1/N3: live push watcher (Issue #6). The server URL is re-resolved every
  // few seconds instead of once at startup, so an invitation letter imported
  // into a RUNNING opencode session (the common cross-machine onboarding
  // path) establishes /ws without a restart; a changed URL (newer letter)
  // rebuilds the connection, and losing all letters drops it. In-connection
  // retries live in connectWs itself (exponential backoff + jitter).
  // The per-session seq watermark (in refreshDigest) keeps toasts from
  // duplicating between the push and poll paths.
  let wsHandle: ReturnType<typeof connectWs> | undefined
  let wsCurrentUrl = ""
  const ensureWs = () => {
    const url = serverUrlFor(dirOfSession(undefined))
    if (!url) {
      if (wsHandle) {
        wsHandle.close()
        wsHandle = undefined
        if (wsConnected) {
          wsConnected = false
          scheduleRefresh()
        }
        log("debug", "ws push stopped: no server URL (no invitation letter)")
      }
      wsCurrentUrl = ""
      return
    }
    if (wsHandle && url === wsCurrentUrl) return
    if (wsHandle) {
      wsHandle.close()
      wsHandle = undefined
    }
    wsCurrentUrl = url
    wsHandle = connectWs({
      serverUrl: url,
      cwd: dirOfSession(undefined),
      onEvent: () => scheduleRefresh(),
      onStatus: (connected) => {
        wsConnected = connected
        // Catch up immediately on (re)connect and on disconnect (before the
        // poller resumes), so nothing is missed across the switch.
        scheduleRefresh()
      },
      log: (level, message) => log(level === "debug" ? "debug" : "warn", message),
    })
    if (!wsBootedWithUrl) {
      log("info", `ws push connected to ${url} (letter picked up at runtime)`)
    }
  }
  const wsBootedWithUrl = serverUrlFor(dirOfSession(undefined)) !== ""
  ensureWs()
  const wsWatchTimer = setInterval(ensureWs, WS_WATCH_INTERVAL)
  void wsWatchTimer

  // Reverse-tunnel auto-restore: re-open tunnels that were exposed before an
  // opencode restart (persisted in ~/.teamx/tunnels.json). Only the provider
  // machine that recorded them re-opens them; each is idempotent (a tunnel
  // that already exists on the server is reported and skipped by the server).
  const restoredTunnelHandles: { close(): void }[] = []
  const restoreCwd = dirOfSession(undefined)
  const restoreServerUrl = serverUrlFor(restoreCwd)
  if (restoreServerUrl) {
    const { listTunnels } = await import("./tunnels-store")
    const { exposeTunnel } = await import("./tunnel")
    for (const t of listTunnels(restoreCwd)) {
      if (t.server_url !== restoreServerUrl) continue
      const handle = exposeTunnel({
        serverUrl: t.server_url,
        name: t.name,
        port: t.port,
        mode: t.mode ?? "local",
        lanIp: t.lan_ip,
        cwd: restoreCwd,
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
    for (const f of listForwards(restoreCwd)) {
      if (f.server_url !== restoreServerUrl) continue
      const handle = forwardTunnel({
        serverUrl: f.server_url,
        name: f.name,
        localPort: f.local_port,
        cwd: restoreCwd,
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
      const props = event.properties as Record<string, unknown> | undefined
      const sessionID = (props?.sessionID as string | undefined) ?? (props?.session_id as string | undefined)
      if (sessionID) currentSessionID = sessionID

      // ---- Enterprise activity collection (A2): record work + human actions.
      // Collected for member sessions only; the flush path handles both modes.
      const record = (row: Parameters<typeof collect>[0]) => {
        // Non-member sessions are skipped (cache check; resolved lazily below).
        if (isMember !== true) return
        collect(row)
      }

      // Membership: only resolve once per session (cached). Unknown sessions
      // trigger a one-time `team list` to decide.
      let isMember = sessionID ? memberStatus(sessionID) : undefined
      if (sessionID && (isMember === undefined || !ownerSessions.has(sessionID))) {
        const key = sk(sessionID)
        const r = await runCli(["team", "list", "--session", key], { cwd: dirOfSession(sessionID) })
        const teams = r.data?.teams as { my_role?: string; my_is_lead?: boolean }[] | undefined
        isMember = r.ok && Array.isArray(teams) && teams.length > 0
        markMember(sessionID, isMember)
        // An owner's / co-lead's own broadcasts shouldn't auto-execute on itself.
        const isOwner = r.ok && Array.isArray(teams) && teams.some((t) => t.my_role === "owner" || t.my_is_lead === true)
        ownerSessions.set(sessionID, isOwner)
        if (isMember) refreshDigest(sessionID).catch(() => {})
      }

      switch (event.type) {
        case "session.idle": {
          if (!sessionID || isMember !== true) return
          // Keep the V1 heartbeat publish (existing behavior).
          const r = await runCli(
            [
              "publish",
              "activity",
              "--data",
              JSON.stringify({ kind: "session.idle" }),
              "--session",
              sk(sessionID),
            ],
            { cwd: dirOfSession(sessionID) },
          )
          if (!r.ok) {
            log("debug", "activity publish failed", { sessionID, stderr: r.stderr })
          }
          // A3 work_session: close the previous work segment now that the
          // session is idle; the segment's has_human flag is set by any human
          // activity that happened inside it (see closeWorkSegment).
          closeWorkSegment(sessionID, nowIso())
          return
        }

        case "session.status": {
          if (!sessionID || isMember !== true) return
          const status = (props?.status as Record<string, unknown> | undefined)?.type
          if (status === "busy" && !workSegments.has(sessionID)) {
            // A turn is starting: open a work segment. Its end is the next
            // session.idle (closeWorkSegment).
            workSegments.set(sessionID, {
              startedAt: nowIso(),
              hasHuman: false,
              task: "",
              cost: 0,
              tokensInput: 0,
              tokensOutput: 0,
              tokensReasoning: 0,
              tokensCacheRead: 0,
              tokensCacheWrite: 0,
            })
          }
          return
        }

        case "message.updated": {
          // Record model attribution per session so step_finish rows can carry
          // model_id/provider_id (needed for the cost pricing table).
          const info = props?.info as Record<string, unknown> | undefined
          const sid = (info?.sessionID as string | undefined) ?? sessionID
          const modelID = info?.modelID as string | undefined
          const providerID = info?.providerID as string | undefined
          if (sid && (modelID || providerID)) {
            sessionModel.set(sid, { modelID: modelID ?? "", providerID: providerID ?? "" })
          }
          return
        }

        case "pty.created": {
          // Presence: track open terminals.
          const pid = (props?.info as Record<string, unknown> | undefined)?.id as string | undefined
          if (pid) { openTerminals.add(pid); schedulePresencePush() }
          return
        }

        case "pty.deleted":
        case "pty.exited": {
          const pid = props?.id as string | undefined
          if (pid) { openTerminals.delete(pid); schedulePresencePush() }
          return
        }

        case "message.part.updated": {
          if (isMember !== true) return
          const part = props?.part as Record<string, unknown> | undefined
          if (!part) return
          const started = nowIso()
          if (part.type === "tool") {
            const state = (part.state ?? {}) as Record<string, unknown>
            const input = state.input ?? {}
            const status = state.status ?? "unknown"
            const time = state.time as { start?: number; end?: number } | undefined
            const durationMs = time?.start && time?.end ? time.end - time.start : undefined
            // Presence: track running tools by part id (running vs completed).
            const pid = (part as { id?: string }).id
            if (pid) {
              const done = status === "completed" || (time?.start != null && time?.end != null)
              if (done) runningTools.delete(pid)
              else runningTools.add(pid)
              schedulePresencePush()
            }
            // Record each tool call ONCE, at its terminal state (completed /
            // error), so one call does not create duplicates and we can carry
            // the tool's output alongside its arguments (bash etc.).
            const terminal = status === "completed" || status === "error"
            if (terminal) {
              const st = state as Record<string, unknown>
              const outRaw = status === "completed" && st.output != null ? String(st.output) : undefined
              const out = outRaw != null && outRaw.length > 4000 ? outRaw.slice(0, 4000) + "\n…(truncated)" : outRaw
              record({
                node_id: instanceId(dirOfSession(sessionID)),
                started_at: time?.start ? new Date(time.start).toISOString() : started,
                duration_ms: durationMs,
                kind: "tool_call",
                detail: {
                  tool: part.tool,
                  state: status,
                  arguments: input,
                  output: out,
                },
              })
            }
          } else if (part.type === "step-finish") {
            const tokens = (part.tokens ?? {}) as Record<string, unknown> | undefined
            const cache = (tokens?.cache ?? {}) as Record<string, unknown> | undefined
            const ti = (tokens?.input as number) ?? undefined
            const to = (tokens?.output as number) ?? undefined
            const tr = (tokens?.reasoning as number) ?? undefined
            const cr = (cache?.read as number) ?? undefined
            const cw = (cache?.write as number) ?? undefined
            // Model attribution: recorded by the `message.updated` hook.
            const model = sessionID ? sessionModel.get(sessionID) : undefined
            record({
              node_id: instanceId(dirOfSession(sessionID)),
              started_at: started,
              kind: "step_finish",
              // Note: input/output include cached read tokens for accounting.
              detail: {
                reason: (part.reason as string) ?? undefined,
                cache_read: cr,
                cache_write: cw,
              },
              tokens_input: ti,
              tokens_output: to,
              tokens_reasoning: tr,
              tokens_cache_read: cr,
              tokens_cache_write: cw,
              cost: (part.cost as number) ?? undefined,
              model_id: model?.modelID || undefined,
              provider_id: model?.providerID || undefined,
            })
            // A3: accumulate this step's cost/tokens into the open work segment
            // so the eventual work_session row carries the aggregated cost.
            accumulateSegment(sessionID, {
              cost: (part.cost as number) ?? 0,
              tokensInput: ti ?? 0,
              tokensOutput: to ?? 0,
              tokensReasoning: tr ?? 0,
              tokensCacheRead: cr ?? 0,
              tokensCacheWrite: cw ?? 0,
            })
          }
          return
        }

        case "command.executed": {
          if (isMember !== true) return
          record({
            node_id: instanceId(dirOfSession(sessionID)),
            started_at: nowIso(),
            kind: "command",
            detail: { name: (props?.name as string) ?? undefined, args: (props?.arguments as string) ?? undefined },
          })
          return
        }

        case "file.edited": {
          if (isMember !== true) return
          record({
            node_id: instanceId(dirOfSession(sessionID)),
            started_at: nowIso(),
            kind: "file_edit",
            detail: { file: (props?.file as string) ?? undefined },
          })
          return
        }

        case "permission.replied": {
          if (isMember !== true) return
          record({
            node_id: instanceId(dirOfSession(sessionID)),
            started_at: nowIso(),
            kind: "human_approval",
            detail: {
              permissionID: (props?.permissionID as string) ?? undefined,
              response: (props?.response as string) ?? undefined,
            },
          })
          markHuman(sessionID)
          return
        }

        case "tui.command.execute": {
          if (isMember !== true) return
          record({
            node_id: instanceId(dirOfSession(sessionID)),
            started_at: nowIso(),
            kind: "human_command",
            detail: { name: (props?.command as string) ?? undefined },
          })
          markHuman(sessionID)
          return
        }

        default:
          return
      }
    },

    // Human input: the chat.message hook fires when a user message arrives,
    // with the full UserMessage + parts (text). This is the "human actually
    // did X" signal.
    "chat.message": async ({ sessionID }, { message, parts }) => {
      if (!sessionID || memberStatus(sessionID) !== true) return
      currentSessionID = sessionID
      const texts = (parts ?? [])
        .filter((p) => p.type === "text")
        .map((p) => (p as { text?: string }).text ?? "")
        .join("\n")
      collect({
        node_id: instanceId(dirOfSession(sessionID)),
        started_at: nowIso(),
        kind: "human_input",
        detail: { sessionID, text: texts || undefined },
      })
      markHuman(sessionID, texts)
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
      const { drain } = await import("./activity")
      drain()
    },
  }
}

export default {
  id: "teamx",
  server: Teamx,
}
