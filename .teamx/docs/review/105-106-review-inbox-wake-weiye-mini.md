# Review: PR #105 + #106（issue #102 S1 inbox-wake 双端移植）

- reviewer: weiye-mini（reviewer≠author：#105 作者 weiye，#106 作者 harness 会话）
- 对象: `origin/fix/issue-102-inbox-wake`（PR #105，dsh-plugin，+299 行）· `origin/fix/issue-102-inbox-wake-opencode`（PR #106，opencode-plugin，+351 行）
- verdict: **PR #105 = PASS（2 非阻塞建议）· PR #106 = CHANGES-REQUESTED（1 阻塞：范围混入）**

## PR #105（dsh-plugin）— PASS

**三个评审问题逐一回答**：

**① seq 游标与首轮基线语义（重启不回放 vs 停机期吞条目）——取舍可接受。**
`selectInboxWake` 的设计：首次调用采纳当前 max seq 为基线不唤醒（防重启回放风暴）；停机期到达的条目不丢——仍可经 `teamx_inbox` 与 digest 查看（代码注释明确声明此兜底）。语义定位正确：**唤醒是注意力信号，不是投递机制**；durable view 是 digest。状态推进放在函数末尾（调用方中途失败时决策可重试，at-most-once）——实现严谨。建议（非阻塞）：把「停机期条目靠 teamx_inbox/digest 补看」的用户预期写进部署文档，避免"我没收到唤醒"的误报。

**② 无 --ack + 客户端游标——正确（#20 同族）。**
server 端 inbox cursor 是 per-member（`inbox_cursors`），多会话共享；本会话 ack 会吞掉其他会话的条目。设计改为：服务端 cursor 不动（无 --ack），去重完全在客户端 per-agent 内存态（`inboxWakeStates` Map，随 agent 注销清理）。代价是每个 agent 重启后重新采基线——与 ① 的取舍自洽。✓

**③ followupWithMessage 不污染任务语义。**
三重保障：a) 消息头部明确「讨论提醒（非任务指派）」——沿用 PR #98 的 addressed/任务 二分词汇；b) 走 `agent.followup`（与 processEvents 同机制）但**不创建 task、不认领任何东西**；c) 先 refreshDigest 再 followup（与 processEvents 同序，唤醒 prompt 带新鲜上下文）。✓

**实现质量**：纯函数决策层（可测）与接入层分离；文本截断 200 字符防刷屏；`text()` 类型守卫健壮。非阻塞建议：`state.questionIds` 无界增长（长期运行会话，每次 poll 累积已见 id）——量级极小（open question 生命周期短），但可加 LRU 或随 issue 关闭清理。

## PR #106（opencode-plugin）— CHANGES-REQUESTED

**inbox-wake.ts 移植本身：语义一致性 ✓。** 121 行与 #105 的 120 行逐行同构（仅 doc 头改为 opencode 语境 + KEEP IN SYNC 标注）——移植忠实。

**🔴 阻塞：provision.rs 的 36 行混入（范围污染）。**
`git diff origin/main...fix/issue-102-inbox-wake-opencode` 显示该分支携带了 `crates/teamx/src/provision.rs` 的 **ActivateCtx 结构体 + CloudProvider::activate 默认实现 + FakeProvider 空行 + WS-I 测试块**——这是 **WS-I/task-99（G2 线）的工作**，与本 PR 的 opencode 移植无关。且按 owner 裁决（seq 8522），G2 已放弃分支路径、**直接在 main 重写 activate() 为单一函数签名（无 ActivateCtx）**——本分支携带的旧版 ActivateCtx 实现与 main 上的裁决版**冲突**：合并 PR #106 会把已废弃的 ActivateCtx 重新引入 main。

**修法（二选一）**：
1. **rebase 到当前 main**：G2 重写已在 main 上，rebase 后 provision.rs 部分自然消失（或变成需手工丢弃的冲突），仅保留 opencode-plugin/ 与 tests/plugin-unit/ 的移植内容；
2. 或新建分支从 main cherry-pick 本 PR 的 opencode-plugin 相关 commit，弃用现分支。

改完无需重评审——移植内容本身 PASS，仅范围修正后即可合并。

**tests/plugin-unit/inbox-wake-opencode.test.ts（112 行）**：结构同 dsh 版，覆盖 8 组断言——随 rebase 一并保留即可。

## 收尾建议（非阻塞）

两份 inbox-wake.ts 逐行同构——建议后续抽公共包或加 CI keep-in-sync 检查（#106 注释已标 KEEP IN SYNC，但无机制强制）。两份 PASS 后 #102 关闭，Teamx Cloud 讨论唤醒全链（dsh + opencode）闭环。
