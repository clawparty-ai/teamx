---
description: teamx 团队协作（子命令：create/join/status/sync/goal/approve/deny/role/ask/publish 等；输入 /team help 查看全部）
agent: teamx
---

teamx 团队协作命令。请按以下子命令路由执行（agent 收到 $ARGUMENTS 后解析并调用对应 teamx_* 工具）：

- `create <name>` → teamx_create_team（创建团队，成为 owner）
- `join <token> --name <显示名>` → teamx_join（加入团队，需 owner 审批）
- `status` → teamx_status（查看当前团队完整状态）
- `sync` → teamx_sync（拉取最新团队事件）
- `goal set <title> [body]` → teamx_set_goal（owner 起草目标）
- `goal share` → teamx_share_goal（owner 共享目标，启动执行）
- `goal close` → teamx_close_goal（owner 验证并关闭目标）
- `approve <member_id>` → teamx_approve（owner 审批入队）
- `deny <member_id>` → teamx_deny（owner 拒绝入队）
- `role set <role>` → teamx_set_role（选择角色：固定角色或已批准的自定义角色）
- `role propose <key> <label> [desc]` → teamx_role_propose（成员提议自定义角色，owner 审批）
- `role approve <key>` / `role deny <key>` → teamx_role_approve / teamx_role_deny（owner 审批/拒绝自定义角色）
- `role update <key> [--label] [--description]` → teamx_role_update（owner 修改角色名/描述）
- `state idle|active` → teamx_set_state（设置工作状态）
- `ask <member_id> <问题>` → teamx_ask（owner 提问）
- `respond <ask_id> <回答>` → teamx_respond（回答提问）
- `publish <type> [data]` → teamx_publish（progress/decision/update/blocked/resumed/achieved/refine）
- `archive` → teamx_archive（owner 归档已完成团队）
- `serve start [--port]` / `serve status` / `serve stop` → teamx_serve_start / teamx_serve_status / teamx_serve_stop（在 opencode 内启动/查询/停止本地 serve，网络模式）
- `serve token <member>` → teamx_serve_token（生成/轮换成员连接 token，owner）
- `help` → 列出以上子命令

先调用 teamx_sync，然后根据 $ARGUMENTS 中的子命令路由到对应工具并执行。

如果 $ARGUMENTS 为空（没有子命令），默认执行 teamx_status 查看团队完整状态。

**重要：入队审批必须由 owner 决策，绝不自动批准。** 在 status/sync 输出中发现有成员申请加入（membership.pending 或成员 state 为 pending）时，只列出待审批成员并提示 owner 可以执行 `approve <member_id>` 或 `deny <member_id>`，不要擅自调用 teamx_approve / teamx_deny。只有用户明确要求批准某成员时，才执行 teamx_approve。

用户输入: $ARGUMENTS
