---
description: teamx 吊销邀请函（owner）
agent: teamx
---

用户（owner）要吊销某邀请函。参数: $ARGUMENTS（invitation id）。调用 teamx_team_invite_revoke（id）。

要点：
- 吊销后，该邀请函签发的客户端证书在 connect 时即被拒绝；已在线成员会被主动断开 WS 连接。
- 建议先 teamx_team_invite_list 确认要吊销的 invitation id。
