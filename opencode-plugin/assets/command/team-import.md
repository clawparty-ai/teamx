---
description: teamx 导入邀请函（成员解包 mTLS 证书 + 认领 pending 席位）
agent: teamx
---

用户要导入 invitation letter。参数: $ARGUMENTS（letter 单行 `teamx-inv:v1:...` 或 .json 文件路径，可选 `--name <显示名>`）。调用 teamx_team_import（letter, name）。

要点：
- 成功后：mTLS 证书/私钥存到 `~/.teamx/letters/<invitation_id>/`，席位变为 pending，等待 owner approve。
- 单机共享 DB 时一步到位（存证书 + 认领）；跨机时本地只落盘，需连接服务器完成认领。
- **自动连接**：letter 内含 server URL，插件启动时会自动发现并进入网络模式（无需手动设置 `TEAMX_SERVER_URL`）；首次 RPC 会自动 `team.import` 认领席位并重试。
- 提示：如需实时推送，确保 opencode 在导入 letter 后重启一次，插件即自动连上 `https://<owner局域网IP>:5781`。
