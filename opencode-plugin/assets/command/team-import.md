---
description: teamx 导入邀请函（成员解包 mTLS 证书 + 认领 pending 席位）
agent: teamx
---

用户要导入 invitation letter。参数: $ARGUMENTS（letter 单行 `teamx-inv:v1:...` 或 .json 文件路径，可选 `--name <显示名>`）。调用 teamx_team_import（letter, name）。

要点：
- 成功后：mTLS 证书/私钥存到 `~/.teamx/letters/<invitation_id>/`，席位变为 pending，等待 owner approve。
- 单机共享 DB 时一步到位（存证书 + 认领）；跨机时本地只落盘，需设 `TEAMX_SERVER_URL` 后由插件在服务器上完成认领。
- 提示：若要实时推送，设置 `TEAMX_SERVER_URL=https://<owner局域网IP>:5781` 并重启 opencode。
