# Member 云节点部署手册（含 clone workspace 接入）

> 适用：把一台 AWS 云主机（Ubuntu 24.04 aarch64）配置成 teamx **member 云节点**——跑 opencode server + ttyd/TUI + opencode headless + teamx fs，用该成员自己的邀请函加入团队，隧道归该成员。
> 本文以原 powernano 三个 member 节点（东京 zhongm / 首尔 zhaoz / 美国 caicai）的真实部署为蓝本整理，供以后部署（含 `team clone` 克隆出的 workspace）复用。
> 更新：2026-09-04

## 0. 节点清单与访问

三台 member 节点与 server 的实例/IP/密钥信息见 **HK server** 上的 `/var/lib/teamx/projects/CLOUD-NODES.md`（AWS 账户 975049986480，密钥 `~/.ssh/teamx-powernano.pem`，SSH 用户 `ubuntu`）。

| 节点 | 区域 | 实例名 | 公网 IP | 成员 user | teamx 版本 |
|---|---|---|---|---|---|
| 东京 | ap-northeast-1 | powernano-zhongm | 35.77.5.99 | zhongm | 最新(含 clone) |
| 首尔 | ap-northeast-2 | powernano-zhaoz | 3.37.16.165 | zhaoz | 最新(含 clone) |
| 美国 | us-west-2 | powernano-caicai | 184.34.85.90 | caicai | 最新(含 clone) |
| server | ap-east-1(香港) | powernano | 95.40.221.159 | — | 最新(含 clone) |

> 韩国节点从本机(mac) SSH 可能超时，可从 HK server 中转（把 pem 放 HK 后 `ssh -i pem ubuntu@3.37.16.165`）。

## 1. 顶层模型

```
teamx server（权威账本/CA，HK 95.40.221.159:443，mTLS）
      ▲  ▲  ▲
      │  │  │  tunnel expose 注册（每个节点 3 条）
      │  │  │
  ┌───┴──┴──┴──────────────────────────┐
  │ member 节点 (zhongm/zhaoz/caicai)   │
  │  opencode serve   127.0.0.1:4199   │   ← 唯一 server（headless）
  │  ttyd (TUI attach)  0.0.0.0:7681   │   ← opencode attach http://127.0.0.1:4199
  │  teamx fs         127.0.0.1:4197   │   ← 文件浏览后端
  │  3× teamx tunnel expose（local 模式）
  └────────────────────────────────────┘
```

- **零入站**：节点只出站连 HK server（mTLS），不开放公网端口。
- **单 workspace = 单团队席位**：每个 `/home/<user>/work*` 里 `.teamx/letters/<invitation>` 即该成员在该团队的席位。要加入**另一支团队（如 clone 出的 powernano-v2）**就再建一个 workspace（端口错开）。
- 身份：节点上所有进程都以成员 user 运行，凭该 workspace 的邀请函证书对外鉴权；隧道归属该 user（per-user ACL：本人 + owner/co-lead 可 forward）。

## 2. 每台节点需要安装的东西（已装则跳过）

```bash
# teamx（Ubuntu aarch64）：从 HK server /tmp/teamx-enterprise-hk/target/release/teamx 分发
sudo install -m 0755 teamx /usr/local/bin/teamx

# opencode（aarch64）
curl -fsSL https://opencode.ai/install | bash && sudo cp ~/.opencode/bin/opencode /usr/local/bin/opencode

# ttyd（aarch64）https://github.com/tsl0922/ttyd/releases
sudo install -m 0755 ttyd.aarch64 /usr/local/bin/ttyd

# teamx opencode 插件（grill/管理命令等更新随插件走）
mkdir -p /opt/teamx-plugin && tar -xzf teamx-plugin.tar.gz -C /opt/teamx-plugin
teamx plugin install /opt/teamx-plugin/opencode-plugin     # 装到 ~/.config/opencode

# 成员系统用户 + workspace
sudo useradd -m -s /bin/bash -U <user>          # 如 zhongm / zhaoz / caicai
```

> 分发物：二进制 `teamx`（arm64）+ 插件包 `teamx-plugin.tar.gz`（`tar -czf teamx-plugin.tar.gz opencode-plugin`）都先在 server/本机构好后 scp 到各节点。

## 3. 建 workspace 并导入该成员的邀请函

**一个 workspace 对应一个团队席位**。以原 powernano 为例（成员 zhongm，团队 powernano）：

```bash
sudo mkdir -p /home/<user>/work && sudo chown <user> /home/<user>/work
sudo -u <user> sh -c 'cd /home/<user>/work && teamx team import "$(cat /tmp/<name>.letter)" --session <user>'
# → 本地落证书到 .teamx/letters/<invitation>/；席位由后续连接认领
```

**加入 clone 出来的团队（如 powernano-v2）时**：clone 会给每个成员发**新邀请函**（同一 user 绑定保留），且必须**另开新 workspace**（不能复用旧团队的 `.teamx`，否则身份混在旧团队）：

