# 缺陷：全局 `TEAMX_SERVER_URL` 覆盖 workspace letter，导致 teamx_sync 失败

- 状态：opened
- 所有者：owner
- 创建者：caishu（owner）
- 创建日期：2026-09-17
- 优先级：高

## 描述

在 `teamx-enterprise` workspace 中，`teamx_sync` / `teamx_list_teams` 持续失败：

```
teamx error: TimeoutError: The operation timed out.        (teamx_sync)
teamx error: Error: certificate signature failure          (teamx_list_teams)
```

根因是**环境变量覆盖**：`~/.zshrc` 全局 `export TEAMX_SERVER_URL=https://95.40.221.159`
（属于另一个团队 powernano 的 server）。插件的 server 解析顺序为
`TEAMX_SERVER_URL > workspace .teamx/letters 自动发现`，于是本 workspace 的请求被指向
`95.40.221.159`，却携带本 workspace 的 teamx-ent 客户端证书 → 服务端拒绝（证书签名不匹配）。

## 复现步骤

1. 在 shell 中 `export TEAMX_SERVER_URL=https://95.40.221.159`。
2. 进入 `teamx-enterprise`（其 `.teamx/letters/*/letter.json` 的 server 为 `https://120.27.222.33:5783`）。
3. 调用 `teamx_sync` / `teamx_list_teams`。

## 预期行为

插件从本 workspace 的 letter 自动发现 server（`https://120.27.222.33:5783`）并正常同步。

## 实际行为

请求发往 `95.40.221.159`，用错证书，握手被拒 / 超时。

## 影响范围

- 所有 workspace 共用一个全局 `TEAMX_SERVER_URL` 的会话；本机受影响 workspace：`teamx-enterprise`（teamx-ent 团队）。
- `powernano-v2` workspace 自带 letter，无需该环境变量，未受影响。

## 修复

- 已从 `~/.zshrc` 移除全局 `export TEAMX_SERVER_URL=...`（改为依赖 letter 自动发现；仅多 server 需覆盖时在对应目录临时 export）。
- 验证：用 workspace letter 直连 `https://120.27.222.33:5783/rpc`，`sync` 与 `team.list` 均返回正常数据。
- 注意：已启动的 opencode 进程仍持有旧环境变量，需**重启 opencode** 后插件才生效。

## 变更记录

| 日期 | 变更 | 作者 |
|------|------|------|
| 2026-09-17 | 创建（opened）+ 记录修复与验证 | caishu |
