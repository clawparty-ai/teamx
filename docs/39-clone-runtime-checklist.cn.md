# Clone 结果检查清单（Checklist）

> 用途：执行 `team clone` 并完成 A/B 两阶段接入后，逐项核对 clone 是否正确、节点是否真正可开工。
> 使用方式：每项按「命令」执行，对照「期望」打勾 ✅ / ❌；所有 ❌ 需在「备注」写明原因并处理。
> 配套：`docs/38-clone-runtime-design.cn.md`（完整运行设计）。更新：2026-09-07。

## 填写表头
- Clone 名：`<clone-name>` ｜ Clone Team ID：`<CLONE-TEAM-ID>`
- 源 Team ID：`<SRC-ID>` ｜ Server：`https://95.40.221.159`
- 执行人 / 日期：＿＿＿＿＿＿

---

## 一、Server 侧（owner/lead，在 owner 的 clone workspace 执行）

| # | 检查项 | 命令 | 期望 | ✅/❌ | 备注 |
|---|---|---|---|---|---|
| S1 | 团队成员就位 | `teamx team status --session <owner>-clone --team <CLONE-TEAM-ID>` | 成员数与源一致，全部 `active`；owner 座位存在 | | |
| S2 | 角色目录 | 同 status / `role` 列表 | 内置角色 + 源自定义角色齐全 | | |
| S3 | 邀请函已认领 | `teamx team invite-list --session <owner>-clone --team <CLONE-TEAM-ID>` | 无遗留 `pending/unused`（owner 之外每成员一张已用） | | |
| S4 | 仓库已建 | `teamx git list --session <owner>-clone --team <CLONE-TEAM-ID>` | 每个源仓库有同名仓库 | | |
| S5 | 仓库非空（G1 完成） | server 机 `git --git-dir <TEAMX_HOME>/repos/<CLONE-TEAM-ID>/<name>.git for-each-ref` | `refs/heads/main` 存在；或成员首次 push 已完成 | | |
| S6 | 仓库权限 | `teamx git permissions <repo> --session <owner>-clone --team <CLONE-TEAM-ID>` | owner ADMIN、成员 WRITE | | |
| S7 | 目标可派发 | `teamx goal set "<goal>" --session <owner>-clone --team <CLONE-TEAM-ID>`（或 taskx） | goal 可设置 / 分享 | | |

## 二、每台 Member 云主机（逐台填写：东京 zhongm / 首尔 zhaoz / 美国 caicai）

节点 IP / user：＿＿＿＿＿＿ ｜ clone workspace：`/home/<user>/work-<clone-name>` ｜ 端口基线 +k=＿＿

| # | 检查项 | 命令 | 期望 | ✅/❌ | 备注 |
|---|---|---|---|---|---|
| M1 | workspace 独立 | `ls -ld /home/<user>/work /home/<user>/work-<clone-name>` | 两个目录；clone 目录含 `.teamx/letters/<id>` | | |
| M2 | 身份已导入 | `sudo -u <user> sh -c 'cd /home/<user>/work-<clone-name> && teamx team status'` | 显示 clone 团队、成员 `active` | | |
| M3 | 邀请函单次使用 | 再次 import 同一 letter | 报错/失效（属正常） | | |
| M4 | 仅一个 opencode serve | `ps -eo pid,ppid,cmd \| grep -E "opencode" \| grep -v grep` | 一个 `serve`（port 4199+k）+ 一个 `attach`（ttyd） | | |
| M5 | 端口监听 | `sudo ss -tlnp \| grep -E ':(4199\+k\|4197\+k\|7681\+k)'` | 三端口监听，且属于本 clone workspace 进程 | | |
| M6 | 服务 active | `systemctl status opencode-serve-<user>-clone ttyd-<user>-clone teamx-fs-<user>-clone tx-expose-*` | `active (running)`，非 restart 循环 | | |
| M7 | 隧道在线 | `teamx tunnel list --session <user>-clone --team <CLONE-TEAM-ID>` | `opencode-<user>-clone / ttyd-<user>-clone / fs-<user>-clone` 在线 | | |
| M8 | 工作区有代码（G2） | `ls /home/<user>/work-<clone-name>/<repo-name>` | checkout 存在；`git log --oneline -1` 与 server 基线一致 | | |
| M9 | 拉取链路（日常） | `cd <repo-name> && teamx git pull <repo-name> --session <user>-clone --team <CLONE-TEAM-ID>`（或原生 `git pull`） | 无冲突、返回 updated/up-to-date | | |

## 三、端到端（lead/owner 在 clone workspace 执行）

| # | 检查项 | 命令 | 期望 | ✅/❌ | 备注 |
|---|---|---|---|---|---|
| E1 | 成员真实在线 | `teamx team status --session <lead>-clone --team <CLONE-TEAM-ID>` | 三成员在线/连接正常 | | |
| E2 | 隧道可 forward | `teamx tunnel forward opencode-zhongm-clone --local-port 14200 …`（zhaoz/caicai 同理） | 三条 forward ok | | |
| E3 | headless 可达 | `curl http://127.0.0.1:<local>/api/session` | 返回 JSON，非空/拒绝 | | |
| E4 | TUI 可达 | 浏览器开 `ttyd-<user>-clone` forward | 出现 opencode TUI | | |
| E5 | 代码闭环 | 任选一节点在 `<repo-name>` 内 commit→push；lead `git pull` | push/pull 成功、commit 一致 | | |

## 四、结论
- 通过项：＿＿/＿＿
- 未通过项与处理：＿＿＿＿＿＿＿＿＿＿＿＿
- 是否可开工：✅ / ❌（❌ 则注明卡在哪一环：owner 未激活 / letter 缺失 / 服务未起 / 仓库空 / 未拉取）

---

## 常见 ❌ 快速定位
| 现象 | 原因 | 处理 |
|---|---|---|
| 成员 absent / pending | letter 未 import，或 import 在旧目录 | 重新在 clone workspace import；letter 烧掉则 owner 补发 |
| `opencode-<user>-clone` 隧道不存在 | 只 import 未起服务/隧道 unit | 起 6 个 systemd unit |
| 隧道在但 curl 空 | 节点 clone 的 opencode serve 没监听/起错 workspace | `journalctl -u opencode-serve-<user>-clone`；查端口 |
| serve 反复 restart | 端口冲突 / 架构错 / letter 缺失 / TEAMX_HOME 错 | 看 journal；`ss` 查端口 |
| 仓库 pull 空 / 无文件 | A5 未 seed 且无人 push（G1） | 先完成 A5 或首个成员 push |
| 工作区无 repo 目录 | B4 未执行（G2） | 节点上 `teamx git clone` |
| 成员 import 后连不上（`certificate signature failure` / TLS alert 51） | clone 时 `TEAMX_HOME` 用了旧 home，证书由旧 CA 签发 | 比对 `ca_fingerprint`；按 `docs/59-incident-clone-wrong-teamx-home.cn.md` 补签 / 重 clone / RPC 重发 |
| `git_repos.path` 指向 `/home/<user>/.teamx/repos/…` | 同上，clone 落错 home | 移到 `/root/.teamx/repos/` 并同步 DB path |
