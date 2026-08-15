---
description: teamx 创建团队（成为 owner）
agent: teamx
---

用户要通过 teamx 创建一个团队。团队名称: $ARGUMENTS。

执行步骤：
1. 调用 teamx_create_team（name 为 $ARGUMENTS）
2. 把返回的 invite_token 展示给用户，说明需要分享给成员
3. 询问用户是否需要立即 teamx_set_goal 起草团队目标