```bash
sudo mkdir -p /home/<user>/work-powernano-v2 && sudo chown <user> /home/<user>/work-powernano-v2
sudo -u <user> sh -c 'cd /home/<user>/work-powernano-v2 && teamx team import "$(cat /tmp/<user>-pnv2.letter)" --session <user>-pnv2'
```

> 邀请函单次有效：被认领后失效；owner 的 personal letter 留给 owner 本人，不要替试。

## 4. systemd 单元（模板，实测自原节点）

> 端口基线：opencode-serve 4199、teamx-fs 4197、ttyd 7681。同机再加一个 workspace 时整体 +k 错开。
> `User=<user>`、`WorkingDirectory=/home/<user>/work`、`Environment=HOME=/home/<user>`。

### 4.1 opencode-serve-<user>.service（唯一 server）
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
ExecStart=/usr/local/bin/opencode serve --hostname 127.0.0.1 --port 4199
Restart=always
RestartSec=3
[Install]
WantedBy=multi-user.target
```

### 4.2 ttyd-<user>.service（TUI client，attach 同一 server，勿用 `-c` 内嵌）
```ini
[Unit]
Description=ttyd web terminal for <user> (opencode attach client)
After=network-online.target opencode-serve-<user>.service
[Service]
Type=simple
User=<user>
ExecStart=/usr/local/bin/ttyd -p 7681 -W -T xterm-256color -t disableReconnect=true \
  /bin/bash -lc "cd /home/<user>/work && exec /usr/local/bin/opencode attach http://127.0.0.1:4199 -c"
Restart=always
RestartSec=3
WorkingDirectory=/home/<user>/work
Environment=HOME=/home/<user>
Environment=PATH=/usr/local/bin:/usr/bin:/bin
[Install]
WantedBy=multi-user.target
```

### 4.3 teamx-fs-<user>.service（文件服务后端）
```ini
[Unit]
Description=teamx fs file service (workspace: <user>/work)
[Service]
Type=simple
User=<user>
WorkingDirectory=/home/<user>/work
Environment=HOME=/home/<user>
Environment=TEAMX_HOME=/home/<user>/work/.teamx
Environment=PATH=/usr/local/bin:/usr/bin:/bin
ExecStart=/usr/local/bin/teamx fs --addr 127.0.0.1 --port 4197
Restart=always
RestartSec=3
[Install]
WantedBy=multi-user.target
```

### 4.4 三条隧道（teamx tunnel expose，local 模式）
```ini
# tx-expose-<user>.service          → ttyd（TUI）
ExecStart=/usr/local/bin/teamx tunnel expose ttyd-<user> --port 7681 --mode local --server https://95.40.221.159 --session <user>
# tx-expose-<user>-serve.service    → opencode serve（headless/Web）
ExecStart=/usr/local/bin/teamx tunnel expose opencode-<user> --port 4199 --mode local --server https://95.40.221.159 --session <user>
# tx-expose-fs-<user>.service       → teamx fs（文件浏览）
ExecStart=/usr/local/bin/teamx tunnel expose fs-<user> --port 4197 --mode local --server https://95.40.221.159 --session <user>
```
每条：`User=<user>`、`WorkingDirectory=/home/<user>/work`、`Environment=HOME=/home/<user>`、`Environment=TEAMX_HOME=/home/<user>/work/.teamx`、`Environment=TEAMX_SERVER_URL=https://95.40.221.159`、`Restart=always`。

## 5. 启动与验证

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now opencode-serve-<user> ttyd-<user> teamx-fs-<user> \
  tx-expose-<user> tx-expose-<user>-serve tx-expose-fs-<user>

# 验证：应只有一个 opencode serve 监听 4199 + attach 子进程 + fs 4197 + ttyd 7681
ps -eo pid,ppid,cmd | grep -E 'opencode|ttyd' | grep -v grep
sudo ss -tlnp | grep -E ':(4199|4197|7681)'
# 隧道在线（server 侧或 tunnel list）
```

## 6. 加入团队（成员入队）——在 opencode 里 `/team import`

节点跑起来后，经 ttyd（浏览器开 `http://127.0.0.1:<ttyd-forward>/`）进 opencode TUI，输入：

```
/team import <该成员在 server 上的邀请函>
```

即完成认领：服务器收到 `team.import` RPC，用邀请函证书 CN 认领预分配席位（clone 邀请函 `auto_approve` → 立即 active），并把该 workspace 与 server 绑定。

> 也可以命令行触发：在 workspace 里 `teamx team import "<letter>" --session <user>` 只会在本地落证书；真正的席位认领发生在与 server 建立连接（opencode 启动 / `/team import`）时，走 mTLS RPC。

## 7. 远程访问（worker 本人 / lead）

```bash
# 本地（个人/lead）把隧道映射回本地端口
teamx tunnel forward ttyd-<user>     --local-port 19033 --session <my>
teamx tunnel forward opencode-<user> --local-port 19034 --session <my>
teamx tunnel forward fs-<user>       --local-port 19035 --session <my>
# 浏览器：19033 → TUI；19034 → opencode Web/Headless（Add project → /home/<user>/work）
# 隧道 per-user ACL：本人 + owner/co-lead 可访问，其它 user 拒绝
```

