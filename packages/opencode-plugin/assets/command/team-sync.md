---
description: teamx 同步团队最新事件
agent: teamx
---

用户要同步 teamx 团队动态。调用 teamx_sync 拉取最新事件，并总结给用户：成员进展、owner 广播、待答复的澄清问题。如果有 clarification.asked 事件，提示用户用 teamx_respond 答复。额外参数: $ARGUMENTS
