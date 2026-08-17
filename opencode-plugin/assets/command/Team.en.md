---
description: teamx team collaboration (subcommands: create/join/status/sync/goal/approve/deny/role/ask/publish/invite/import etc.; type /team help to see all)
agent: teamx
---

teamx team collaboration command. Route and execute based on the following subcommands (agent parses $ARGUMENTS and calls the corresponding teamx_* tool):

- `create <name>` → teamx_create_team (create a team, become owner)
- `join <token> --name <display_name>` → teamx_join (join a team, requires owner approval)
- `status` → teamx_status (view full current team status)
- `sync` → teamx_sync (pull latest team events)
- `goal set <title> [body]` → teamx_set_goal (owner drafts goal)
- `goal share` → teamx_share_goal (owner shares goal, start execution)
- `goal close` → teamx_close_goal (owner verifies and closes goal)
- `approve <member_id>` → teamx_approve (owner approves membership)
- `deny <member_id>` → teamx_deny (owner denies membership)
- `invite "<role>: <description>" [--server-url <url>]` → teamx_team_invite (owner issues invitation letter + mTLS client certificate, network mode)
- `import <letter> [--name <name>]` → teamx_team_import (member imports invitation letter, claim pending seat)
- `invite-list` → teamx_team_invite_list (owner lists issued invitations)
- `invite-revoke <invitation_id>` → teamx_team_invite_revoke (owner revokes invitation)
- `role set <role>` → teamx_set_role (choose role: built-in or approved custom role)
- `role propose <key> <label> [desc]` → teamx_role_propose (member proposes custom role, owner approves)
- `role approve <key>` / `role deny <key>` → teamx_role_approve / teamx_role_deny (owner approves/denies custom role)
- `role update <key> [--label] [--description]` → teamx_role_update (owner modifies role name/description)
- `state idle|active` → teamx_set_state (set working state)
- `ask <member_id> <question>` → teamx_ask (owner asks question)
- `respond <ask_id> <answer>` → teamx_respond (answer a question)
- `publish <type> [data]` → teamx_publish (progress/decision/update/blocked/resumed/achieved/refine)
- `archive` → teamx_archive (owner archives completed team)
- `destroy` → teamx_team_destroy (owner soft-destroys a team: hides it, keeps data, irreversible)
- `serve start [--port]` / `serve status` / `serve stop` → teamx_serve_start / teamx_serve_status / teamx_serve_stop (start/query/stop local serve within opencode, network mode)
- `serve token <member>` → teamx_serve_token (generate/rotate member connection token, owner)
- `help` → list the above subcommands

Call teamx_sync first, then route to the corresponding tool based on the subcommand in $ARGUMENTS and execute.

If $ARGUMENTS is empty (no subcommand), default to executing teamx_status to view full team status.

**Important: Membership approval must be decided by the owner, never auto-approved.** When status/sync output shows pending members (membership.pending or member state is pending), only list the pending members and prompt the owner that they can execute `approve <member_id>` or `deny <member_id>`. Do not call teamx_approve / teamx_deny on your own. Only call teamx_approve when the user explicitly requests to approve a specific member.

User input: $ARGUMENTS
