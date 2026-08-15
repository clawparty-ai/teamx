# teamx 双窗口 Demo 手动测试指引（Manual Test）

> 本文件是 `docs/demo.md` 的配套测试 runbook：**"我"手动启动两个 opencode 窗口**，窗口 A 作为 team owner 创建团队并产出设计方案，窗口 B 作为 reviewer 加入并评审方案，全程通过 teamx `/Team` agent + `teamx_*` 工具协作。

## 0. 前置检查（30 秒）

```bash
which teamx          # 应输出 ~/.local/bin/teamx（插件靠 PATH 找到 CLI）
cd ~/github/teamx && ./tests/smoke.sh   # CLI 核心自检：全 PASS 说明账本/状态机没问题
```

- smoke 全过 → 问题只可能出在插件/模型层，排查范围缩小。
- 若 `/Team` 命令不存在 → 重跑 `./install.sh` 并重启 opencode。

## 1. 启动两个窗口

```bash
cd ~/github/teamx && ./demo/start.sh   # 打开两个 Terminal，各自进入 demo/workspace 并启动 opencode
```

（或手动：开两个终端，都 `cd ~/github/teamx/demo/workspace && opencode`。**两个窗口必须在同一目录**，才能共享 requirement.md / design-plan.md / review-plan.md。）

## 2. 窗口 A（owner）

### ① 创建团队 + 目标

输入：

```
/Team 创建一个团队，名字叫「看板设计团队」。目标：根据当前目录下的 requirement.md 需求，产出一份「轻量任务看板」的设计方案，写入 design-plan.md。
```

✅ 应看到：agent 调用 `teamx_create_team` → 返回 **invite_token**（复制备用）+ `teamx_set_goal`。

### ② 设计方案 + 广播

输入：

```
阅读 requirement.md，开始设计，完成后把方案写入 design-plan.md，然后用 teamx 广播"设计方案已完成，请 reviewer 评审"。
```

✅ 应看到：agent 用 read 读需求 → write 生成 design-plan.md → `teamx_publish decision`（提示已广播）。确认文件：`ls demo/workspace/` 应出现 `design-plan.md`。

## 3. 窗口 B（reviewer）

### ③ 加入 + 申请角色

输入：

```
/Team 加入团队，invite_token 是 <从窗口A复制的token>，我叫评审员，申请 reviewer 角色。
```

✅ 应看到：`teamx_join` → 提示 **pending，等待 owner 审批**；`teamx_set_role reviewer`。

## 4. 回到窗口 A

### ④ 审批 + 分享目标

输入：

```
/Team 审批新加入的成员，然后把目标分享给成员。
```

✅ 应看到：`teamx_approve`（成员变 active）+ `teamx_share_goal`（goal=shared，team=active）。

## 5. 回到窗口 B

### ⑤ 同步 + 评审 + 汇报

输入：

```
/Team 同步团队最新状态，然后读取 design-plan.md 进行评审，把改进意见写入 review-plan.md，并向团队汇报评审结论。
```

✅ 应看到：`teamx_sync` 返回 owner 的"方案完成"广播 → read design-plan.md → write review-plan.md → `teamx_publish progress`。

## 6. 回到窗口 A

### ⑥ 采纳 + 迭代 + 关闭

输入：

```
/Team 同步评审意见，阅读 review-plan.md，采纳合理的改进并更新 design-plan.md，用 teamx 广播处理结果，最后关闭目标。
```

✅ 应看到：sync → read review-plan.md → 更新 design-plan.md → `teamx_publish decision` → `teamx_close_goal`。

## 7. 最终验证（任意第三个终端）

```bash
# team_id 在窗口 A 建队时返回的输出里
teamx team status --team <team_id> --json
# 应看到 team.state=completed，goal.state=closed
teamx events --team <team_id> --json
# 事件链应包含（按 seq）：
# team.created → goal.set → membership.pending → member.role_set → membership.approved
#   → goal.shared → team.state_changed → decision.broadcast(方案完成)
#   → progress.published(评审) → decision.broadcast(处理结果)
#   → goal.state_changed(close) → team.completed
ls ~/github/teamx/demo/workspace/   # 应有 design-plan.md 和 review-plan.md
```

## 8. 常见问题排查

| 现象 | 原因 | 处理 |
|---|---|---|
| `/Team` 没有这个命令 | 安装后没重启 opencode | 重启后再试 |
| agent 找不到 teamx / "spawn ENOENT" | `~/.local/bin` 不在 PATH | `export PATH="$HOME/.local/bin:$PATH"` 后重启 opencode |
| 窗口 B 审批失败 "not pending" | 用了旧版 binary（未含审批修复） | 重跑 `./install.sh` |
| 窗口 B 看不到 owner 广播 | 还没 `teamx_sync`（V1 无推送，靠协议） | 让 B 的 agent 先 `teamx_sync`（提示词里已要求） |
| agent 停在权限确认 | 模型要读写文件/执行命令 | 在窗口里允许（Always/一次） |
| publish 报 data 非 JSON | 模型把 data 传成裸字符串 | 让它用 `{"message": "..."}` 格式，或忽略（核心流程不依赖 data） |

## 9. 测试记录

- 日期：____
- 结果：□ 全部通过　□ 部分通过（问题：__________）
- 事件链是否完整：□ 是　□ 否（差异：__________）
- 产物是否生成：design-plan.md □　review-plan.md □
