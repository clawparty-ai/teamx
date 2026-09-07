# Teamx 团队健康检查清单（server + member 节点）

> 用途：对**任意一支给定 team**，在「可 SSH 访问 server 与各 member 节点」的前提下，逐项判断 teamx server 与各 member 节点是否健康运行。
> 配套：`docs/39-clone-runtime-checklist.cn.md`（clone 结果核对）在 clone 后做一次；本文用于**日常巡检 / 故障定位 / 交接验收**，可随时重复执行。
> 蓝本：基于 powernano-v2 三节点（HK server + 东京/首尔/美国 member）实际巡检经验整理。
> 更新：2026-09-07

---

## 0. 前置：填写巡检对象

| 项 | 值 |
|---|---|
| Team 名称 / ID | ＿＿＿＿＿＿ ／ ＿＿＿＿＿＿＿＿ |
| Server 地址 / SSH | `https://＿＿＿＿＿＿` / `ssh -i <key> <user>@<server>` |
| Server 权威 DB | `＿＿＿＿/teamx.db`（如 `/var/lib/teamx/teamx.db`） |
| owner 会话名 | ＿＿＿＿＿＿（如 `yongyan-pnv2-owner`） |
| 节点清单 | 角色 / user / IP / SSH key 逐行填写（server、member1..n） |

```bash
KEY=<你的 pem>
SVR=<server 公网 IP>
SSH="ssh -i $KEY"
# 韩国等从本机超时的节点，可经 server 中转（把 key 放到 server 后 ssh -i key user@<member-ip>）
```

> 只读优先：本文所有命令**不改状态**；需要写操作（修 worker/端口等）见文末「处置指引」并先经 owner 确认。

---

## 一、Server 侧健康检查

### S1. server 进程与监听
```bash
$SSH <user>@$SVR 'ps aux | grep -E "teamx (serve|ui|tunnel)" | grep -v grep'
$SSH <user>@$SVR 'sudo ss -tlnp | grep -E ":(443|9527)"'
```
**期望**：
- 有 `teamx serve --port 443`（或你实际端口）持续在跑；
- 有 `teamx ui`（admin-console，可选）与 `tunnel expose teamx-ui`（可选）；
- 443 / 9527 有监听。
**判断点**：serve 没起 = 全队失联（member tunnel/WS 全断）；`Restart=always` 下反复重启要看日志。

### S2. serve 日志健康
```bash
$SSH <user>@$SVR 'sudo journalctl -u teamx.service -n 50 --no-pager'   # 按实际 unit 名
```
**期望**：无连续 panic / TLS 握手风暴 / DB 锁错误；错误率不随成员心跳增长。
**判断点**：出现大量 `mTLS` 拒绝 = 有成员证书吊销/失效；`database is locked` = 并发写异常。

### S3. 权威 DB 可读 + 团队存在
```bash
$SSH <user>@$SVR 'python3 - <<PY
import sqlite3
con=sqlite3.connect("file:<DB>?mode=ro", uri=True)
con.row_factory=sqlite3.Row
for r in con.execute("SELECT id,name,state FROM teams"): print(dict(r))
PY'
```
**期望**：目标 team 存在、`state` 符合预期（active / completed / archived）；无异常重复。
**判断点**：状态机异常（如 blocked 但无提问）需查 goal/events。

### S4. 团队状态（owner 视角）
```bash
$SSH <user>@$SVR "sudo teamx team status --json --session <owner-session> --team <TEAM-ID> --db <DB>"
```
**期望**：
- 成员全部 `active`，无卡在 pending；
- owner / co-lead 在位（`is_lead`）；
- 每成员有最近 activity 或在线标识；goal 状态符合预期。
**判断点**：成员 `idle` 正常；`waiting` = 有未答提问（看 questions）；`left` = 已退出。

