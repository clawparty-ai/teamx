# 用户手册：Grill Consensus Mode（团队共识设计访谈）

> 适用：需要**团队一起**把不确定的方案聊成"共识结论"的场景（而不是单人拍板）。
> 协议：grill-with-docs **v2 · Consensus Mode**（见 `protocols/grill-with-docs.md` 第 3A 节）。
> 面向：发起/主持访谈的 leader（人类）与参与作答的团队成员。

---

## 1. 它是什么

把 grill-doc（设计访谈）从"单人作答"升级为"**全员作答 → 收敛共识**"：

- **1 个 grill session = 1 个 discussion 线程**
- leader 对**每一个** Design Question（`DQ-xxxx`）发 **1 条顶层消息**
- 成员在该消息下用 **reply** 给出「答案 + 理由 + 异议」
- leader 汇总后，在同一问题下发 **`✅ 结论 DQ-xxxx`**（reply），并写入 **Design Session Record** 文件
- 逐题串行，直到 Frontier 为空、leader 确认共识完成

> 定位提醒：grill 用于**梳理需求/决策**；它**不写代码、不提交、不发布**。

---

## 2. 前置条件

- 你在一个 teamx 团队里（grill 也可在**建队前**以 pre-team 模式运行，但 Consensus Mode 需要 discussion，故需团队 + 服务端）。
- 客户端具备 `teamx_discussion` 工具（opencode/dsh 插件）或可用 CLI `teamx discussion`（网络模式）。
- 已更新到 **grill 协议 v2**（`/team-grill` 显示 v2）。

---

## 3. 角色

| 角色 | 谁 | 职责 |
|---|---|---|
| **leader** | 人类（发起者） | 建线程、构建 Design Tree、逐题发问、汇总结论、**唯一**写 record/CONTEXT/ADR、最后确认共识 |
| **member** | 团队成员 | 对每个 DQ 用 `discussion.reply` 作答（答案+理由+异议） |

> 单一写者原则：只有 leader 编辑 record；成员只通过讨论回复贡献内容。

---

## 4. 开始一个 Consensus Mode grill

### 4.1 创建讨论线程（leader）

```bash
# opencode/dsh 会话内使用工具：
teamx_discussion action=create title="<主题> 方案 grill（Consensus Mode）" body="<session 说明：leader、共识规则、结构>"
# 返回 discussion_id，例如 dsc_xxxxxxxx
```

`body` 建议包含：leader 身份、**共识规则**（unanimous / majority / leader-decides）、结构说明、Design Tree 初稿。

### 4.2 创建 Design Session Record（leader）

新建 `docs/design/<slug>.md`（模板见 `protocols/grill-with-docs.md`），填写 Context、Design Tree、Frontier。
> 注意：只有 leader 写这个文件。

### 4.3 逐题进行（对每个 DQ，串行）

**① leader 发顶层消息（问题）**

```bash
teamx_discussion action=say id=<discussion_id> body="❓ **DQ-0001** - **<标题>**：<问题+权衡+选项>

➡️ <推荐答案与理由>

请全员作答（discussion.reply，reply_to=本条）。作答窗口：全员已答或 N 分钟。"
```

**② 唤醒全员**

```bash
# 对每位成员（leader 用 publish/ask 定向触发）
teamx_publish type=update assignee=<member_id> data={"message":"【grill DQ-0001 征集】请 reply 到 <discussion_id>#<msg_id> 作答…"}
# 或用 teamx_ask member_id=<id> question="…"
```

**③ 成员作答（reply）**

```bash
teamx_discussion action=reply id=<discussion_id> message_id=<DQ 消息 id> body="答案：…；理由：…；异议：…"
```

**④ 汇总（leader）**

在作答窗口后，leader 发一条 **结论 reply**（reply_to 同一个问题消息）：

```bash
teamx_discussion action=reply id=<discussion_id> message_id=<DQ 消息 id> body="✅ 结论 DQ-0001
- 共识：<结论>
- 依据：…
- 分歧(dissent)：…
- 一致性：unanimous | majority | leader-decides"
```

**⑤ 写入 record（leader）**

在 `docs/design/<slug>.md` 的 `Settled Decisions` 增加：

```markdown
- DQ-0001 <标题>: <共识> — rationale: …; dissent: …;
  unanimity: majority; discussion: <discussion_id>#<msg_id>
```

**⑥ 开下一题**：回到 ① 发下一条顶层消息（DQ-0002）。

---

## 5. 完成条件

- 所有 `DQ-*` 都有 `✅ 结论` reply 且 record 中有对应条目；
- Frontier 为空；
- leader 明确确认 **Shared Understanding**；
- 将 record `status: completed`；
- `discussion.close <id>` 关闭线程；
- （可选）`teamx_publish type=decision` 公告，指向 record。

---

## 6. 规则与约定

- **共识规则**开局声明：`unanimous`（全体一致）/ `majority`（多数）/ `leader-decides`（leader 裁决）。
- **始终记录 dissent**，即使决策通过。
- **作答窗口**：全员已答或超时；缺席记为弃权。
- **讨论 append-only**：不能编辑问题消息；结论 = 结论 reply + record 双写。
- **事实 vs 取舍**：事实用 Fact Request（`FR-*`）；Consensus Mode 只解决主观取舍。
- **线程生命周期**：全程一个线程，结束才 close。

---

## 7. 常见问题

**Q：为什么结论要"reply + 文件"双写？**
讨论是不可编辑的台账（append-only），无法承载会演进的权威答案；文件（record）才是权威、可编辑的载体，讨论留作过程证据。

**Q：一个 session 可以用多个 discussion 吗？**
可以，但需 leader 维护 DQ→线程映射；默认**单线程**更简单。

**Q：普通成员能改 record 吗？**
不能。单一写者原则：只有 leader 写 record/CONTEXT/ADR。

**Q：成员没装 `teamx_discussion` 工具怎么办？**
先按第 2 节更新客户端；否则可用 CLI `teamx discussion reply`（网络模式），或由 leader 在台账代转（不推荐，破坏"成员自己发"的原则）。

**Q：高危动作（合并/revoke/destroy/goal.close）能进 IM 吗？**
这是 IM 接入的话题，不是 grill 本身；grill 只产决策文档。
