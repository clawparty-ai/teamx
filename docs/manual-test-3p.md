# teamx 三人协作手动测试（owner + contributor + reviewer）

> 本文是**可一步步照着执行**的测试 runbook（对应测试用例 **TM-04**）。你用三个 opencode 窗口扮演三个成员，走完「建队 → 加入 → 审批 → 设计 → 评审 → 澄清 → 定稿 → 归档」全流程，并在最后用 CLI 验证账本。

## 0. 前置条件

1. 已执行 `./install.sh`（Rust CLI + 插件三件套已装），并**重启过 opencode**（`/Team` 命令生效）。
2. 验证命令可用：
   ```bash
   which teamx          # 应输出 ~/.local/bin/teamx
   teamx --version      # 能打印版本号
   ```
3. 三窗口启动（都在同一个 `demo/workspace` 目录）：
   ```bash
   cd ~/github/teamx && ./demo/start.sh 3
   ```
   打开三个 Terminal：**窗口 A=owner，B=contributor（设计者），C=reviewer（评审员）**。
   三者共享 `~/.teamx/teamx.db`（全局库）与 `demo/workspace/` 下的文件。

## 1. 测试步骤（严格按顺序，三窗口交错）

### 步骤 1｜窗口 A：创建团队 + 目标

输入：

```
/Team 创建一个团队，名字叫「产品评审组」。目标：根据当前目录 requirement.md 完成「轻量任务看板」产品方案设计，并由 reviewer 评审定稿。
```

✅ 预期：
- agent 调用 `teamx_create_team`，回复里出现 **invite_token**（一串 32 位字符）——**复制备用**。
- 目标是 `proposed`（草拟中），你是 owner。

> 记录：team id = ________，invite_token = ________

### 步骤 2｜窗口 B：加入 + 申请 contributor

输入（把 `<token>` 换成步骤 1 的 token）：

```
/Team 加入团队，invite_token 是 <token>，我叫设计者，申请 contributor 角色。
```

✅ 预期：`teamx_join` → 提示 **pending，等待 owner 审批**；`teamx_set_role contributor`（仍 pending）。

### 步骤 3｜窗口 C：加入 + 申请 reviewer

输入：

```
/Team 加入团队，invite_token 是 <token>，我叫评审员，申请 reviewer 角色。
```

✅ 预期：`teamx_join` → pending；`teamx_set_role reviewer`（仍 pending）。

### 步骤 4｜窗口 A：审批两人 + 分享目标

输入：

```
/Team 审批所有待审批的成员，然后把目标分享给成员。
```

✅ 预期：`teamx_approve` × 2（成员变 active、角色保留）+ `teamx_share_goal`（team=active，goal=shared）。

### 步骤 5｜窗口 B：设计方案 + 汇报

输入：

```
/Team 同步团队状态，阅读 requirement.md 完成设计方案写入 design-plan.md，然后向团队汇报「设计方案完成，请评审」。
```

✅ 预期：`teamx_sync` → 读需求 → 生成 `design-plan.md` → `teamx_publish progress`。
文件确认：`ls demo/workspace/` 应出现 `design-plan.md`。

### 步骤 6｜窗口 C：评审 + 汇报

输入：

```
/Team 同步团队状态，读取 design-plan.md 进行评审，把改进意见写入 review-plan.md，然后向团队汇报「评审完成」。
```

✅ 预期：`teamx_sync` 能看到 B 的 progress → 读方案 → 生成 `review-plan.md` → `teamx_publish progress`。

### 步骤 7｜窗口 A：澄清 + 采纳

输入：

```
/Team 同步进展，向设计者提一个澄清问题，得到答复后采纳评审意见并广播处理结果。
```

✅ 预期：`teamx_sync` → `teamx_ask`（设计者变 waiting，得到 question id）→ **等 B 先回答**（此时可能提示"等待成员答复"）。

### 步骤 8｜窗口 B：答复 + 报告完成

输入：

```
/Team 同步状态，回答 owner 的澄清问题，然后报告目标已达成。
```

✅ 预期：`teamx_respond`（回到 active）→ `teamx_publish achieved`（goal=achieved）。

### 步骤 9｜窗口 A：关闭 + 归档

输入：

```
/Team 验证并关闭目标，然后归档团队。
```

✅ 预期：`teamx_close_goal`（team=completed）→ `teamx_archive`（team=archived）。

## 2. 验证（任意第三个终端）

把 `<team_id>` 换成步骤 1 记录的 team id：

```bash
# 1) 状态：应为 archived / closed
teamx team status --team <team_id> --json

# 2) 审计回放（成员名已解析）
teamx log --team <team_id>

# 3) 产物
ls ~/github/teamx/demo/workspace/   # 应有 requirement.md / design-plan.md / review-plan.md
```

✅ 预期事件链（`teamx log` 按 seq 递增，顺序应大致为）：

```
team.created → goal.set → membership.pending ×2 → member.role_set ×2
→ membership.approved ×2 → goal.shared → team.state_changed
→ progress.published(设计) → progress.published(评审)
→ clarification.asked → clarification.responded
→ decision.broadcast(采纳) → goal.achieved
→ goal.state_changed(close) → team.completed → team.state_changed(archive)
```

## 3. 常见问题排查

| 现象 | 原因 | 处理 |
|---|---|---|
| `/Team` 不是命令 | 装完没重启 opencode | 重启 opencode |
| agent 说找不到 teamx / spawn 失败 | `~/.local/bin` 不在 PATH | `export PATH="$HOME/.local/bin:$PATH"` 后重启 |
| 步骤 4 审批失败 "not pending" | 用了旧 binary | 重跑 `./install.sh` |
| B/C 看不到对方的进展 | 没 `teamx_sync`（V1 无推送，靠协议） | 让对应窗口 agent 先 `teamx_sync` |
| agent 停在权限确认 | 模型要读写文件/执行命令 | 在窗口里点允许（Always） |
| publish 报 data 非 JSON | 模型传了裸字符串 | 忽略即可（已自动兜底为 message） |
| owner 无法 `leave` | 有意限制（防团队孤儿） | 用 `teamx_close_goal` + `teamx_archive` 收尾 |

## 4. 测试记录

- 日期：________
- 执行人：________
- 结果：□ 全部通过　□ 部分通过（失败步骤：________）
- 终态：team=________ / goal=________（预期 archived / closed）
- 事件链完整：□ 是　□ 否（差异：________）
- 产物：design-plan.md □　review-plan.md □

---

> 等价自动化验证：不依赖真实模型、跑通同一事件链的脚本是 `tests/three-member.sh`（`./tests/three-member.sh` 直接运行）。想先确认底层逻辑没问题，可先跑它再走本 runbook。
