# teamx 团队 Clone：完整运行设计（含 repo 初始化与节点拉取）

> 定位：`34-team-clone.cn.md` 是实现/Feature 视角；本文是 **clone 之后到「成员能开工」的完整运行设计**（操作者视角）。
> 补齐两个现状缺口：①clone 只建空仓库、**没有初始化 repo 内容**；②clone 后**各 member 云主机没有把 repo 拉到工作区**。
> 目标读者：owner / co-lead / 节点运维（部署 powernano-v2 这类 clone 团队的人）。
> 更新：2026-09-07

---

## 0. 本文回答的问题

1. clone 到底在 server 上做了哪些事、没做哪些事？
2. 要让一支 clone 团队「真正能开工写代码」，从 server 到每台 member 云主机还需要哪些操作？
3. 每一步怎么做、预期结果是什么、怎么验证？

---

## 1. 工作原理（clone 建了什么、没建什么）

### 1.1 clone 在 server 上做的事（一次命令，事务内）

| 对象 | clone 结果 |
|---|---|
| team | 新 team（`state=forming`、`owner_member_id`=新 owner 座位、新 `team_id`） |
| roles | 复制源团队全部 approved 自定义角色 + `seed_roles` 补齐 6 个内置角色 |
| members | **不插 member 行**，改为逐成员 mint 邀请函（证书 CN=`member:<id>:<role>`，`auto_approve=true`） |
| 仓库 | 为源每个 git 仓库建**同名空 bare repo**（`~/.teamx/repos/<clone_team>/<name>.git`），**内容为空、不预授权限** |
| 事件/任务/提问/隧道/goal/旧邀请 | **全部不复制**（完全独立） |

### 1.2 clone 在 server 上没做的事（当前缺口）

| 缺口 | 现状 | 后果 |
|---|---|---|
| **G1 repo 内容初始化** | 空 bare repo 无任何 commit | 成员拉到的仓库是空的，无法开工 |
| **G2 每节点拉取 repo** | clone 交付只 import 邀请函 + 起服务 | 节点工作区只有 `.teamx`，没有代码 checkout |

### 1.3 身份与仓库通道

- **一个 workspace = 一个团队席位**：`/home/<user>/work-<clone>/.teamx/letters/<id>` 是身份；`teamx` CLI / git 客户端从该目录向上发现 `.teamx/letters` 取证书。
- 仓库存取双通道：
  - **RPC bundle**：`teamx git clone/pull/push`（bundle base64 over mTLS JSON-RPC）。
  - **Smart HTTP**：`teamx git setup` 把 mTLS 证书写进 `~/.gitconfig` 后，原生 `git clone/pull/push https://<server>/git/<team_id>/<repo>`。
- 权限（import auto-approve 时自动授予）：clone 仓库 **owner → ADMIN，其余成员 → WRITE**（都能 read/push）。repo 管理员可 `git grant` 调整。

> 注意：`git.bundle` 只服务有 commit 的仓库；空仓库返回 `empty=true`，客户端 `git init -b main` 起一个空本地仓库——这是**初始化 repo 的起点**，不是 bug。

---

## 2. 总流程（两阶段）

```
阶段 A  server 侧（owner/lead）
  A1 前置校验 → A2 dry-run 预览 → A3 执行 clone（返回每张 letter）
  A4 owner 在自己的 clone workspace 导入激活
  A5 【G1 补齐】初始化每个 repo 的初始内容（seed push）

阶段 B  member 云主机侧（每台节点，逐台做）
  B1 建独立 workspace → B2 import 该成员的 clone letter（认领 active）
  B3 起本 clone 的独立进程组（opencode-serve/ttyd/fs + 3×隧道，端口错开）
  B4 【G2 补齐】把 repo 拉到该节点工作区
  B5 验证（本机端口 + server 侧成员/隧道 + lead 端到端）
```

---

## 3. 阶段 A：Server 侧步骤与预期

> 在 server 机（HK `95.40.221.159`）含源团队 owner `.teamx` 的目录执行；网络模式请带 `--session <owner>`。

### A1 前置校验
```bash
teamx team status --session <owner-session>
teamx git list   --session <owner-session>
```
**预期**：你在源团队且为 owner/co-lead；记下源 `team_id` 与仓库清单。
**判断点**：源 owner 必须 `active`，否则 clone Err。

### A2 dry-run（不落库）
```bash
teamx team clone --name <clone-name> --team <SRC-ID> --session <owner-session>
```
**预期**：返回预览摘要（源/目标/成员数/角色数/repo 数），**DB 无变化**。
**判断点**：成员数 = 期望人数；名字唯一。

