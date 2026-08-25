# teamx 网络模式手动测试：owner + 测试 + 评审 三人并行

> 场景：**一个 team lead** 创建团队、内嵌启动 `teamx serve`，**发两个 invitation letter**（一个给测试、一个给代码评审）；两个成员各自导入邀请函、经 **mTLS + WebSocket** 连到 owner 的 serve，**审批后同时开始工作**，owner 实时看到两人的进展。
>
> 对比 `docs/13-demo-team.md`（V1 单机 token 入队），本文走的是**网络模式**（形态① owner 内嵌 serve）：身份来自 mTLS 客户端证书（invitation letter），推送走 WebSocket，不再是"自报 session + 轮询"。

## 0. 前置条件

1. `./install.sh` 已执行并重启 opencode（`/Team`、`/team` 子命令可用）。
2. `which teamx` → `~/.local/bin/teamx`；`cd ~/github/teamx && ./tests/smoke.sh` 全 PASS。
3. 三个 opencode 窗口（同一台机器即可；两台机器见 §3）。
4. owner 机器放行入站端口 `5781`（跨机才需要）。

## 1. 网络模式数据流（三人）

```
┌─ owner 窗口 ───────────────┐   ┌─ 测试成员窗口 ──────────┐   ┌─ 评审成员窗口 ──────────┐
│ /Team agent + plugin       │   │ /Team agent + plugin    │   │ /Team agent + plugin    │
│ 建队 / serve start / invite │   │ import letter → mTLS 连  │   │ import letter → mTLS 连  │
└────────────┬───────────────┘   └────────────┬────────────┘   └────────────┬────────────┘
             │ spawn teamx serve (owner 机器)  │  wss://<owner-ip>:5781      │
             ▼                                 ▼                            ▼
   ┌───────────────────────────────────────────────────────────────────────────────┐
   │  teamx serve（mTLS 强制）— SQLite 账本（唯一权威）+ RPC + WS 广播（team→member）│
   └───────────────────────────────────────────────────────────────────────────────┘
```

- **身份**：每个成员的客户端证书 CN = `member:<id>:<role>`，server 从证书解析成员身份（不靠自报 session）。
- **推送**：任何一方 `publish` 落账后，server 按 team 实时广播给所有在线成员的 WS。
- **证书 ≠ 授权**：证书只能"连上 + 提交申请"；owner `approve` 后才进入 `active` 能干活。

## 2. 推荐：单机三窗口（内嵌 serve）

### 2.1 owner 窗口 —— 建队 + 起服务 + 发两个邀请函

```
/Team 创建团队「网络协作组」，目标：完成一次真实的多人协作（测试 + 评审）。
```

预期：`teamx_create_team` → 返回 `owner_member_id`（记下）、team id。

然后起内嵌 serve（会打印 server URL）：

```
/team serve start
```

预期：返回 `server_url: https://<你的局域网IP>:5781`（例如 `https://172.20.10.3:5781`）。记下这个地址，后面邀请和成员连接都要用它。

> 手动等价：`teamx serve --addr 0.0.0.0 --port 5781 --san <你的局域网IP> &`
> 查局域网 IP：macOS `ipconfig getifaddr en0`；Linux `hostname -I | awk '{print $1}'`。
>
> 可选（owner 也要实时推送）：给 owner 自己也发一张证书，并设 `TEAMX_SERVER_URL` 连上 serve ——
> `teamx cert issue <owner_member_id> owner --out ~/.teamx/owner-cert`，然后
> `export TEAMX_SERVER_URL=https://<你的局域网IP>:5781` 与
> `export TEAMX_MTLS_CERT=~/.teamx/owner-cert/member.crt TEAMX_MTLS_KEY=~/.teamx/owner-cert/member.key TEAMX_MTLS_CA=~/.teamx/ca/ca.crt`，重启 opencode。
> 不做这步也不影响协作，owner 用 `teamx_sync` 拉取即可。

发两个邀请函（**务必带 `--server-url <上面的地址>`**）：

```
/team invite "测试工程师: 负责功能测试并汇报缺陷" --server-url https://<你的局域网IP>:5781
```

```
/team invite "reviewer: 负责代码评审并给出意见" --server-url https://<你的局域网IP>:5781
```

预期：两次都返回单行 **letter**（`teamx-inv:v1:...`）——第一份给测试，第二份给评审。分别复制，通过聊天/文件发给两个成员。

### 2.2 测试成员窗口 —— 导入 + 连接

1. 先用 CLI 导入邀请函（**存证书 + 认领 pending 席位**；单机共享 DB 时一步到位）：

   ```
   /team import <测试的 letter> --name 测试员
   ```

   预期：`teamx_team_import` → `status=pending`，`role=role-<hex>`（中文 label「测试工程师」自动派生的 key），并提示证书已存到 `~/.teamx/letters/<invitation_id>/`。

2. 让本窗口走网络模式（连 owner 的 serve 拿实时推送）：**重启 opencode 即可**——letter 内含 server URL（`teamx_invitation.server.url`），插件启动时自动发现并建立 mTLS RPC/WS，无需手动设置 `TEAMX_SERVER_URL`。

> 多 server / 需覆盖时仍可显式指定：
> ```bash
> export TEAMX_SERVER_URL="https://<你的局域网IP>:5781"
> # 再重启 opencode
> ```
> 手动等价：`teamx team import <letter> --name 测试员 --session <本会话key>`。

