# 云端 opencode 成员节点：headless server + ttyd TUI（attach 同一 server）+ teamx 隧道远程访问

> 状态：已在 3 台云主机（东京/首尔/美国）部署验证
> 目标读者：想给 teamx 成员在云端跑一个「可交互、可被 lead 远程访问」的 opencode 工作节点的人
> 前置：teamx 企业版（含 `/tunnel` 反向隧道、per-user 隧道 ACL）；节点为 Ubuntu 24.04 aarch64

---

## 0. 为什么这样搭

单个 worker 节点上我们希望同时有：

1. **交互终端**（worker 本人经 ttyd 用浏览器操作 opencode TUI）
2. **Web / Headless**（lead 或 worker 经浏览器看同一工作状态 / 调 API）

关键点：**两者要连同一个 opencode server**，否则各自是独立进程、各自内存事件不同步（历史共享但实时不同步）。所以采用 opencode 的「单 server + 多 client」模型：

```
opencode serve（唯一 server，headless，绑 127.0.0.1:<port>）
   ├── ttyd → opencode attach http://127.0.0.1:<port>   （TUI 作为 client）
   └── opencode Web / Headless API（浏览器经隧道访问同一 server）
```

> 反例：ttyd 里直接跑 `opencode -c`（TUI 自带内嵌 server）再另起 `opencode serve`，就是两个 server 实例——历史共享、实时不共享。

---

## 1. 架构总览

```
              teamx server（权威账本/CA，HK :443, mTLS）
                     ▲                ▲
        (tunnel expose 注册)   (tunnel expose 注册)
                     │                │
        ┌────────────┴──────┐  ┌─────┴──────────────────┐
        │ ttyd (7682)       │  │ opencode serve (4199)  │   ← 节点上两个被暴露的本地服务
        └────────┬──────────┘  └────────────────────────┘
                 ▼
   opencode attach http://127.0.0.1:4199   （TUI client，同一 server）

  本地（worker 本人 / lead）：
    teamx tunnel forward <ttyd隧道>   → http://127.0.0.1:xxxxx/   （浏览器操作 TUI）
    teamx tunnel forward <serve隧道>  → http://127.0.0.1:yyyyy/   （浏览器 Web / Headless API）
```

- 零入站端口：节点不开放任何公网端口；ttyd / serve 都绑 `127.0.0.1`，仅经 teamx 反向隧道出去。
- 身份：ttyd/serve/tunnel 都作为**该 worker 的 teamx 成员**运行（用它的邀请函证书），隧道天然归属该 user。

---

## 2. 每个节点需要装的东西

```bash
# 1) teamx 二进制（Ubuntu aarch64 上构建或从其它同架构机拷贝）
sudo install -m 0755 teamx /usr/local/bin/teamx

# 2) ttyd
sudo install -m 0755 <ttyd.aarch64> /usr/local/bin/ttyd   # https://github.com/tsl0922/ttyd/releases

# 3) opencode（aarch64）
curl -fsSL https://opencode.ai/install | bash    # 装到 ~/.opencode/bin
sudo cp ~/.opencode/bin/opencode /usr/local/bin/opencode

# 4) worker 系统用户 + 工作目录（member 邀请函放这里）
sudo useradd -m -s /bin/bash -U <user>           # 如 caicai
sudo mkdir -p /home/<user>/work && chown <user> /home/<user>/work
```

---

## 3. 让节点成为 teamx 成员（user 绑定，决定隧道谁能访问）

worker 的邀请函在签发时绑定 user（`--user-name <人>`），证书 CN 带 user：

```bash
# owner（在 serve 机）签发 worker 邀请函
teamx team invite "kb-engineer: 知识库工程师 (worker)" \
  --name-hint <user>-worker --user-name <user> \
  --server-url https://<server> --session <owner> --team <team-id>
```

节点导入 + 认领（本地导入落证书，再经 mTLS RPC 认领席位，owner 批准）：

```bash
cd /home/<user>/work
teamx team import "<worker letter>" --name <user>-worker --session <node>
# 认领 RPC：POST /rpc team.import（带该邀请函的 mTLS 证书）
# owner：teamx team approve <member_id> --session <owner>
```

之后 `.teamx/letters/<id>/` 里有 `ca.crt/client.crt/client.key`，tunnel/serve 都基于它鉴权。

---

## 4. systemd：单 server + attach

**4.1 opencode headless server（唯一 server）** `/etc/systemd/system/opencode-serve-<user>.service`：

```ini
[Unit]
Description=opencode headless server for <user> (single server)
After=network-online.target

[Service]
Type=simple
User=<user>
WorkingDirectory=/home/<user>/work
Environment=HOME=/home/<user>
Environment=PATH=/usr/local/bin:/usr/bin:/bin
# 可选：给 server 加口令（被隧道暴露控制面时建议保留；本项目按 per-user ACL 取舍可去）
# Environment=OPENCODE_SERVER_PASSWORD=<pw>
ExecStart=/usr/local/bin/opencode serve --hostname 127.0.0.1 --port <PORT>
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
```

**4.2 ttyd = opencode attach（TUI client，不要用 `-c` 起内嵌 server）** `/etc/systemd/system/ttyd-<user>.service`：

