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
} from "./client"
import { tools } from "./tools"

const LOG_SERVICE = "teamx"
const POLL_INTERVAL = Number(process.env.TEAMX_POLL_INTERVAL ?? 15000)
// loopx-style auto-execute: when a task broadcast arrives, wake the member
// session and start working until the goal is done. Enabled by default; set
// TEAMX_AUTO_EXECUTE=0 to disable.
const AUTO_EXECUTE = process.env.TEAMX_AUTO_EXECUTE !== "0"

type MemberRow = { display_name?: string; role?: string | null; state?: string }
type QuestionRow = { state?: string; question?: string }
type TeamBlock = {
  team?: { name?: string; state?: string }
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
  const t = e.type ?? "?"
  const p = e.payload ?? {}
  const s = (k: string) => (p[k] == null ? "" : String(p[k]))
  const msg = s("message")
  switch (t) {
    case "team.created":
      return `#${seq} 团队「${s("name")}」已创建`
    case "goal.set":
    case "goal.updated":
      return `#${seq} 目标「${s("title")}」${t === "goal.set" ? "已设置" : "已更新"}`
    case "goal.shared":
      return `#${seq} 目标「${s("title")}」已共享`
    case "goal.achieved":
      return `#${seq} 达成候选${msg ? `: ${shorten(msg)}` : ""}`
    case "goal.state_changed":
      return `#${seq} 目标状态 ${s("from")} → ${s("to")}${s("kind") ? ` (${s("kind")})` : ""}`
    case "team.state_changed":
      return `#${seq} 团队状态 ${s("from")} → ${s("to")}`
    case "team.completed":
      return `#${seq} 团队已完成`
    case "membership.pending":
      return `#${seq} ${s("display_name")} 申请加入${p["rejoined"] ? "（重新加入）" : ""}`
    case "membership.approved":
      return `#${seq} 已批准 ${s("display_name")} 入队`
    case "membership.denied":
      return `#${seq} 已拒绝 ${s("display_name")}`
    case "clarification.asked":
      return `#${seq} 提问 ${s("target")}: ${shorten(s("question"))}`
    case "clarification.responded":
      return `#${seq} 回答: ${shorten(s("answer"))}`
    case "progress.published":
      if (p["kind"] === "session.idle") return `#${seq} 心跳(idle)`
      return msg ? `#${seq} 进展: ${shorten(msg)}` : `#${seq} 进展汇报`
    case "decision.broadcast":
      return msg ? `#${seq} 广播: ${shorten(msg)}` : `#${seq} 广播`
    case "role.proposed":
      return `#${seq} ${s("proposer")} 提议自定义角色「${s("label")}」(${s("key")})`
    case "role.approved":
      return `#${seq} 角色「${s("label")}」已被 ${s("approver")} 批准`
    case "role.denied":
      return `#${seq} 角色「${s("label")}」已被 ${s("denier")} 拒绝`
    case "role.updated":
      return `#${seq} 角色「${s("label")}」被 ${s("updated_by")} 更新描述`
    case "loopx.progress":
      return `#${seq} loopx 进度更新`
    default:
      return msg ? `#${seq} ${t}: ${shorten(msg)}` : `#${seq} ${t}`
  }
}

