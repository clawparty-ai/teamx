---
description: teamx 查看我的统一收件箱（任务 + 提问 + 未读提醒）
agent: teamx
---

用户要查看统一收件箱（inbox）。调用 teamx_inbox（ack 默认 true，表示已读并推进 nudge 游标）。

展示分组结果：
- tasks：分派给我/我的角色的未完成任务（状态未 done/verified）
- questions：发给我的待回答提问
- nudges：离线/重启期间的未读团队提醒（含 server 游标 last_seq）

如用户想保留未读（稍后再看），传 ack=false。
