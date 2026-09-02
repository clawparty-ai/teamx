import {
  isOwnerSession,
  myMemberId,
  myMemberIds,
  assignedToMe,
  shouldAutoExecute,
} from "/Users/caishu/github/teamx/opencode-plugin/src/index"

// Helpers to build minimal sync-like data
const ownerData = { teams: [{ team: { my_role: "owner", my_member_id: "owner-1" } }], new_events: [] }
const memberData = { teams: [{ team: { my_role: "contributor", my_member_id: "member-1" } }], new_events: [] }
const noTeamData = { teams: [], new_events: [] }

// A multi-team session: leads team-alpha, works as a regular member in
// team-beta. Events from team-beta carry team_id so team-aware matching must
// not be confused by the first team's member id.
const multiTeamData = {
  teams: [
    { team: { id: "team-alpha", my_role: "owner", my_member_id: "alpha-lead" } },
    { team: { id: "team-beta", my_role: "contributor", my_member_id: "beta-member" } },
  ],
  new_events: [],
}

function broadcast(assignee?: string, type = "decision.broadcast", team_id?: string) {
  const payload: Record<string, unknown> = { message: "do something" }
  if (assignee) payload.assignee_member_id = assignee
  const e: Record<string, unknown> = { seq: 1, type, payload }
  if (team_id) e.team_id = team_id
  return e
}

let fail = 0
const check = (name: string, ok: boolean) => {
  if (!ok) { fail++; console.log("FAIL:", name) }
}

// --- isOwnerSession ---
check("owner 会话判定为 owner", isOwnerSession(ownerData as any))
check("member 会话判定为非 owner", !isOwnerSession(memberData as any))
check("无团队判定非 owner", !isOwnerSession(noTeamData as any))
check("多团队中任一为 lead → owner", isOwnerSession(multiTeamData as any))

// --- myMemberId / myMemberIds ---
check("myMemberId 返回首团队成员 id", myMemberId(memberData as any) === "member-1")
check("无团队 myMemberId undefined", myMemberId(noTeamData as any) === undefined)
check("myMemberIds 收集全部团队 member id",
  JSON.stringify(myMemberIds(multiTeamData as any).sort()) === JSON.stringify(["alpha-lead", "beta-member"].sort()))

// --- assignedToMe ---
check("分派给我 → true", assignedToMe(memberData as any, [broadcast("member-1")] as any))
check("分派给别人 → false", !assignedToMe(memberData as any, [broadcast("other-2")] as any))
check("无 assignee 广播 → false", !assignedToMe(memberData as any, [broadcast()] as any))
check("无团队 → false", !assignedToMe(noTeamData as any, [broadcast("member-1")] as any))
check("goal.shared 分派给我 → true", assignedToMe(memberData as any, [broadcast("member-1", "goal.shared")] as any))
// any publish type with an assignee counts as a directed task (regression: too-narrow type matching)
check("progress.published 分派给我 → true", assignedToMe(memberData as any, [broadcast("member-1", "progress.published")] as any))
check("start 分派给我 → true", assignedToMe(memberData as any, [broadcast("member-1", "goal.state_changed")] as any))
// Multi-team: a beta task assigned to beta-member IS for me, even though
// teams[0].my_member_id == alpha-lead (regression: teams[0] only matching).
check("多团队 beta 任务分派给 beta-member → true",
  assignedToMe(multiTeamData as any, [broadcast("beta-member", "doc.created", "team-beta")] as any))
check("多团队 alpha 任务分派给 beta-member → false",
  !assignedToMe(multiTeamData as any, [broadcast("beta-member", "doc.created", "team-alpha")] as any))

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
// Multi-team: 作为普通成员的角色（beta）收到定向任务 → 执行（即使另一团队是 owner）
check("多团队中普通成员团队(beta)收到任务 → 执行",
  shouldAutoExecute(runOpts(multiTeamData, [{ seq: 2, team_id: "team-beta", type: "doc.created", payload: { assignee_member_id: "beta-member" } }])))
// Multi-team: 作为 owner 的团队（alpha）分派给自己 → 不执行
check("多团队中 owner 团队(alpha)分派给自己 → 不执行",
  !shouldAutoExecute(runOpts(multiTeamData, [{ seq: 2, team_id: "team-alpha", type: "doc.created", payload: { assignee_member_id: "alpha-lead" } }])))

console.log(fail === 0 ? `ALL PASS (${fail} fail)` : `${fail} FAILED`)
process.exit(fail === 0 ? 0 : 1)
