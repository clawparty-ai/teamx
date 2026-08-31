# 在云主机运行 teamx server 与 teamx member（最佳实践）

> 状态：**已实践验证**（hub03 = serve + owner；197 = ttyd + opencode + member）
> 目标读者：想把 teamx 从「单机演示」升级为「云端多人协作」的部署者
> 前置：`teamx` 二进制 v0.2.0+（含 `team invite --as-lead / --auto-approve`，schema v14）
> 关联：`docs/03-design-network.cn.md`（网络模式设计）、`docs/20-manual-tunnel-proxy-cli.cn.md`（隧道手册）、`docs/04-design-team-invite.cn.md`（邀请函）

---

## 0. 架构总览

```
                 ┌─────────────────────────────────────────────┐
                 │   云主机 A  (hub03, 公网可达)                │
                 │   systemd: teamx serve :8888 (mTLS)          │
                 │   - 权威 SQLite 账本 (TEAMX_DB)               │
                 │   - CA / server 证书 (~/.teamx/ca)           │
                 │   - 团队在此创建：owner 身份在 A                │
                 └──────────────┬──────────────────────────────┘
                                │ mTLS (证书 CN = member:<id>:<role>[:<user>])
        ┌───────────────────────┼────────────────────────────┐
        ▼                       ▼                             ▼
 ┌───────────────┐      ┌───────────────┐            ┌───────────────┐
 │ 本地管理机     │      │ 云主机 B (197)  │            │ 其他成员        │
 │ import lead    │      │ ttyd+opencode │            │ teamx CLI/插件 │
 │ letter 后      │      │ teamx 插件     │            │               │
 │ 远程管理 team   │      │ tunnel expose │            │ tunnel forward│
 └───────────────┘      │ ttyd :7681    │            │ → 本地端口     │
                        └───────────────┘            └───────────────┘
```

**核心思路**：云端一台机器作为 `teamx serve`（权威账本 + CA），团队在**该机器本地**创建（owner 身份天然落在 serve 机器）；其他所有机器（本地管理机、另一台云主机、普通成员）都是 member/lead，通过 mTLS 出站连接，**零入站端口、零暴露面**。

---

## 1. 准备 teamx 二进制

三处都需要 teamx 二进制（本地管理机 / 云主机 A / 云主机 B）：

- 本地：`cargo build --release -p teamx` 后 `install -m 0755 target/release/teamx ~/.local/bin/teamx`
- 云主机：把源码包传到目标机，在**目标架构**上构建（ARM/x86 各自编译），然后安装到系统目录：
  ```bash
  sudo install -m 0755 ~/teamx/target/release/teamx /usr/local/bin/teamx
  ```
- 云主机上建议同时保留 `~/.local/bin/teamx`（systemd 服务用绝对路径指向它或 `/usr/local/bin/teamx`）。

---

## 2. 云主机 A：用 systemd 启动 teamx server

建议用**专用系统用户**（如 `teamx`）跑 serve，隔离文件权限：

```bash
sudo useradd -r -m -s /usr/sbin/nologin teamx
```

创建 systemd 服务 `/etc/systemd/system/teamx-serve.service`：

```ini
[Unit]
Description=teamx network-mode server (HTTP RPC + WS push + tunnels)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=teamx
Environment=TEAMX_HOME=/home/teamx/.teamx
Environment=TEAMX_DB=/home/teamx/.teamx/teamx.db
ExecStart=/usr/local/bin/teamx serve --addr 0.0.0.0 --port 8888 --san <云主机域名或公网IP>
Restart=always
RestartSec=3
NoNewPrivileges=true
ProtectSystem=full
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

> 说明：
> - `--port 8888`：serve 的 mTLS HTTPS 端口（RPC + WS + tunnels 共用）。安全组必须放行它。
> - `--san`：可多个，建议同时给域名与公网 IP（如 `--san hub03.example.com --san 1.2.3.4`），否则客户端校验 SNI 会失败。
> - 首次启动自动生成实例 CA（`$TEAMX_HOME/ca/`）与 server 证书。
> - **从不上公网 `0.0.0.0:8888` 暴露给非成员**：mTLS 本身要求客户端证书，未持有证书的连接在 TLS 层即被拒绝。

启动并验证：

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now teamx-serve
sudo systemctl status teamx-serve        # active (running)
journalctl -u teamx-serve -n 20          # 应看到 "teamx serve listening on https://0.0.0.0:8888 (mtls, ...)"
```

---

## 3. 在云主机 A 本地创建团队（owner 在 server 机器）

