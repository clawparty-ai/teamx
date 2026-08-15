---
description: teamx 验证并关闭目标（owner，仅当已 achieved）
agent: teamx
---

用户要关闭 teamx 团队目标。先 teamx_sync + teamx_status 确认目标已达 achieved 状态（成员已 publish achieved），再调用 teamx_close_goal。若未达成，说明需先完成并 publish achieved。额外参数: $ARGUMENTS