### A3 执行 clone
```bash
teamx team clone --name <clone-name> --team <SRC-ID> --yes \
  --session <owner-session> --server-url https://95.40.221.159
```
**预期**：
```
ok team:{id:<CLONE-TEAM-ID>, state:forming}
members:[ {display_name, role, is_lead, is_owner, letter:"teamx-inv:v1:…"}, … ]
repos:[{source:<src>, name:<同名>}, …]
```
**判断点**：
- 新 `CLONE-TEAM-ID` 记下（后续全部 `--team` 用它）。
- 每张 letter 原样保存分发；**缺一张少一个席位**。
- 若 CLI 在 serve 机本地跑且未传 `--server-url`，letter 内可能写 127.0.0.1 → 节点连不上。

### A4 owner 激活 clone（另开 workspace，不复用源团队 .teamx）
```bash
mkdir -p <parent>/<clone-name> && cd <parent>/<clone-name>
teamx team import "<owner letter>" --session <owner>-clone
teamx team status --session <owner>-clone --team <CLONE-TEAM-ID>
```
**预期**：import 返回 `active`（auto_approve）；status 显示新团队 + owner 座位 + 空仓库列表，仓库权限为 ADMIN。
**判断点**：owner 未激活前团队一直 `forming`；先解决这里再碰节点。

### A5【G1 补齐】初始化 repo 内容（seed push）
clone 仓库是空的，**需要把初始代码/基线放进去**。以「把一份已有代码变成初始内容」为例：

```bash
# 1) 在临时目录 clone 空仓库（得到带 teamx origin 的本地仓库，默认分支 main）
mkdir -p <seed-ws> && cd <seed-ws>
teamx git clone <repo-name> --session <owner>-clone --team <CLONE-TEAM-ID>
#    预期：cloned into <repo-name>；空仓库 → 本地已 init -b main

cd <repo-name>
# 2) 放入初始内容：拷贝文件（或从已有 git 仓库拉内容，例如）
#    git remote add seed <path-to-existing-repo> && git fetch seed && git merge seed/main
cp -R <path-to-baseline>/* .
git add -A
git commit -m "seed: initial baseline for <clone-name>"

# 3) push 回 server 空仓库（写权限 → git.receive 收 bundle）
teamx git push <repo-name> --session <owner>-clone --team <CLONE-TEAM-ID>
#    预期：pushed
```

**判定 G1 是否完成**：
```bash
# server 机直接看 bare repo 是否有 ref（只读）
git --git-dir <TEAMX_HOME>/repos/<CLONE-TEAM-ID>/<repo-name>.git for-each-ref
# 预期输出有 refs/heads/main（不再是空）
```
- 若新团队**不需要旧代码**、要从零起仓库：可跳过 A5，让成员直接在各自 checkout 里写第一版再 push（首个 push 即完成初始化）。
- 若**想把源团队某个仓库现有内容作为起点**：因 clone 不复制内容，须手动「源 clone 一份 → 推送到 clone 空仓库」（或 `git remote add src <源仓库路径> && git pull src main` 后 push）。

---

## 4. 阶段 B：Member 云主机侧步骤与预期

> 每台节点一套。节点：东京 `zhongm`(35.77.5.99) / 首尔 `zhaoz`(3.37.16.165) / 美国 `caicai`(184.34.85.90)。
> 以任一 `<user>` 为例，三台一致，只是 user/letter/端口/隧道名不同。

### B1 建独立 workspace（不复用源团队目录）
```bash
sudo mkdir -p /home/<user>/work-<clone-name> && sudo chown <user> /home/<user>/work-<clone-name>
```
**判断点**：源团队 `work` 与 clone `work-<clone-name>` 必须分开；身份混队会出错。

### B2 import clone 邀请函
```bash
sudo -u <user> sh -c 'cd /home/<user>/work-<clone-name> && \
  teamx team import "$(cat /tmp/<user>-clone.letter)" --session <user>-clone'
```
**预期**：`.teamx/letters/<id>/` 生成 `ca.crt/client.crt/client.key`；letter 单次失效。
**判断点**：真正 active 在「与 server 建立连接 / RPC 认领」时生效（auto_approve）；若曾在旧目录误 import 烧掉 letter，需 owner 补发/重新 clone。

### B3 起本 clone 的独立进程组
端口基线（源团队已占 `4199/4197/7681`），clone 组**整体 +k 错开**（先 `sudo ss -tlnp` 确认空闲再定 k）。unit 模板见 `docs/35-manual-member-cloud-node.cn.md` §4，关键点：

- 目录/身份：`WorkingDirectory=/home/<user>/work-<clone-name>`、`User=<user>`、`Environment=HOME=/home/<user>`
- opencode serve：`opencode serve --hostname 127.0.0.1 --port <4199+k>`（**每 clone 唯一 server**）
- ttyd：`opencode attach http://127.0.0.1:<4199+k> -c`（attach 同一 server，勿再 `opencode -c`）
- fs / 隧道：`TEAMX_HOME=/home/<user>/work-<clone-name>/.teamx`、`TEAMX_SERVER_URL=https://95.40.221.159`、`--session <user>-clone`
- 隧道名：`opencode-<user>-clone / ttyd-<user>-clone / fs-<user>-clone`，`--mode local`