### 2.3 评审成员窗口 —— 导入 + 连接

同上，用第二份 letter：

```
/team import <评审的 letter> --name 评审员
```

预期：`status=pending`，`role=reviewer`（ASCII label 直接作为 key）。

然后同样**重启 opencode**（插件自动从 letter 发现 server URL 并连接）。

### 2.4 owner 窗口 —— 审批两人

```
/Team 审批所有待审批成员。
```

预期：`teamx_approve` × 2（测试员、评审员都变 `active`，角色各自保留）。

### 2.5 三人并行工作（验证实时推送）

**测试成员窗口** 输入：

```
/Team 同步团队状态，开始编写测试用例，完成后向团队汇报「测试用例编写完成」。
```

**评审成员窗口**（几乎同时）输入：

```
/Team 同步团队状态，开始代码评审，完成后向团队汇报「代码评审完成」。
```

**owner 窗口** 输入：

```
/Team 同步团队状态，观察两名成员的最新进展。
```

预期（网络模式核心）：
- 任一成员 `teamx_publish progress` 后，**其余在线成员（已连 WS）在 <1s 内收到推送**（TUI toast「新事件」），无需手动 `teamx_sync`。
- owner 若也连了 WS（见 §2.1 可选步骤）会实时 toast；否则用 `teamx_sync` 拉取两条 `progress.published`（测试 / 评审）按 seq 排序。

## 3. 进阶：两台机器（真实跨网络）

owner 在机器 A，测试/评审在机器 B（或各一台）。步骤与 §2 相同，差异仅在**成员连接**：

1. owner（机器 A）建队 + `serve start` + 邀请时 `--server-url https://<机器A的局域网IP>:5781`。
2. 成员（机器 B）先本地导入 letter 存证书（此时本地 DB 无该邀请，返回 `status=stored`，只落盘）：
   ```bash
   teamx team import <letter> --name 测试员 --session <本会话key>
   ```
3. 成员**重启 opencode**（插件自动从 letter 发现 server URL 并连接；多 server 需覆盖时才显式 `export TEAMX_SERVER_URL`）。
4. 成员在 `/Team` 里再 `/team import <letter>`（此时走 RPC，把证书身份绑定到服务器上的预分配席位，变 pending；插件也会在首次 RPC 时自动认领）。
5. owner `approve` 后即可并行工作。

> 成员**不开放任何入站端口**（出站注册）；证书 = "能连上"，approve = "能干活"；吊销后连连接都会被拒。

## 4. 验证清单（任意终端）

```bash
# team_id 在 owner 建队时返回
teamx team status --team <team_id> --json
#  → members 三条：owner(owner/active)、测试员(role-xxx/active)、评审员(reviewer/active)

teamx events --team <team_id> --json
# 事件链应包含（按 seq）：
# team.created → invitation.created×2 → membership.pending×2 → membership.approved×2
#   → progress.published(测试) → progress.published(评审)（顺序取决于谁先汇报）

# 实时性：owner 的 serve 在线连接数（连了 WS 的成员数；owner 若也连则为 3）
curl --cacert ~/.teamx/ca/ca.crt --cert <owner cert> --key <owner key> https://<ip>:5781/health
#  → "connections": 2（两个成员在线；owner 也连则为 3）
```

## 5. 故障排查

| 现象 | 原因 / 处理 |
|---|---|
| 成员 `import` 报 `cannot read ca.crt` | owner 没先 `serve start`（PKI 未生成）；先起 serve 或 `teamx cert init` |
| 成员连不上 / TLS 握手失败 | 邀请时 `--server-url` 用了 `127.0.0.1`（应填局域网 IP）；或防火墙未放行 5781 |
| 证书校验失败（unable to verify） | server 证书 SAN 缺局域网 IP → 重启 serve 带 `--san <IP>`（`serve start` 会自动传） |
| 成员看不到实时推送 | 没设 `TEAMX_SERVER_URL` 或没重启 opencode；或仍在纯 CLI 轮询模式 |
| RPC 报 `member has been revoked` | 该成员邀请函已被 owner `invite-revoke` 吊销 |
| 成员 `status` 报 `member ... not found` | 还没在服务器上 `import`（两机场景第 4 步）；或证书与 letter 的 member_id 不匹配 |

## 6. 自动化等价验证

三人/网络流程已有自动化测试，无需真实模型即可跑通同一事件链与 mTLS/WS 链路：

```bash
./tests/run-all.sh            # 全量（含 mtls-test.sh / ws-test.ts / cross-network.sh）
./tests/cross-network.sh      # 单机局域网模拟（经非 loopback IP 走完整 mTLS）
./tests/mtls-test.sh          # 证书身份 + 吊销强制
bun tests/ws-test.ts          # WS 推送：register / 实时广播 / 心跳 / 吊销断连
```

## 7. 测试记录

- 日期：____
- 方式：□ 单机三窗口　□ 两台机器
- 结果：□ 全部通过　□ 部分通过（问题：__________）
- 两名成员是否都实时收到对方/owner 的推送：□ 是　□ 否
- 事件链：`invitation.created×2 → membership.pending×2 → membership.approved×2 → progress.published×2` 是否完整：□ 是　□ 否
