---
description: teamx 加入团队（凭 invite_token，需 owner 审批）
agent: teamx
---

用户要加入 teamx 团队。参数: $ARGUMENTS（应为 invite_token 与 --name 显示名）。

执行步骤：
1. 解析 invite_token（和可选的 --name 显示名；未给则询问）
2. 调用 teamx_join（token, name）
3. 告知用户已提交 pending 申请，需 owner 审批；审批后可用 /team role set 选择角色