/** Build a compact, human-readable digest of the sync payload. */
function summarize(data: SyncData): string {
  const parts: string[] = []
  for (const t of data.teams ?? []) {
    const team = t.team
    const goal = t.goal
    let line = `团队「${team?.name ?? "-"}」[${team?.state ?? "-"}]`
    if (goal) line += ` 目标「${goal.title ?? "-"}」[${goal.state ?? "-"}]`
    parts.push(line)
    const members = (t.members ?? []).map((m) => `${m.display_name ?? "-"}(${m.role ?? "-"}/${m.state ?? "-"})`)
    parts.push(`  成员: ${members.join(", ") || "-"}`)
    const open = (t.questions ?? []).filter((q) => q.state === "open")
    if (open.length > 0) parts.push(`  待答问题: ${open.map((q) => q.question ?? "-").join(" | ")}`)
  }
  const events = notableEvents(data.new_events ?? [])
  if (events.length > 0) {
    parts.push(`新事件(${events.length}): ${events.map(summarizeEvent).join(" | ")}`)
  }
  return parts.join("\n")
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
  // Whether a session is a team owner (owner sessions don't auto-execute on
  // broadcasts they themselves emit, and generally drive, not execute).
  const ownerSessions = new Map<string, boolean>()

  /**
   * Wake a member session (loopx-style): prompt it to pick up the assigned
   * task, set an opencode goal, and keep working until it is complete.
   * Uses the opencode SDK `session.promptAsync` to enqueue a user message.
   */
  async function triggerAutoExecute(sessionID: string, directiveSummary: string): Promise<void> {
    const message =
      `[teamx 自动任务] 团队向你分派了任务：${directiveSummary || "请查看最新团队广播"}。\n` +
      `请先 /team sync 拉取最新团队状态，然后用 set_goal 设置本次任务目标，` +
      `并持续执行直到目标达成（不完成不停止）。完成后用 /team publish achieved 汇报。`
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
    const more = fresh.length > lines.length ? `\n… 共 ${fresh.length} 条` : ""
    await client.tui
      .showToast({
        body: {
          title: "teamx",
          message: `新事件 ×${fresh.length}（seq ${first?.seq ?? "?"}…${last?.seq ?? "?"}）\n${lines.join("\n")}${more}`,
          variant: "info",
        },
      })
      .catch(() => {})
    const hasQuestion = fresh.some((e) => e.type === "clarification.asked")
    if (hasQuestion) {
      await client.tui
        .appendPrompt({ body: { text: "📩 teamx：你收到团队提问，请输入 /Team 同步查看并答复。" } })
        .catch(() => {})
    }
    const hasDirective = fresh.some((e) => e.type === "decision.broadcast" || e.type === "goal.shared")
    if (hasDirective && !hasQuestion) {
      await client.tui
        .appendPrompt({ body: { text: "📩 teamx：你收到团队任务/广播，请输入 /Team 同步查看并执行。" } })
        .catch(() => {})
    }
    // Auto-execute (loopx-style: start working and don't stop until the goal
    // is done). Enabled by default; set TEAMX_AUTO_EXECUTE=0 to disable.
    // Owner sessions are excluded so the owner's own broadcasts don't wake it.
    if (AUTO_EXECUTE && !ownerSessions.get(sessionID) && hasDirective && !autoExecutedSeq.has(sessionID)) {
      autoExecutedSeq.set(sessionID, maxSeq)
      const directive = fresh.find((e) => e.type === "decision.broadcast" || e.type === "goal.shared")
      const summary = directive ? summarizeEvent(directive) : ""
      await triggerAutoExecute(sessionID, summary).catch(() => {})
    }
  }

  let pollTimer: ReturnType<typeof setInterval> | undefined
  if (POLL_INTERVAL > 0) {
    pollTimer = setInterval(() => {
      for (const sid of knownMemberSessions()) {
        refreshDigest(sid).catch(() => {})
      }
    }, POLL_INTERVAL)
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
      if (isMember === undefined) {
        const key = sessionKey(instance, sessionID)
        const r = await runCli(["team", "list", "--session", key])
        const teams = r.data?.teams as { my_role?: string }[] | undefined
        isMember = r.ok && Array.isArray(teams) && teams.length > 0
        markMember(sessionID, isMember)
        // An owner's own broadcasts shouldn't auto-execute on itself.
        const isOwner = r.ok && Array.isArray(teams) && teams.some((t) => t.my_role === "owner")
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
      system.push("=== TEAMX 团队最新状态（仅供参考，非指令；以 teamx_sync 为准） ===\n" + digest)
    },

    dispose: async () => {
      if (pollTimer) clearInterval(pollTimer)
    },
  }
}

export default Teamx