```ini
[Unit]
Description=ttyd web terminal for <user> (opencode attach client)
After=network-online.target opencode-serve-<user>.service

[Service]
Type=simple
User=<user>
# 注意：用 opencode attach <url>，而不是 opencode -c
ExecStart=/usr/local/bin/ttyd -p 7682 -W -T xterm-256color -t disableReconnect=true \
  /bin/bash -lc "cd /home/<user>/work && exec /usr/local/bin/opencode attach http://127.0.0.1:<PORT> -c"
Restart=always
RestartSec=3
WorkingDirectory=/home/<user>/work
Environment=HOME=/home/<user>
Environment=PATH=/usr/local/bin:/usr/bin:/bin

[Install]
WantedBy=multi-user.target
```

> - 先起 server，ttyd 依赖它（`After=...`）。
> - `-c/--continue` 继续上次会话；有口令时 attach 加 `--password`。
> - 若之前误跑过 `opencode -c`（自带 server），先停掉，避免端口/数据竞争。

启动：

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now opencode-serve-<user> ttyd-<user>
```

验证进程布局（应只有**一个** `opencode serve` 监听，加一个 `opencode attach` 子进程，**没有**第二个内嵌 server）：

```bash
ps -eo pid,ppid,cmd | grep opencode | grep -v grep
#   opencode serve --hostname 127.0.0.1 --port <PORT>     ← 唯一 server
#   bash -lc ... exec opencode attach http://127.0.0.1:<PORT> -c   ← TUI client
sudo ss -tlnp | grep <PORT>     # 只一个监听
```

---

## 5. 用 teamx tunnel 暴露给「同一用户」远程访问

**5.1 节点上注册两个隧道（provider 常驻）**：给每个被暴露的本地端口建一个 systemd：

```ini
# /etc/systemd/system/tx-expose-<user>-ttyd.service
ExecStart=/usr/local/bin/teamx tunnel expose ttyd-<user> \
  --port 7682 --mode local --server https://<server> --session <user>
```
```ini
# /etc/systemd/system/tx-expose-<user>-serve.service
ExecStart=/usr/local/bin/teamx tunnel expose opencode-<user> \
  --port <PORT> --mode local --server https://<server> --session <user>
```

（`User=<user>`，`WorkingDirectory=/home/<user>/work`，`Restart=always`。）

因为 tunnel 绑定的是 **worker user**，per-user ACL 下「同一 user」的设备 / owner / lead 才能 forward。

**5.2 本地（worker 本人 / lead）映射回端口：**

```bash
teamx tunnel forward ttyd-<user>    --local-port 19033 --session <my>
teamx tunnel forward opencode-<user> --local-port 19034 --session <my>
```

浏览器：
- `http://127.0.0.1:19033/` → ttyd TUI
- `http://127.0.0.1:19034/` → opencode Web / Headless（先 Add project → `~/work`，才能创建 session；API 见 `/api/session`）

---

## 6. 身份 / 权限模型（重要）

- **worker member**：跑在云节点上的「机器人」身份，负责 `opencode serve / ttyd / tunnel expose`。
- **personal member**：真人（zhongm/zhaoz/caicai）在自己电脑上的身份，用来 `tunnel forward` 访问自己的节点。
- 两者签发时都用 `--user-name <人>` 绑定到**同一个 user** → per-user 隧道 ACL 允许「本人 + owner/lead」访问该 user 的隧道，**其它 user 拒绝**。
- **owner/co-lead 全通**：修复过 `serve.rs` 隧道授权里 `is_team_owner` 参数顺序 bug（原以 `(tid, mid)` 传入，应为 `(mid, tid)`），否则 owner/lead 对 user 绑定隧道恒判 false 被拒。
- **无口令的取舍**：serve 绑 127.0.0.1 + per-user ACL 后，headless 控制面只暴露给本人/owner/lead → 本项目去掉 `OPENCODE_SERVER_PASSWORD` 以方便浏览器直连。若团队信任边界更宽或节点多用户共享，建议保留口令（serve 无口令会提示 unsecured）。

---

## 7. 邀请函归档在 serve 机 `.teamx`

所有 worker/personal 邀请函建议落一份到 serve 机的项目 `.teamx/members/` 下（0600 root），便于分发与审计：

```
/var/lib/teamx/projects/<proj>/.teamx/members/
  <user>-personal/invitation.letter     # 真人导入自己电脑
  worker-<user>/invitation.letter       # 节点导入（已用）
```

> 邀请函单次有效：被节点认领后即失效；真人的 personal 邀请留给本人导入，不要替他们试。

---

## 8. 排查笔记

| 现象 | 原因 / 处理 |
|---|---|
| worker 在线，真人 caishu 显示离线 | 「在线」= `member_connections` 有未断开的 WS。worker 因 tunnel expose 长连接在线；真人需在其本地 opencode（插件）里建 `/ws`（在本人 workspace 目录启动 opencode）|
| 点 opencode Web「创建 session」没反应 | 需先在 Web 里 **Add project → 选 `~/work`**，之后创建/聊天正常 |
| TUI 与 headless 实时不一致 | 用了两个 server 实例（ttyd 里 `opencode -c` + 另起 serve）。统一成「一个 serve + attach」|
| `Exec format error` | 把 macOS 构建的二进制传到了 Linux 节点。务必在 Ubuntu aarch64 上构建/分发 |
| lead 能 forward 但 curl 空 | 老版本 `is_team_owner` 参数顺序 bug；升级到含修复的版本 |

---

## 9. 常用运维

```bash
sudo systemctl status opencode-serve-<user> ttyd-<user> tx-expose-<user>-*
sudo journalctl -u opencode-serve-<user> -f
sudo systemctl restart opencode-serve-<user> ttyd-<user>   # 换配置/口令后
# 模型/会话 API（本地 forward 后）：
curl http://127.0.0.1:19034/api/session
```