### S5. 邀请函无异常
```bash
$SSH <user>@$SVR "sudo teamx team invite-list --json --session <owner-session> --team <TEAM-ID> --db <DB>"
```
**期望**：无「长期 unused 且本应已导入」的邀请；`revoked` 的确实要吊销。
**判断点**：unused 邀请堆积 = 有成员一直没上线（配合 S7 判断是否节点问题）。

### S6. 目标（goal）状态
```bash
$SSH <user>@$SVR "sudo teamx goal status --json --session <owner-session> --team <TEAM-ID> --db <DB>"   # 或从 S4 读
```
**期望**：goal 状态机与团队工作阶段一致（forming→shared→in_progress→achieved/closed）；无长期 stuck 在 blocked 而无人处理。

### S7. 成员在线连接（真实活动）
```bash
$SSH <user>@$SVR 'python3 - <<PY
import sqlite3
con=sqlite3.connect("file:<DB>?mode=ro", uri=True)
con.row_factory=sqlite3.Row
for r in con.execute("SELECT member_id, endpoint, ip, connected_at FROM member_connections WHERE team_id=? AND disconnected_at IS NULL", ("<TEAM-ID>",)):
    print(dict(r))
PY'
```
**期望**：每台常驻节点 ≥1 条 `tunnel` 在线；在线 agent/lead 应有 `ws` 连接；时间戳新鲜（非几天前）。
**判断点**：
- 某节点一条 tunnel 都没有 → 该节点隧道单元没起（对应节点 M7）；
- 全无 ws → 没有任何 agent/lead 在 opencode 里挂着（不一定异常，配合 worker 判断）。

### S8. 仓库健康（列表 + 权限 + 非空）
```bash
$SSH <user>@$SVR "sudo teamx git list --json --session <owner-session> --team <TEAM-ID> --db <DB>"
# 权限用 RPC 从带证书的 lead/owner 侧查：teamx git permissions <repo> --session <s> ...
# server 本地直接看 bare 是否有提交：
sudo git --git-dir=<TEAMX_HOME>/repos/<TEAM-ID>/<repo>.git for-each-ref
```
**期望**：仓库存在；关键成员有对应 read/write/admin；每个应工作的仓库非空（有 refs）。
**判断点**：某成员无法 clone/push = 先查仓库权限表（git_repo_permissions）是否漏授权（clone 之外手动建的仓库常漏人）。

---

## 二、Member 节点健康检查（对每台节点重复）

> 以单节点 user `<u>` 为例；三台/多台逐个把 `<u>` 与 IP 替换。clone 团队务必检查 **-pnv2（或对应 clone 后缀）** 整套单元。

### M1. 可达与身份
```bash
$SSH <user>@$NODE_IP 'hostname; whoami; ls /home/<u>/work*/.teamx/letters/'
```
**期望**：能登录；该 user 有 work 目录；`letters/<invitation_id>` 存在（含 ca.crt/client.crt/client.key）。
**判断点**：无 letter = 没 import；多个 work 目录对应多团队席位，逐个查不要串。

### M2. systemd 单元齐全且 active（每团队一套）
```bash
$SSH <user>@$NODE_IP "systemctl list-units --type=service --all | grep -iE 'opencode|ttyd|teamx-fs|tx-expose|teamx-worker'"
# 关键单元逐一点名检查（clone 场景要有 -pnv2 后缀版本）：
for u in opencode-serve-<u>-<clone> teamx-fs-<u>-<clone> ttyd-<u>-<clone> \
         tx-expose-<u>-<clone> tx-expose-<u>-<clone>-serve tx-expose-fs-<u>-<clone> \
         teamx-worker-<u>-<clone>; do echo "$u: $(systemctl is-active $u)"; done
```
**期望**：每团队应有一套完整单元且全部 `active (running)`；`activating/auto-restart`、`failed` = 异常。
**判断点**：auto-restart 常见根因 = **端口冲突**（两套服务配了同一端口）或 **二进制架构错误**（Exec format error）。