> **为什么必须在 serve 机器创建第一个团队**：网络模式下 `team.create` 要求调用者已有成员席位（证书），而第一个团队没有任何可签发的邀请函来引导。因此第一个团队必须在持有 CA 的 serve 机器本地用 CLI 创建（引导约束）。

```bash
# 以 teamx 用户（或任意有写 DB 权限的 shell）执行：
sudo -u teamx bash
export TEAMX_HOME=/home/teamx/.teamx TEAMX_DB=/home/teamx/.teamx/teamx.db

teamx team create "cloud-proxy-lab" --session s:cloud \
  --goal-title "云端代理实验团队"
# 期望输出：team 含 id / invite_token（invite_token 仅本地 join 用，网络模式用邀请函）
```

记录返回的 **team id**（后续 invite / tunnel 指定 `--team <id>`）。

> 提示：owner 会话的 `--session` 由你自定（如 `s:cloud`），它只在 serve 本地的 CLI 上下文有意义；网络模式下 server 一律以**证书 CN** 识别身份，`--session` 参数被忽略。

---

## 4. 签发第一个邀请函：标记为 team lead + auto-approve

> 依赖 v0.2.0+（schema v14）的 `team invite --as-lead --auto-approve`。若你的二进制不含此参数，见文末「兼容」一节。

```bash
# 云主机 A（owner）：
teamx team invite "管理员: 远程管理团队" \
  --name-hint "admin" \
  --server-url https://<云主机A>:8888 \
  --as-lead --auto-approve \
  --session s:cloud --team <team-id>
# 期望输出：ok + letter（teamx-inv:v1:<base64> ...）
```

说明：
- `--as-lead`：成员 import 后**直接是 co-lead**（`is_lead=1`），可行使 owner 级团队授权（invite/approve/goal/role 等），除"把 owner 角色授予他人 / 降级真正 owner"外。
- `--auto-approve`：成员 import 后**直接 active**（跳过 owner 手动 approve），自动获得团队 git 仓库读权限。
- `--server-url`：**必须填成员可连到的地址**（云主机域名或公网 IP），不能是 `127.0.0.1`。
- `--as-lead` 仅 **primary owner** 可签发（co-lead 不能 mint 新 lead）。

把返回的 `letter` 单行字符串发给目标管理员。

---

## 5. 管理员在本地机器 import 邀请函 → 获得远程管理权

管理员机器（本地）准备 teamx 二进制后：

```bash
# 本地，无需连接任何 server 即可 import（import 会落证书并认领席位）
teamx team import "teamx-inv:v1:<...粘贴 letter...>" --name admin --session s:admin
# 期望输出：ok, status=active, is_lead=true  （auto-approve + as-lead 已生效）
```

import 之后：
- 本地 `~/.teamx/letters/<invitation_id>/` 存有 `client.crt / client.key / ca.crt`
- 本地 **opencode 插件**会自动从 letter 发现 server URL（`TEAMX_SERVER_URL` 无需手动设置），所有 `teamx_*` 工具走网络通道
- 管理员即可在本地远程管理团队：`teamx team status --team <id>`、`teamx goal set`、`teamx team invite` 等（需要时带 `--session s:admin --team <id>`）

> 验证：`teamx team status --session s:admin --team <team-id>` 应返回 `my_is_lead: true`。

---

## 6. 另一台云主机 B：ttyd + opencode + teamx 插件，并把 ttyd 端口通过隧道开放给团队

### 6.1 准备云主机 B

1. 安装 teamx 二进制（系统目录 `/usr/local/bin/teamx`）。
2. 安装 opencode（aarch64/x86_64 各自官方安装）与 ttyd：
   ```bash
   curl -fsSL https://opencode.ai/install | bash        # 装到 ~/.opencode/bin
   sudo cp ~/.opencode/bin/opencode /usr/local/bin/opencode
   sudo install -m 0755 <ttyd 二进制> /usr/local/bin/ttyd
   ```
3. 创建 cicd 用户（可选，用于隔离 web 终端身份）：
   ```bash
   sudo useradd -m -s /bin/bash -U cicd
   ```

### 6.2 用 systemd 跑 ttyd + opencode

`/etc/systemd/system/ttyd-cicd.service`：

