---
description: teamx 审批自定义角色（owner，自动授予提议者）
agent: teamx
---

用户（owner）要审批一个自定义角色。参数: $ARGUMENTS（角色 key）。调用 teamx_role_approve（role=$ARGUMENTS）。审批后该角色进入目录并自动授予提议者。若角色不存在或非 proposed 状态，报错提示。