### M3. 端口唯一且不冲突（每团队错开）
```bash
$SSH <user>@$NODE_IP 'sudo ss -tlnp | grep -E ":(4197|4198|4199|4200|7681|7682|7683|...)"'
# 核对每单元实际端口：
systemctl show opencode-serve-<u>-<clone> ttyd-<u>-<clone> teamx-fs-<u>-<clone> -p ExecStart --value | tr ";" "\n" | grep argv
```
**期望**：每个本地端口只被一个进程监听；源团队与 clone 团队端口错开（如 4199 vs 4200、7681 vs 7682）。
**判断点**：`lws_socket_bind: ERROR on binding ... (98)` = 端口被占，两单元争同一端口（见过源 ttyd 与 pnv2 ttyd 都 7682 的情况）。

### M4. 每团队恰好一个 opencode serve + attach（无第二个内嵌 server）
```bash
$SSH <user>@$NODE_IP "ps -eo pid,ppid,cmd | grep -E 'opencode (serve|attach)' | grep -v grep"
```
**期望**：每个团队恰好 1 个 `opencode serve --port <X>` + ttyd 里 1 个 `opencode attach http://127.0.0.1:<X> -c`。
**判断点**：出现两个 serve = 有人用 `opencode -c` 起了内嵌 server（历史不同步）；同一端口两个 serve = 立即冲突。

### M5. 日志无致命错误
```bash
$SSH <user>@$NODE_IP "sudo journalctl -u opencode-serve-<u>-<clone> --no-pager -n 30"
$SSH <user>@$NODE_IP "sudo journalctl -u teamx-worker-<u>-<clone> --no-pager -n 30"
```
**期望**：opencode serve 无崩溃；worker 日志周期出现 `nothing actionable` 或 `would run opencode ...`。
**判断点（重点）**：若出现
```
error: teamx: no opencode sessionID available in this tool context
  at sessionKey (.../plugins/teamx.js)
```
说明 **headless 环境里插件缺 opencode 会话上下文**——agent 看似在跑但不干活；worker 是让节点自动推进任务的关键（无 worker 的团队，任务派下去没人认领）。

### M6. worker 归属正确（clone 团队必须独立 worker）
```bash
$SSH <user>@$NODE_IP "systemctl show teamx-worker-<u>-<clone> -p Environment --value | tr ' ' '\n' | grep TEAMX_HOME"
```
**期望**：worker 的 `TEAMX_HOME` 指向**对应团队**的 workspace（如 `/home/<u>/work-<clone>/.teamx`），且 `--opencode-url` 指向该团队 serve 端口。
**判断点**：所有 worker 都指向旧 `work/.teamx` = clone 团队没有自己的 worker，新任务没人自动做（powernano-v2 踩过）。

### M7. 工作区有正确代码 checkout（G2）
```bash
$SSH <user>@$NODE_IP "sudo -u <u> bash -c 'cd /home/<u>/work-<clone>/<repo> && git log --oneline -1 && git status -s | head && git remote -v'"
```
**期望**：checkout 的 HEAD 与 server 仓库一致；工作区 clean 或只有进行中改动；remote 指向 `teamx::<TEAM-ID>/<repo>`（或 smart HTTP URL）。
**判断点**：
- 有文件但无 `.git` = 手工拷贝，不是 teamx clone（无法 pull/push，需重 clone）；
- HEAD 落后 server = 需要 pull。

### M8. 拉取链路可用
```bash
$SSH <user>@$NODE_IP "sudo -u <u> bash -c 'export TEAMX_HOME=/home/<u>/work-<clone>/.teamx; cd /home/<u>/work-<clone>/<repo> && teamx git pull <repo> --session <u>-<clone> --server https://$SVR --team <TEAM-ID>'"
```
**期望**：返回 `pulled main: Already up to date` 或正常合并；无权限/连接错误。
**判断点**：`read permission required` = 仓库漏授权（回 S8 补）；连接失败 = letter/server-url 问题。

