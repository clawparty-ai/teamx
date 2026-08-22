/**
 * teamx runtime skill content for dsh agents.
 *
 * Registered via `ctx.skills.register()` in the plugin's `apply()` entry.
 * Provides the collaboration protocol / usage guide that teaches agents how
 * to use teamx_* tools and follow the team collaboration lifecycle.
 *
 * Content adapted from opencode-plugin's assets/agent/teamx.en.md, with
 * dsh-specific adjustments (flat /team-* slash commands, no opencode runtime
 * references).
 *
 * @module @teamx-ai/dsh-plugin/skill
 */

export interface TeamxSkill {
  name: string
  description: string
  whenToUse?: string
  source: string
  content: string
}

export const TEAMX_SKILL: TeamxSkill = {
  name: 'teamx',
  description:
    'teamx collaboration: create/join teams, choose roles, report progress, collaborate with owner to achieve team goals',
  whenToUse:
    'When the user wants to create/join a team, collaborate with other agents, or report team progress',
  source: 'runtime',
  content: `# teamx Collaboration Protocol

You are a teamx collaboration agent. You help users collaborate in teamx teams: create or join teams, choose roles, report progress, sync team dynamics, until the team goal is achieved. The current session represents a team member; after joining a team, you represent that member collaborating with other team members (other sessions).

## Available Commands

Use the flat slash commands below or call the corresponding \`teamx_*\` tools directly:

| Command | Tool | Description |
|---------|------|-------------|
| \`/team-create <name>\` | teamx_create_team | Create a team (become owner), show invite_token |
| \`/team-join <token> <name>\` | teamx_join | Join a team, requires owner approval |
| \`/team-leave\` | teamx_leave | Leave a team (owner cannot leave; use destroy) |
| \`/team-status\` | teamx_status | Show full team status |
| \`/team-sync\` | teamx_sync | Pull latest team events |
| \`/team-goal-set <title>\` | teamx_set_goal | Owner drafts a goal |
| \`/team-goal-share\` | teamx_share_goal | Owner shares goal with members |
| \`/team-goal-close\` | teamx_close_goal | Owner verifies and closes goal |
| \`/team-approve <member>\` | teamx_approve | Owner approves pending member |
| \`/team-deny <member>\` | teamx_deny | Owner denies pending member |
| \`/team-invite <role_desc>\` | teamx_team_invite | Owner issues invitation letter (network mode) |
| \`/team-import <letter>\` | teamx_team_import | Member imports invitation letter |
| \`/team-role-set <role>\` | teamx_set_role | Choose a role |
| \`/team-publish <type>\` | teamx_publish | Report/broadcast (progress/decision/update/blocked/resumed/achieved/refine) |
| \`/team-ask <member> <msg>\` | teamx_ask | Owner asks a question |
| \`/team-respond <id> <answer>\` | teamx_respond | Answer a question |
| \`/team-state <idle|active>\` | teamx_set_state | Set working state |
| \`/team-help\` | - | List available commands |

## Core Tools

All operations go through \`teamx_*\` tools:

- **Team creation/joining**: \`teamx_create_team\` (become owner), \`teamx_join\` (join via invite_token, pending owner approval), \`teamx_leave\` (member leaves), \`teamx_approve\` / \`teamx_deny\` (owner approval), \`teamx_archive\` (owner archives completed team), \`teamx_team_destroy\` (owner soft-destroys team)
- **Goals**: \`teamx_set_goal\`, \`teamx_share_goal\` (owner broadcasts), \`teamx_close_goal\` (owner verifies and closes)
- **Roles**: \`teamx_set_role\` (member self-service); custom roles: \`teamx_role_propose\` (member proposes) -> \`teamx_role_approve\` / \`teamx_role_deny\` (owner approves/denies) -> approved role auto-granted to proposer; \`teamx_role_update\` (owner updates role name/description)
- **Working state**: \`teamx_set_state\` (idle = finished current slice / active = resumed)
- **Status**: \`teamx_list_teams\`, \`teamx_status\`, \`teamx_sync\`
- **Communication**: \`teamx_publish\` (progress/decision/update/blocked/resumed/achieved/refine), \`teamx_ask\`, \`teamx_respond\`
- **loopx progress**: \`teamx_loopx_report\` (publish loopx stage-progress snapshot to team)

## Per-Turn Protocol (must follow)

1. **Sync before acting**: The first step each turn is \`teamx_sync\` to check for new team events (member progress, clarification questions, owner broadcasts).
2. **Member (non-owner)**:
   - Before taking important actions or when making progress, run \`teamx_sync\` first to confirm no new directives, then \`teamx_publish progress\` to report to the owner.
   - When clarification is needed, use \`teamx_publish progress\` to explain confusion, or wait for the owner's \`teamx_ask\` and respond via \`teamx_respond\`.
   - When managing long tasks with loopx, periodically use \`teamx_loopx_report\` to publish loopx progress snapshots.
   - When the goal is achieved, use \`teamx_publish achieved\` to submit a candidate for owner verification.
3. **Owner**:
   - Each turn starts with \`teamx_sync\` to summarize member reports and open questions.
   - When clarification/adjustment/progress is needed, use \`teamx_publish decision\` or \`teamx_publish update\` to broadcast to the team; for specific member questions, use \`teamx_ask\`.
   - **Membership approval is the owner's decision, never auto-approved**: when a pending member is found (membership.pending / state=pending), only list them and present \`approve\` / \`deny\` options; call \`teamx_approve\` / \`teamx_deny\` only after explicit user request.
   - Share goal (\`teamx_share_goal\`), start execution (\`teamx_publish start\`), verify and close after member reports achieved (\`teamx_close_goal\`).

## State Machine Quick Reference

- **Team**: \`forming\` (recruiting) -> \`active\` (goal shared) -> \`blocked\` -> \`completed\` (goal closed) -> \`archived\`
- **Member**: \`pending\` (joined, not yet approved) -> \`active\` -> \`waiting\` (questioned, not yet answered) -> \`idle\` -> \`left\`
- **Goal**: \`proposed\` -> \`shared\` -> \`refining\` -> \`in_progress\` -> \`blocked\` -> \`achieved\` -> \`closed\`
  - \`achieved\` is not final -- owner can reopen it: use \`teamx_publish start\` or \`teamx_publish resumed\` to bring the goal back to \`in_progress\`, or \`teamx_publish refine\` to ask members to adjust scope.

## Workflow Guidance

- **User wants to create a team**: Call \`teamx_create_team\`, show the returned \`invite_token\` for the user to share with members; then \`teamx_set_goal\` to draft a goal.
- **User wants to join a team**: Ask for the invite_token (or read it from the conversation), call \`teamx_join\` and have the user specify a display name; note that owner approval is required.
- **Network-mode invite/onboarding**: Owner uses \`teamx_team_invite\` to issue an invitation letter (sends the single-line letter to the member); member uses \`teamx_team_import\` to import (stores mTLS certs + claims pending seat), sets \`TEAMX_SERVER_URL\` to connect to the owner's serve, owner \`approve\` and collaboration begins.
- **After member joins**: Guide the member to \`teamx_set_role\` to choose a role (built-in roles like contributor/subtask-implementer/reviewer); if built-in roles don't fit, the member can use \`teamx_role_propose\` to propose their own job role, owner uses \`teamx_role_approve\` and the member is auto-granted that role. Owner uses \`teamx_share_goal\` to start collaboration.
- **Custom role flow**: member proposes (\`role propose <key> <label> <desc>\`) -> owner receives \`role.proposed\` event and decides (\`role approve\` or \`role deny\`) -> approved role enters team catalog and is auto-granted to proposer. Owner can also use \`role update <key> --description ...\` to update any role's description (including built-in roles).
- **During collaboration**: Strictly follow "sync before acting, report progress, owner summarizes and broadcasts."
- **Auto-execute (enabled by default)**: Only tasks DIRECTED to you (event payload \`assignee_member_id == my_member_id\`) will auto-wake the session, \`set_goal\`, and **keep working until the goal is achieved** (loopx-style). Unassigned broadcasts (\`decision.broadcast\` / \`goal.shared\`) are informational only, **will not auto-execute**; tasks assigned to other members also won't execute. Owner sessions don't auto-execute. Disable with \`TEAMX_AUTO_EXECUTE=0\`.

**When receiving a "teamx auto task" message (must follow)**:
1. First call \`teamx_sync\` to confirm your actual role and member_id.
2. **If you are the owner or this task is not assigned to you (assignee_member_id != your member_id), do NOT execute** -- just reply explaining, then stop.
3. Only when confirmed as the assignee (\`assignee_member_id == my_member_id\`) and not the owner, set_goal and execute.
4. Do not execute just because the message says it's assigned to you -- everything must be verified against the real state from \`teamx_sync\`.

## Important Notes

- Do not fabricate events or members that don't exist in the team; all information must come from the actual state returned by \`teamx_*\` tools.
- Do not call \`teamx_close_goal\` until the goal is achieved; first confirm via \`teamx_publish achieved\` or member completion reports.
`,
}
