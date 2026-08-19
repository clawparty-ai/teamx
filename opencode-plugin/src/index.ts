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
import { collect, initActivity } from "./activity"
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
  team?: { name?: string; state?: string; my_role?: string; my_member_id?: string }
  goal?: { title?: string; state?: string } | null
  members?: MemberRow[]
  questions?: QuestionRow[]
}
type SyncEvent = { seq?: number; type?: string; member_id?: string; payload?: Record<string, unknown> }
type SyncData = { teams?: TeamBlock[]; new_events?: SyncEvent[] }

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
const workSegments = new Map<string, { startedAt: string; hasHuman: boolean }>()

/** Record human activity inside the current work segment (if any). */
function markHuman(sessionID: string | undefined): void {
  if (!sessionID) return
  const seg = workSegments.get(sessionID)
  if (seg) seg.hasHuman = true
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
  collect({
    node_id: instanceId(),
    started_at: seg.startedAt,
    ended_at: now,
    duration_ms: endMs - startMs,
    kind: "work_session",
    detail: { sessionID },
    has_human: seg.hasHuman,
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
    case "membership.denied":
      return t("toast.membership_denied", { seq: String(seq), name: s("display_name") })
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

/** True if the sync data shows this session is a team lead (auto-execute excluded). */
export function isOwnerSession(data: SyncData): boolean {
  return (data?.teams ?? []).some((t) => t.team?.my_role === "owner")
}

/** The member id of the current session, from the sync response. */
export function myMemberId(data: SyncData): string | undefined {
  return data?.teams?.[0]?.team?.my_member_id
}

/**
 * Whether a broadcast event is a DIRECTED task assignment to the current
 * session. Any publish carrying `assignee_member_id` that matches our member id
 * is a task for us (regardless of publish type); everything else (unassigned
 * broadcasts, other people's tasks) is informational only.
 */
export function assignedToMe(events: SyncEvent[], myId: string | undefined): boolean {
  if (!myId) return false
  return events.some((e) => e.payload?.assignee_member_id === myId)
}

/** Whether an auto-execute should fire for the current refresh. */
export function shouldAutoExecute(opts: {
  data: SyncData
  events: SyncEvent[]
  lastExecutedSeq: number
}): boolean {
  if (!AUTO_EXECUTE || isOwnerSession(opts.data)) return false
  const myId = myMemberId(opts.data)
  if (!myId) return false
  // A directed task NEWER than the last seq we already executed on triggers a
  // fresh auto-execute (so a member can be re-woken for later tasks).
  return opts.events.some(
    (e) => e.payload?.assignee_member_id === myId && (e.seq ?? 0) > opts.lastExecutedSeq,
  )
}

export const Teamx: Plugin = async ({ client }) => {
  const instance = instanceId()

  // Enable the enterprise activity collector. The flush path needs a session
  // key to attribute rows in V1 local mode; network mode uses the mTLS cert.
  initActivity({
    sessionKey: () => sessionKey(instance, currentSessionID),
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
    setDigest(sessionID, summarize(data))
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
      const directive = fresh.find(
        (e) => e.payload?.assignee_member_id === myMemberId(data) && (e.seq ?? 0) > lastExecuted,
      )
      const summary = directive ? summarizeEvent(directive) : ""
      await triggerAutoExecute(sessionID, summary).catch(() => {})
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
        const key = sessionKey(instance, sessionID)
        const r = await runCli(["team", "list", "--session", key])
        const teams = r.data?.teams as { my_role?: string }[] | undefined
        isMember = r.ok && Array.isArray(teams) && teams.length > 0
        markMember(sessionID, isMember)
        const isOwner = r.ok && Array.isArray(teams) && teams.some((t) => t.my_role === "owner")
        ownerSessions.set(sessionID, isOwner)
        if (isMember) refreshDigest(sessionID).catch(() => {})
      }

      switch (event.type) {
        case "session.idle": {
          if (!sessionID || isMember !== true) return
          // Keep the V1 heartbeat publish (existing behavior).
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
            workSegments.set(sessionID, { startedAt: nowIso(), hasHuman: false })
          }
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
            record({
              node_id: instance,
              started_at: time?.start ? new Date(time.start).toISOString() : started,
              duration_ms: durationMs,
              kind: "tool_call",
              detail: { tool: part.tool, state: status, arguments: input },
            })
          } else if (part.type === "step-finish") {
            const tokens = (part.tokens ?? {}) as Record<string, unknown> | undefined
            const cache = (tokens?.cache ?? {}) as Record<string, unknown> | undefined
            const ti = (tokens?.input as number) ?? undefined
            const to = (tokens?.output as number) ?? undefined
            const tr = (tokens?.reasoning as number) ?? undefined
            record({
              node_id: instance,
              started_at: started,
              kind: "step_finish",
              detail: { reason: (part.reason as string) ?? undefined },
              tokens_input: ti,
              tokens_output: to,
              tokens_reasoning: tr,
              cost: (part.cost as number) ?? undefined,
              // Note: input/output include cached read tokens for accounting.
              ...(cache && {
                detail: {
                  reason: (part.reason as string) ?? undefined,
                  cache_read: (cache.read as number) ?? undefined,
                  cache_write: (cache.write as number) ?? undefined,
                },
              }),
            })
          }
          return
        }

        case "command.executed": {
          if (isMember !== true) return
          record({
            node_id: instance,
            started_at: nowIso(),
            kind: "command",
            detail: { name: (props?.name as string) ?? undefined, args: (props?.arguments as string) ?? undefined },
          })
          return
        }

        case "file.edited": {
          if (isMember !== true) return
          record({
            node_id: instance,
            started_at: nowIso(),
            kind: "file_edit",
            detail: { file: (props?.file as string) ?? undefined },
          })
          return
        }

        case "permission.replied": {
          if (isMember !== true) return
          record({
            node_id: instance,
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
            node_id: instance,
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
        node_id: instance,
        started_at: nowIso(),
        kind: "human_input",
        detail: { sessionID, text: texts || undefined },
      })
      markHuman(sessionID)
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