启动：
```bash
sudo systemctl daemon-reload
sudo systemctl enable --now opencode-serve-<user>-clone ttyd-<user>-clone teamx-fs-<user>-clone \
  tx-expose-<user>-clone tx-expose-<user>-clone-serve tx-expose-<user>-clone-fs
```

### B4【G2 补齐】把 repo 拉到该节点工作区
身份用该 workspace 的 `.teamx`（在 `work-<clone-name>` 下执行，CLI 会向上发现 letters）：

```bash
sudo -u <user> sh -c 'cd /home/<user>/work-<clone-name> && \
  teamx git clone <repo-name> --session <user>-clone --team <CLONE-TEAM-ID>'
```
**预期**：`cloned into <repo-name>`；目录内容 = A5 初始化的基线（若 A5 已 seed）。
**替代（原生 git over mTLS）**：
```bash
sudo -u <user> sh -c 'cd /home/<user>/work-<clone-name> && \
  teamx git setup --server https://95.40.221.159 && \
  git clone https://95.40.221.159/git/<CLONE-TEAM-ID>/<repo-name>'
```
**判断点**：节点工作区现在应有 `repo-name/` 代码目录，opencode 后续 Add project 选它（或 `WorkingDirectory` 指向它）。

> 每台节点都要 B4；只有一台拉了不算完成。后续日常同步在 `repo-name/` 内 `teamx git pull <repo-name>`（或原生 `git pull`）。

---

## 5. 验证方法（分三层）

### 5.1 Server 侧状态
| 检查 | 命令 | 期望 |
|---|---|---|
| 团队 | `teamx team status --session <owner>-clone --team <CLONE-TEAM-ID>` | 成员全部 active；goal 可设可 share |
| 邀请函 | `teamx team invite-list --session <owner>-clone --team <CLONE-TEAM-ID>` | 已用（非 pending） |
| 仓库列表 | `teamx git list --session <owner>-clone --team <CLONE-TEAM-ID>` | 同名仓库存在 |
| 仓库内容（G1） | server：`git --git-dir <TEAMX_HOME>/repos/<CLONE-TEAM-ID>/<name>.git for-each-ref` | 有 `refs/heads/main`（非空） |
| 仓库权限 | `teamx git permissions <repo> --session <owner>-clone --team <CLONE-TEAM-ID>` | owner ADMIN、成员 WRITE |

### 5.2 节点本机
| 检查 | 命令 | 期望 |
|---|---|---|
| 进程 | `ps -eo pid,ppid,cmd \| grep opencode \| grep -v grep` | 一个 `serve` + 一个 `attach` |
| 端口 | `sudo ss -tlnp \| grep -E ':(4199\+k\|4197\+k\|7681\+k)'` | 三个都在监听 |
| 服务状态 | `systemctl status opencode-serve-<user>-clone ttyd-<user>-clone …` | active（不是 restart 循环） |
| 工作区代码（G2） | `ls /home/<user>/work-<clone-name>/<repo-name>` | 与 server 基线一致；`git log` 与 A5 commit 相同 |

### 5.3 隧道与端到端（lead / owner 在 clone workspace）
| 检查 | 命令 | 期望 |
|---|---|---|
| 隧道在线 | `teamx tunnel list --session <lead>-clone --team <CLONE-TEAM-ID>` | 3 节点 × 3 条在线 |
| 端到端 | `teamx tunnel forward opencode-zhongm-clone --local-port 14200 …; curl http://127.0.0.1:14200/api/session` | 返回 JSON |

---

## 6. 现状缺口与建议（为什么需要本文）

| # | 缺口 | 现状 | 建议补齐点 | 状态 |
|---|---|---|---|---|
| G1 | repo 未初始化内容 | clone 只建空 bare repo | 阶段 A5（seed push）或首个成员 push | 流程补齐（本文） |
| G2 | 节点未拉取 repo | clone 交付不含 checkout 步骤 | 阶段 B4（每节点 clone/pull 到工作区） | 流程补齐（本文） |

可选的自动化方向（后续功能）：clone 时新增 `--seed-repo <src-url>`（把源仓库/外部仓库内容一并复制进空仓库）与「clone-node 脚本内自动 git clone」。当前以本文 A5/B4 手工补齐为准。

---

## 7. 相关文档

- `docs/34-team-clone.cn.md`（clone Feature 实现/测试）
- `docs/35-manual-member-cloud-node.cn.md`（member 云节点部署、systemd/隧道模板）
- `docs/26-manual-cloud-host-best-practice.cn.md`（server+member 最佳实践、git 双通道）
- `docs/30-cloud-node-opencode-headless-tui.cn.md`（headless server + ttyd 架构）
- 交付脚本参考：`scripts/teamx-clone-node.sh`（如存在，需补 B4）
