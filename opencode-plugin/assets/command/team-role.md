---
description: teamx 选择角色（固定角色或已批准的自定义角色）
agent: teamx
---

用户要为当前会话选择 teamx 角色。角色: $ARGUMENTS。调用 teamx_set_role（role=$ARGUMENTS）。若角色不在目录中，列出可用角色让用户选择（含自定义角色）。注意：未批准的自定义角色不可用，需 owner 先 `role approve`。

角色相关其他命令：
- `/team role propose <key> <label> [desc]`（或 `/team-role-propose`）— 成员提议自定义角色
- `/team role approve <key>`（或 `/team-role-approve`）— owner 审批
- `/team role deny <key>`（或 `/team-role-deny`）— owner 拒绝
- `/team role update <key> [--label] [--description]`（或 `/team-role-update`）— owner 修改角色描述
