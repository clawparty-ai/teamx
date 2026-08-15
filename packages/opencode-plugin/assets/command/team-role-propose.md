---
description: teamx 提议自定义角色（成员自建，owner 审批）
agent: teamx
---

用户要提议一个自定义角色。参数: $ARGUMENTS（格式: <key> <label> [description]）。调用 teamx_role_propose（role=key, label=label, description=description）。key 不能与固定角色（owner/observer/supervisor/contributor/subtask-implementer/reviewer）冲突，也不可重复。提议后提示 owner 用 `/team role approve <key>` 审批。
