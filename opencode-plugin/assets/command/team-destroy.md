---
description: teamx 软销毁团队（owner，隐藏并保留数据）
agent: teamx
---

用户要销毁 teamx 团队。先 teamx_sync + teamx_status 确认自己是 owner，再调用 teamx_team_destroy。销毁是**软删除**：团队标记 destroyed、从所有成员列表隐藏、吊销未使用的邀请函，但数据（members/events/goals/roles）保留可审计。这是不可逆操作，执行前向用户确认。额外参数: $ARGUMENTS
