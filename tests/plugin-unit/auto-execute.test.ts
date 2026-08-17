import { isOwnerSession, myMemberId, assignedToMe, shouldAutoExecute } from "/Users/caishu/github/teamx/opencode-plugin/src/index"

// Helpers to build minimal sync-like data
const ownerData = { teams: [{ team: { my_role: "owner", my_member_id: "owner-1" } }], new_events: [] }
const memberData = { teams: [{ team: { my_role: "contributor", my_member_id: "member-1" } }], new_events: [] }
const noTeamData = { teams: [], new_events: [] }

function broadcast(assignee?: string, type = "decision.broadcast") {
  const payload: Record<string, unknown> = { message: "do something" }
  if (assignee) payload.assignee_member_id = assignee
  return { seq: 1, type, payload }
}

let fail = 0
const check = (name: string, ok: boolean) => {
  if (!ok) { fail++; console.log("FAIL:", name) }
}

// --- isOwnerSession ---
check("owner 会话判定为 owner", isOwnerSession(ownerData as any))
check("member 会话判定为非 owner", !isOwnerSession(memberData as any))
check("无团队判定非 owner", !isOwnerSession(noTeamData as any))

// --- myMemberId ---
check("myMemberId 返回当前成员 id", myMemberId(memberData as any) === "member-1")
check("无团队 myMemberId undefined", myMemberId(noTeamData as any) === undefined)

// --- assignedToMe ---
check("分派给我 → true", assignedToMe([broadcast("member-1")] as any, "member-1"))
check("分派给别人 → false", !assignedToMe([broadcast("other-2")] as any, "member-1"))
check("无 assignee 广播 → false", !assignedToMe([broadcast()] as any, "member-1"))
check("无 myId → false", !assignedToMe([broadcast("member-1")] as any, undefined))
check("goal.shared 分派给我 → true", assignedToMe([broadcast("member-1", "goal.shared")] as any, "member-1"))
// any publish type with an assignee counts as a directed task (regression: too-narrow type matching)
check("progress.published 分派给我 → true", assignedToMe([broadcast("member-1", "progress.published")] as any, "member-1"))
check("start 分派给我 → true", assignedToMe([broadcast("member-1", "goal.state_changed")] as any, "member-1"))

// --- shouldAutoExecute ---
const runOpts = (data: any, events: any[], lastExecutedSeq = 0) => ({ data, events, lastExecutedSeq })

// owner 永不执行（即使分派给 owner）
check("owner 分派给自己也不执行", !shouldAutoExecute(runOpts(ownerData, [broadcast("owner-1")])))
// member 收到分派给我的 → 执行
check("member 收到分派给我的 → 执行", shouldAutoExecute(runOpts(memberData, [broadcast("member-1")])))
// member 收到别人的任务 → 不执行
check("member 收到别人的任务 → 不执行", !shouldAutoExecute(runOpts(memberData, [broadcast("other-2")])))
// member 收到无 assignee 广播 → 不执行（纯通知）
check("member 收到无 assignee 广播 → 不执行", !shouldAutoExecute(runOpts(memberData, [broadcast()])))
// 已执行过 seq=1 → 同一 seq 不再执行（水位去重，不是布尔"曾执行过"）
check("已执行过 seq=1 → 同 seq 不重复", !shouldAutoExecute(runOpts(memberData, [broadcast("member-1")], 1)))
// 更新的定向任务（seq=5）→ 再次执行（回归：auto-execute 只触发一次）
const newTask = { seq: 5, type: "progress.published", payload: { assignee_member_id: "member-1" } }
check("新的定向任务(seq=5) → 再次执行", shouldAutoExecute(runOpts(memberData, [newTask], 1)))

console.log(fail === 0 ? `ALL PASS (${fail} fail)` : `${fail} FAILED`)
process.exit(fail === 0 ? 0 : 1)
