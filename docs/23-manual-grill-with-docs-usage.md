# Grill with Docs 使用说明

`teamx-grill-with-docs` 用于在编码前把一个仍有不确定性的方案问清楚。它由 Teamx owner 主动启动，通过多轮问题逐步展开设计树，并把结论写入仓库中的设计记录、术语表和 ADR。

适合以下场景：

- 新功能存在多种可行架构，需要逐项权衡；
- 团队对范围、术语或完成条件理解不一致；
- 需要成员调查事实，但最终决定仍由 owner 做出；
- 讨论可能跨会话继续，需要从仓库记录准确恢复。

它不是普通问答，也不会在完成讨论后自动编码、提交或推送。

## 使用前提

1. 已安装 Teamx 及对应的 OpenCode 或 DSH 插件。
2. 当前会话已加入一个 Teamx team。
3. 当前成员是该 team 的 owner。成员可以承担事实调查，但不能代替 owner 启动或完成 Design Session。
4. 建议先共享团队 goal，以便所有参与者看到相同的目标和协作事件。

## 在 OpenCode 中使用

### 新建设计会话

```text
/team-grill 设计订单取消与退款流程
```

未指定文档时，会话记录默认创建为：

```text
docs/design/<topic-slug>.md
```

如果希望固定记录路径：

```text
/team-grill 设计订单取消与退款流程 --doc docs/design/order-cancellation.md
```

### 恢复已有会话

```text
/team-grill --resume docs/design/order-cancellation.md
```

恢复必须使用准确路径。工作流不会根据标题或最近修改时间猜测要打开哪个记录。

已完成的记录默认不会重新打开；需要先明确确认要重新讨论。新的结论会标记为替代旧结论，而不是悄悄覆盖历史。

## 在 DSH 中使用

DSH 通过自然语言选择运行时 Skill，不提供单独的 slash command。可以直接说：

```text
请使用 teamx-grill-with-docs，讨论订单取消与退款流程，
并把记录写到 docs/design/order-cancellation.md。
```

恢复时可以说：

```text
请使用 teamx-grill-with-docs，恢复
docs/design/order-cancellation.md 中的设计会话。
```

OpenCode 和 DSH 使用同一份规范协议，因此 owner 权限、问题轮次、事实调查和完成条件保持一致。

## 一次会话如何进行

### 1. 建立设计树

工作流先同步 Teamx 状态、确认 owner 身份，再读取仓库中能够安全发现的事实。所有尚未确定的选择会获得稳定编号，例如 `DQ-0001`。

问题之间可以存在依赖。只有前置问题已经解决的节点才会进入当前 Frontier。

### 2. 回答当前轮问题

同一轮会一次展示完整 Frontier，每个问题都包含推荐选择和理由。owner 可以逐项回答，也可以在认可所有推荐时回复：

```text
全部按推荐
```

回答含糊或遗漏时，对应问题仍保持未解决，不会自动采用推荐答案。依赖当前答案的后续问题会在下一轮出现。

### 3. 委派事实调查

如果某个决定缺少事实，owner 可以让工作流向成员发布 Fact Request。请求会包含：

- 关联的 `DQ-*` 设计问题；
- 独立的 `FR-*` 调查编号；
- 调查约束、所需证据和预期输出；
- 当前 Design Session Record 的路径。

成员通过 Teamx `update` 返回 Fact Report。报告只是待 owner 评估的证据，不能自动成为决定。如果证据不可访问、互相冲突或没有按时返回，owner 可以选择重新分派、缩小问题、放弃该证据，或明确接受不确定性。

### 4. 持续写入文档

每轮确认后，owner 会话会立即更新：

- `docs/design/<slug>.md`：完整的 Design Session Record；
- `CONTEXT.md`：项目专有术语的简短定义；
- `docs/adr/*.md`：重要、非显然且难以逆转的架构决定。

Teamx ledger 只保存协作事件和文档引用，Git 中的文件才是持久知识来源。

### 5. 明确完成

只有同时满足以下条件，Design Session 才能完成：

1. 设计树没有未访问分支；
2. Frontier 与 Remaining Branches 均为空；
3. 每个已提出的问题都有明确处理结果；
4. 设计记录、术语表和 ADR 相互一致；
5. human owner 明确确认 Shared Understanding。

例如：

```text
我确认 Shared Understanding，可以结束设计会话。
```

完成后，工作流会把记录状态改为 `completed`，并发布指向这些文档的决定通知。此时只表示可以开始实现；编码、运行系统命令、提交和推送仍需要单独授权。

## 常见问题

### 提示当前成员不是 owner

切换到 team owner 的会话，或先检查 `/team-status`。成员可以完成 Fact Request，但不能替 owner 做设计决定。

### 找不到恢复记录

使用从仓库根目录开始的准确相对路径，例如：

```text
/team-grill --resume docs/design/order-cancellation.md
```

### 一轮问题太多

Frontier 只包含当前已解除依赖的问题。可以要求缩小当前设计主题；不要通过跳过未回答问题来提前结束会话。

### 想在讨论完成后立即编码

先明确确认 Shared Understanding，让 Design Session 正常完成；然后在新的指令中授权实现。这样设计记录与执行边界保持清晰。

协议维护者请修改 `protocols/grill-with-docs.md`，再运行 `node scripts/generate-grill-protocol.mjs`。生成的 OpenCode 和 DSH 适配文件不应直接编辑。
