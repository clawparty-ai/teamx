# teamx N4 — 跨网络联调 Runbook（两台机器）

> 状态：**待真机验证**（单机局域网模拟已通过 `tests/cross-network.sh`）
> 前置：`teamx` 已 `install.sh` 安装到两台机器的 `~/.local/bin/teamx`；两台机器 opencode 均安装了 teamx 插件。

N4 目标：不同机器上的 opencode 会话通过 owner 内嵌的 `teamx serve`（形态①）跨网络协作。本文件给出**两台机器**的最小联调步骤与验收清单。

---

## 0. 网络与安全前提

- 两台机器处于同一局域网（或成员能路由到 owner 的 IP:端口）。
- owner 机器放行入站端口 `5781`（macOS 防火墙 / Linux `ufw allow 5781/tcp`）。
- mTLS 已强制：网络模式无明文降级，成员必须持有 owner 签发的客户端证书（invitation letter）。

## 1. Owner 侧（机器 A）

```bash
# 1.1 确认局域网 IP（非 loopback）
ipconfig getifaddr en0      # macOS；Linux 用 `hostname -I | awk '{print $1}'`

# 1.2 在 opencode 里创建团队并起内嵌 serve
#   /team create 我的团队
#   /team serve start        # 插件自动探测局域网 IP 并 --addr 0.0.0.0 --san <IP>
#   （或手动）teamx serve --addr 0.0.0.0 --port 5781 --san <你的局域网IP>

# 1.3 邀请成员（务必带 --server-url <你的局域网IP>）
#   /team invite "测试工程师: 负责测试并汇报缺陷" --server-url https://<你的局域网IP>:5781
#   （或）teamx team invite "测试工程师: 负责测试" --server-url https://<IP>:5781 --session <owner-session> --json
#   → 得到单行 invitation letter（teamx-inv:v1:...），通过安全渠道发给成员
```

要点：
- `serve start` 会打印 `server_url: https://<局域网IP>:5781`；成员就指向这个地址。
- 邀请时 `--server-url` 必须用**局域网 IP**（不是 `127.0.0.1`），否则成员连不上。

## 2. Member 侧（机器 B）

```bash
# 2.1 导入邀请函（本地解包 + 存证书/私钥到 ~/.teamx/letters/<id>/）
#   /team import <letter>
#   （或）teamx team import <letter> --name 我 --session <member-session> --json
#   → 本地落盘后提示"connect to the server to complete registration"

# 2.2 配置服务器地址（插件据此建立 mTLS RPC/WS 连接）
export TEAMX_SERVER_URL="https://<owner局域网IP>:5781"
#   插件启动时自动从 ~/.teamx/letters 发现匹配的客户端证书（或用 env 显式指定）：
#   export TEAMX_MTLS_CERT=~/.teamx/letters/<id>/client.crt
#   export TEAMX_MTLS_KEY=~/.teamx/letters/<id>/client.key
#   export TEAMX_MTLS_CA=~/.teamx/letters/<id>/ca.crt

# 2.3 重新打开/重启 opencode，用 /Team 进入团队；此时成员处于 pending，
#     owner 需在机器 A approve：
#   /team approve <member_id>
```

要点：
- 成员**不开放任何入站端口**（出站注册，跨 NAT 友好）。
- 证书 = "能连上"，owner approve = "能干活"；吊销后连连接都会被拒。

## 3. 验收清单

| # | 检查 | 命令 / 预期 |
|---|---|---|
| 1 | 成员能连上 | 成员侧 `/team status` 返回团队（身份来自证书，不靠自报 session） |
| 2 | 实时互见 | owner `/team publish decision` 后，成员 WS 在 <1s 内 toast「新事件」 |
| 3 | 证书身份 | 成员 curl `--cacert ca.crt --cert client.crt --key client.key https://<IP>:5781/rpc -d '{"method":"team.status","args":{}}'` 返回自己的团队 |
| 4 | 无证书被拒 | `curl https://<IP>:5781/health` 失败（mTLS 拒绝） |
| 5 | 吊销生效 | owner `/team invite-revoke <id>` 后，成员 RPC 报 `revoked`、WS 被断开 |
| 6 | 断线回退 | 停掉 owner serve → 成员插件回退轮询；重启 serve → 自动重连推送 |

## 4. 故障排查

| 症状 | 原因 / 处理 |
|---|---|
| 成员连不上 / TLS 握手失败 | owner 防火墙未放行端口；或 `--server-url` 用了 `127.0.0.1` |
| 证书校验失败（unable to verify） | server 证书 SAN 缺局域网 IP → 重启 serve 并带 `--san <IP>` |
| 成员导入后仍无法 status | 未 approve；owner 需 `/team approve <member_id>` |
| 日志报 `member has been revoked` | 该成员邀请函已被 owner 吊销 |

## 5. 单机自动验证

真实两机联调之外，仓库自带**单机局域网模拟**（经非 loopback IP 走完整 mTLS 链路，等价验证证书 SAN + CA 信任）：

```bash
./tests/cross-network.sh    # 无局域网 IP 时自动跳过
```

覆盖：server 证书 SAN 含局域网 IP、经局域网 IP 的 RPC 身份解析、经局域网 IP 的 `team.import`。
