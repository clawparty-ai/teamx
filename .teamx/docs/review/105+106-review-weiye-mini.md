# task-103-adjacent 评审 — issue #102 收尾：PR #105（dsh 侧）+ PR #106（opencode 侧）inbox-wake

> 评审人：weiye-mini（独立会话；#105 作者=weiye 主会话，#106 作者=harness 会话，reviewer≠author 成立）· 2026-09-25
> 对象：`fix/issue-102-inbox-wake`（#105，单 commit 476b69c，dsh-plugin 4 文件 +299）· `fix/issue-102-inbox-wake-opencode`（#106，2 commits 7504e63+bcb9bfc，6 文件 +351）
> 评审方式：两分支 diff 全读 + inbox-wake.ts 逐字对照（diff 确认仅注释差异）+ **两份单测本机 bun 实跑（各 24 断言，全绿）** + 落后度/夹带检查

## Verdict

- **PR #105（dsh-plugin 侧）：PASS**（2 条非阻塞建议）
- **PR #106（opencode-plugin 侧）：CHANGES-REQUESTED（1 必改，合并机制层面；opencode 实质代码 PASS）**

---

## 1. 共同 PASS 面（两 PR 的 inbox-wake 决策层逐条核对）

| 检查项 | 结论 | 证据 |
|---|---|---|
| seq 游标去重 | ✅ | `selectInboxWake`：`seq > state.seq` 才 wake；同 seq 幂等不重复（测试块 3） |
| 首轮基线不回放 | ✅ | `firstCallIsBaseline` 只采 max seq + 标记 questionIds，不 wake——重启无积压风暴（测试块 1） |
| question id 去重 | ✅ | questionIds Set，同 id 只 wake 一次（测试块 4） |
| 畸形条目健壮性 | ✅ | seq 非 number→0、text 非 string→''、无 id 的 question 跳过且不污染游标（测试块 5：cursor 停在 well-formed max） |
| **无 --ack（#20 同族硬约束）** | ✅ | 两侧读 `team.inbox` 均不带 ack；注释明示服务端游标 per-member、ack 会吞掉同成员其他会话的条目 |
| 状态推进时机 | ✅ | state 在决策末尾统一推进——调用方中途失败可安全重试（at-most-once 唤醒；digest 兜底持久视图） |
| 诚实唤醒文案 | ✅ | `buildInboxWakeMessage` 明示「非任务指派」+ 指向 teamx_discussion/teamx_inbox，不污染任务语义（followupWithMessage 走独立消息通道，不进 auto-execute 任务管道） |
| 测试有效性 | ✅ 实跑 | 本机 bun：#105 `ALL PASS (inbox-wake)`、#106 `ALL PASS (inbox-wake-opencode)`，各 24 断言；两份测试仅 import 路径与注释差异（决策层断言同构，防漂移） |

## 2. #105（dsh 侧）专项：PASS

- `followupWithMessage`（auto-execute.ts）：先 refreshDigest 再发 user message——唤醒 prompt 带新鲜上下文；独立于 processEvents 任务管道，语义无污染。
- 接线：refreshAgent 轮询末尾追加 `team.inbox` 消费；unregister 清理 inboxWakeStates（无泄漏）。
- **建议（非阻塞）**：① 多 agent 场景每轮询每 agent 各跑一次 `team.inbox` CLI——CLI 进程放大 N 倍，量大可考虑进程内共享一次结果；② 「停机期条目靠 teamx_inbox/digest 兜底」依赖 digest 可靠性——已在注释声明取舍，建议在 #102 关单时同步写进 issue（设计权衡留痕）。

## 3. #106（opencode 侧）：**必改 1（合并机制）+ 实质代码 PASS**

### CR-1（必改）：分支夹带 superseded 的 B 版 G2 实现 + 落后 main 76 commits
- **判据**：分支 `origin/main..fix/issue-102-inbox-wake-opencode` 含 2 commits——7504e63（opencode 移植，本 PR 主体）+ **bcb9bfc（task-99 的 B 版 G2 activate trait 实现）**。bcb9bfc 携带 `crates/teamx/src/{provision.rs,k8s.rs,commands.rs}` 改动（ActivateCtx + activate trait + approve 门调用迁移）。
- **影响**：owner 裁决（task-99）G2 权威 = **A 版 `feat/g2-activate-trait @ 49264d1`**，B 版（bcb9bfc，PR #104）已按裁决 superseded 关闭。#106 若按当前分支合并，会把已判 B 版的 crates 改动带入 main：① 与 A 版合并冲突；② B/A 版语义差异（错误消息、缺 3 条单测、commands.rs 对 provider_for 失败的处理 return Err vs 回落 EC2 路径）与裁决冲突。
- **修法（干净无冲突）**：`git rebase --onto origin/main eead794 fix/issue-102-inbox-wake-opencode` —— bcb9bfc 只触碰 crates/、7504e63 只触碰 opencode-plugin/+tests/，剔除后 rebase 零冲突；rebase 后 PR diff 应只剩 opencode-plugin/ + tests/ 两处。
- **opencode 实质代码本身 PASS**：inbox-wake.ts 与 #105 逐字一致（仅注释适配）；refreshInbox 挂 refreshAll（轮询+WS push 双路径）+ 新成员会话注册时基线采点（防新会话回放）✓；唤醒 session.promptAsync（opencode 运行时对应 followup）✓；`inbox?.data` 包装层处理正确（opencode runCli 的 {data} 包装，与 dsh 侧裸 JSON 差异已处理）✓。

## 4. 关单路径

两份 verdict 落地后：#105 可合（owner）；#106 rebase 剔除 bcb9bfc 后可合 → issue #102 关闭 → Teamx Cloud 讨论唤醒全链（dsh + opencode）闭环。

*评审实跑记录：bun tests ×2 全绿（24+24 断言）；diff 对照见评审过程（inbox-wake.ts 逐字一致）。*