### M9. 隧道注册在线（节点侧看是否还活着）
```bash
$SSH <user>@$NODE_IP "sudo ss -tlnp | grep -E ':(<serve-port>|<fs-port>|<ttyd-port>)'"
# 远端核对：server S7 的 member_connections 有该节点 tunnel
```
**期望**：本地端口在听；server 侧能看到该节点 tunnel 长连接。
**判断点**：本地在听但 server 无 tunnel = tunnel expose 进程没起来/证书失效/重连中。

---

## 三、端到端（lead/owner 侧）

### E1. tunnel 可 forward
```bash
# 在 lead/owner 的该团队 workspace（有该团队 letter）执行：
teamx tunnel forward <tunnel-name> --local-port <port> --session <lead>-<clone> --server https://$SVR --team <TEAM-ID>
```
**期望**：`ok forward ... listening on 127.0.0.1:<port>`。

### E2. headless/Web 可达
```bash
curl -k http://127.0.0.1:<local-port>/api/session   # 或 opencode Web API
```
**期望**：返回 JSON（含会话），非空/拒绝。

### E3. 代码闭环（可选，验证可写）
```bash
# 某节点在 <repo> 内 commit → push；lead pull；两边 HEAD 一致
```
**期望**：push/pull 成功，无权限拒绝。

### E4. 任务能流转（最接近「团队是否真的能干活」）
- server 上有分派任务（taskx/doc）时，目标成员 active 且其 worker 在线；
- 观察 events：目标成员出现 `doc.acknowledged` / `doc.claimed` / `progress.published`。
**期望**：任务不再长期停在「未认领」。
**判断点**：任务一直没人碰 = 该成员没起 worker（M6）或 worker 报 sessionID 错（M5）。

---

## 四、结论模板

- Server：S1–S8 通过 ＿＿/8
- Member 节点：M1–M9 每台 通过 ＿＿/9（节点1/2/3 …）
- 端到端：E1–E4 通过 ＿＿/4
- 健康结论：✅ 健康 / ⚠️ 有异常 / ❌ 不健康
- 异常项与处置：＿＿＿＿＿＿＿＿

---

## 五、常见故障快速定位（处置需 owner 确认）

| 现象 | 根因 | 处置 |
|---|---|---|
| 全员失联 | server serve 挂了 / 网络 | 起 serve；查 journal |
| 某节点无 tunnel | 节点隧道单元没起 | 起 `tx-expose-*` 系列 |
| 某单元 auto-restart | 端口冲突（多团队同端口）/ 架构错 | 端口 +k 错开；换 arm64 二进制 |
| `no opencode sessionID` | headless 插件无会话上下文 | 确认 worker 正确指向该 workspace + serve 端口 |
| 任务无人认领 | 该成员无独立 worker / worker 指错 workspace | 补 `teamx-worker-<user>-<clone>`，`TEAMX_HOME` 指对目录 |
| clone 拉取权限拒绝 | clone 外手动建的仓库漏授权 | owner/admin `git grant <repo> <member> --permission write` |
| 工作区无 .git | 手工拷文件而非 teamx clone | 备份后 `teamx git clone <repo>` |
| member 显示 waiting | 有未答提问 | lead/owner 看 questions 并 respond/ask |
| DB locked / 写异常 | 并发或 WAL 问题 | 查 serve 日志；必要时重启 serve（owner 确认） |

---

## 相关文档
- `docs/39-clone-runtime-checklist.cn.md`（clone 后一次性核对）
- `docs/38-clone-runtime-design.cn.md`（clone 运行设计，含 G1/G2）
- `docs/35-manual-member-cloud-node.cn.md`（member 云节点部署 / systemd / 隧道模板）
- `docs/30-cloud-node-opencode-headless-tui.cn.md`（headless server + ttyd 架构）
