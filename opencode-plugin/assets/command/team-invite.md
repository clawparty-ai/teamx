---
description: teamx 邀请成员（owner 签发 mTLS 客户端证书 + invitation letter）
agent: teamx
---

用户要以 owner 身份邀请成员（网络模式）。参数: $ARGUMENTS（格式: `"<角色>: <描述>"`，可选 `--name-hint <名>` `--server-url <url>`）。调用 teamx_team_invite（role_desc, name_hint, server_url）。

要点：
- `--server-url` 必须是 owner 机器的**局域网 IP**（如 https://192.168.1.5:5781），不能用 127.0.0.1，否则成员连不上。
- 返回单行 invitation letter（`teamx-inv:v1:...`），提示用户通过安全渠道发给对应成员；成员用 `/team import` 导入。
- 每个邀请对应一个成员席位（pending），成员导入后仍需 owner `approve` 才能工作。
- 中文角色 label 会自动派生 role key（形如 `role-<hex>`），ASCII label（如 reviewer）直接作为 key。

先 teamx_sync 确认自己是 owner 且知道 server URL（可用 /team serve status 查）。