```ini
[Unit]
Description=ttyd web terminal for cicd user (opencode)
After=network-online.target

[Service]
Type=simple
User=cicd
ExecStart=/usr/local/bin/ttyd -p 7681 -t disableReconnect=true /usr/local/bin/opencode
Restart=always
RestartSec=3
WorkingDirectory=/home/cicd
Environment=HOME=/home/cicd

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload && sudo systemctl enable --now ttyd-cicd
sudo ss -tlnp | grep 7681    # ttyd 监听 0.0.0.0:7681
```

> ttyd 在 7681 提供 web 终端（直接进入 opencode TUI）。**默认无认证**：若安全组放行公网，务必在 `ExecStart` 加 `-c user:pass` 或走内网/隧道访问。

### 6.3 云主机 B 作为 teamx 成员

云主机 B 需要一枚成员邀请函（可用第 4 步同款方式由 owner 签发，或复用 `--as-lead` 之外的普通邀请函）：

```bash
# 云主机 B：
teamx team import "teamx-inv:v1:<...>" --name cicd-b --session s:b
```

### 6.4 把 ttyd 端口通过 teamx 隧道开放给团队

云主机 B 是 **provider**，把本地 7681 暴露给团队（`local` 模式，server 不占公网端口，最安全）：

```bash
# 云主机 B（长驻进程，可用 systemd 或 nohup）：
teamx tunnel expose ttyd --port 7681 --mode local \
  --session s:b --team <team-id> --server https://<云主机A>:8888
# 期望输出：ok tunnel registered: name=ttyd mode=local port=0 target_port=7681
```

### 6.5 管理员本地把 ttyd 端口映射回来使用

管理员（或任何有权限的成员）在本地做 **consumer** 端 forward：

```bash
# 本地：
teamx tunnel forward ttyd --local-port 17681 \
  --session s:admin --team <team-id>
# 期望输出：ok forward: name=ttyd listening on 127.0.0.1:17681
```

然后浏览器访问 `http://127.0.0.1:17681/` 即可进入云主机 B 的 web 终端（opencode）。**全程不暴露 7681 公网端口**。

> 隧道访问隔离：若邀请函绑定了 `user`，则同 user 的设备零配置互访；其他 user 默认拒绝，owner/lead 例外（见 `docs/20-manual-tunnel-proxy-cli.cn.md` §4.5）。

---

## 7. 日常运维小结

| 操作 | 命令（在对应机器） |
|---|---|
| 查看 serve 状态 | `sudo systemctl status teamx-serve` |
| serve 日志 | `journalctl -u teamx-serve -f` |
| 团队状态 | `teamx team status --session <s> --team <id>` |
| 签发成员邀请 | `teamx team invite "角色: 描述" --server-url https://A:8888 --session <s> --team <id>` |
| 签发 lead+自动批准 | 加 `--as-lead --auto-approve` |
| 查看已签发邀请 | `teamx team invite list --session <s> --team <id>` |
| 吊销邀请 | `teamx team invite revoke <invitation_id> --session <s> --team <id>` |
| 开放服务 | `teamx tunnel expose <name> --port <local> --mode local --session <s> --team <id>` |
| 本地使用 | `teamx tunnel forward <name> --local-port <p> --session <s> --team <id>` |
| 关闭隧道 | `teamx tunnel close <name> --session <s> --team <id>` |

---

## 8. 安全与注意事项

1. **8888 安全组**：只对需要协作的对端开放；mTLS 已提供证书级保护，但结合安全组能进一步缩小暴露面。
2. **ttyd 无认证**：除非走隧道/内网，否则公网直连必须加 `-c user:pass`。
3. **`--server-url` 不要写 `127.0.0.1`**：邀请函里的 server 地址是给远端成员连的。
4. **owner 身份在 serve 机器**：真正 owner 的证书/DB 在云主机 A；lead（co-lead）可远程管理但保留 founder 语义。
5. **证书即身份**：邀请函泄露 = 攻击者可 import 该席位；用 `team invite revoke` 吊销后证书在连接时被拒。
6. **schema 迁移**：v0.2.0 → 新版本首次运行自动迁移（如 v14 加 `invitations.auto_approve/as_lead`），无需手工干预。

---

## 9. 兼容：旧二进制没有 `--as-lead / --auto-approve`

若 serve 机器是 v0.1.x/v0.2.0（无 v14），等价流程退化为两步：

```bash
# 1) 签发普通邀请函
teamx team invite "管理员: 远程管理团队" --server-url https://A:8888 --session s:cloud --team <id>
# 2) 成员 import 后，owner 在 serve 机器手动批准 + 提升
teamx team approve <member_id> --session s:cloud --team <id>
teamx team promote-lead <member_id> --session s:cloud --team <id>
```