## 8. 常见问题

- **只导入没认领**：CLI `team import` 在无 server DB 的机器上仅本地存证（`status: stored`）；必须经 opencode `/team import` 或 RPC 完成认领才 active。
- **端口冲突 / 双 server**：确保每个 workspace 只有一套进程（一个 `opencode serve`）。同机多 workspace 记得端口 +k。
- **韩国节点 SSH 超时**：从 HK server 中转（SG 已放行 22/0.0.0.0）。
- **团队成员数只增不减**：clone 后每人要导入自己的新 letter 才会出现在新团队；旧团队成员不受影响。

## 9. 相关文档

- HK server 节点清单：`/var/lib/teamx/projects/CLOUD-NODES.md`
- 架构蓝本：`docs/30-cloud-node-opencode-headless-tui.cn.md`
- 团队 clone：`docs/34-team-clone.cn.md`（§8 节点接入、§13 使用指南）
- 部署物来源：server `/tmp/teamx-enterprise-hk/target/release/teamx`（二进制）、本机 `opencode-plugin/`（插件）

## 10. teamx-ui 企业看板：按团队隔离 + 经隧道开放给 lead

### 10.1 teamx-ui 是什么

`teamx ui` 是**企业版 activity analytics 看板**（HTTPS + token，owner-only）：读 server 本地 DB，展示团队 activity/成本/节点/成员等。默认绑 `127.0.0.1:9527`，每次启动生成**随机 token**（打印在日志）。

```
teamx ui --addr 127.0.0.1 --port 9527 --session <owner session> --db <db> --server-url <server>
```

### 10.2 关键：会话决定看板看到哪个团队（owner/lead 隔离）

`teamx ui` 启动时收集该 `--session` 对应成员**所拥有/lead 的团队**（`owner_teams`）。为了看板只服务目标团队：

1. **用目标团队的 owner/lead 会话启动**（不要用旧团队/无关 session）。例（powernano-v2）：
   ```ini
   # /etc/systemd/system/teamx-ui.service
   ExecStart=/usr/local/bin/teamx ui --addr 127.0.0.1 --port 9527 \
     --session yongyan-pnv2-owner \
     --db /var/lib/teamx/teamx.db --server-url https://95.40.221.159
   ```
   `yongyan-pnv2-owner` → powernano-v2 的 owner 成员（`e0b655b9`），于是 `owner_teams = {powernano-v2}`。

2. **代码层（已合入）按 owner 团队隔离**：
   - `/api/teams` 只返回 `owner_teams`（原先列 DB 全部团队，会导致用旧 session 启动时默认显示旧团队数据）；
   - 数据接口用 `owned_team` 校验：请求的 `?team=` 必须属于 `owner_teams`，否则 **403**；`team` 缺省时用第一个 owner 团队（避免 `team=None` 跨团队聚合）。
   - 效果：同一 DB 下，用哪支团队的 owner 会话启动，看板就只看哪支团队。

改代码后需重编并部署二进制 + `systemctl restart teamx-ui`（token 会变）。

### 10.3 经隧道开放给远端 lead（如 caishu）

看板绑 127.0.0.1，把它反向隧道暴露给远端 co-lead/owner：

```ini
# HK server：/etc/systemd/system/tx-expose-teamx-ui.service
[Service]
Type=simple
User=ubuntu
WorkingDirectory=/var/lib/teamx/projects/<team>        # owner workspace（含 owner .teamx）
Environment=TEAMX_HOME=/var/lib/teamx/projects/<team>/.teamx
Environment=TEAMX_SERVER_URL=https://95.40.221.159
ExecStart=/usr/local/bin/teamx tunnel expose teamx-ui --port 9527 \
  --mode local --team <team_id> --server https://95.40.221.159 --session <owner-session>
Restart=always
```
- 以该团队 **owner 身份**注册隧道 → 团队内 lead（co-lead）也受隧道 ACL 允许 forward（owner/lead 全通）。
- 远端 lead 访问：
  ```bash
  cd ~/…/<lead workspace>
  TEAMX_SERVER_URL=https://95.40.221.159 teamx tunnel forward teamx-ui --local-port 19527 --session <lead-session>
  # 浏览器打开（接受自签证书；token 每次重启变）：
  #   https://127.0.0.1:19527/?token=<从 ui 日志取>
  ```
- 取 token：`journalctl -u teamx-ui --no-pager | grep -o 'token=[0-9a-f]*' | tail -1`

### 10.4 参考现状（powernano-v2，2026-09-04）

- `teamx-ui.service` 用 `--session yongyan-pnv2-owner`（powernano-v2 owner）
- `tx-expose-teamx-ui.service` 暴露 9527，provider = powernano-v2 owner
- `/api/teams` → 仅 `powernano-v2`；访问旧团队返回 403
