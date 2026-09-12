# discussion 参与机制：通知 / AI 唤醒与自主判断 / per-topic 配额

> 对应 `docs/43-design-discussion.cn.md`（事件/引用底层）与 `docs/44-design-console-discussion.cn.md`（console UI）之上的**参与层设计**。
> 解决：成员(人/AI)收到"有人回复讨论" → AI 是否/何时被唤醒 → AI 如何判断该不该回 → 如何防止无限消耗。
> 状态：**设计定稿（待测试方案 → 开发）**；更新：2026-09-08
> 相关代码：`opencode-plugin/src/index.ts`（toast/auto-execute/notableEvents）、`crates/teamx/src/server_tasks.rs`（worker inbox）、`discussion.rs`、`docs/27-taskx.cn.md`（任务模型/水位思想）

## 1. 背景与动机

讨论（discussion）目前是「成员主动进 console 发/回」的拉模型：有人回复后，其他成员**并没有可靠地被通知**，AI 成员也**不会被唤醒参与**，更不会判断"我该不该回"。这使讨论无法成为异步团队的真实协作层——回复石沉大海，AI 不参与。

本设计补齐三件事：
1. **通知**：成员（在线/离线、人/AI）能感知"有人回复了讨论"。
2. **唤醒**：AI 在满足条件时被唤醒到"可以读这条讨论"。
3. **配额护栏**：每 AI 每话题设最大主动回复次数，防止无限消耗；用户可随时调整。

## 2. 现状（代码事实）

| 机制 | 现状 |
|---|---|
| 事件推送 | `discussion.*` 事件进 ledger → 在线 opencode 经 WS 收到 → 插件 `summarizeEvent` 走 **default 分支**（通用文案，无专属提示） |
| 离线 AI（worker） | worker inbox（`server_tasks::inbox`）只含 tasks/questions/nudges，**不含 discussion** → 收不到、不唤醒 |
| auto-execute | `shouldAutoExecute` 只对 `assignee_member_id==self` 事件触发；discussion 无 assignee → **不自动参与** |
| 自主判断 | 无（AI 从未被带到"是否该回"的决策点） |
| 防消耗 | 无任何限额/水位 |

## 3. 决策摘要（已与 owner 对齐）

| # | 决策 | 定稿 |
|---|---|---|
| D1 | 谁会收到"回复"通知 | **全体可见提示**；AI 仅在被 @ / 订阅 / 任务相关时**被唤醒** |
| D2 | AI 何时被唤醒 | **@提及 → 订阅该讨论 → 讨论挂到自己的 task**，三者皆可触发（可配置开关） |
| D3 | AI 是否自主判断该不该回 | **唤醒后自主判断**：读上下文 → 有相关信息/职责/被@才回，否则静默不打扰（记"已读未回"水位，避免反复唤醒） |
| D4 | 配额护栏 | **per-topic 配额**：每 AI × 每话题独立「最大主动回复次数」（默认 20）；主动发言消耗配额，**被 @ 响应不消耗**；达上限后 AI 不再主动发言但仍可响应 @；用户可随时调整 |
| D5 | 关注方式 | 先做 **@ 提及**；订阅/任务绑定作为可配置触发一并纳入 v1 开关 |

### D4 配额语义（关键细节）

- **per-topic**：配额按 `(member_id, discussion_id)` 独立计数。话题 A 回满 5 次不影响话题 B。
- **主动 vs 响应**：
  - **主动回复**（无 @ 触发、AI 自己决定插话）→ 消耗 1 次配额。
  - **被 @ 响应**（消息里 `@<display_name>` 明确指向该 AI）→ **不消耗配额**。
- **达到上限后**：该 AI 不再主动回复该话题；但被 @ 仍可响应（响应不耗配额、不受上限限制）。
- **计数来源**：事件即存储 → 统计该 AI 在该话题发出的 `discussion.message` 条数中「主动发言」（非被@响应）即可，无需额外计数表。
- **管理**：成员级属性 `discussion_quota`（默认 20，per member，作用于其所有话题）；owner/lead 可设，成员可改自己的；存 server 随成员走。

## 4. 详细设计

### 4.1 通知层（插件 toast/digest）

`notableEvents`/`summarizeEvent` 增加 `discussion.created / discussion.message / discussion.closed` 专属分支：

| 事件 | 文案 |
|---|---|
| `discussion.created` | 「{author} 创建讨论「{title}」」 |
| `discussion.message` | 「{author} 在「{title}」回复：{正文前 80 字}」；若含 `@我` 前缀加「(提及你)」高亮 |
| `discussion.closed` | 「讨论「{title}」已关闭」 |

- 在线成员：toast/digest 即时可见。
- 离线 AI（worker）：见 4.2 的 inbox 扩展。

### 4.2 唤醒层（AI 何时被带到可决策点）

**触发条件（任一）**
1. **@提及**：`discussion.message` 正文解析到 `@<self display_name>`（或角色）。
2. **订阅**：该 AI 曾 `discussion subscribe <id>`（新增 RPC/CLI），该讨论任何新消息唤醒它。
3. **任务相关**：讨论创建/挂载了 `task_id`，而该 AI 是该 task 的 assignee/相关成员。

**实现载体**：worker 的 inbox（`server_tasks::inbox`）新增第 4 类 `discussions`（命中的讨论新消息，带消息摘要与讨论 id）。worker 轮询发现有新 `discussions` 条目且满足唤醒冷却时，跑一次 headless opencode turn，把「待处理讨论」注入 prompt。

**唤醒节流**：沿用 worker 现有 `cooldown`；对每 AI×每讨论维护「已处理水位 seq」——低于水位的旧消息不重复唤醒。

### 4.3 自主判断层（AI 决定该不该回）

被唤醒后给 AI 的指令（worker prompt / 插件 auto-run prompt 追加）：

```
[teamx discussion 提醒]
讨论「{title}」(id) 有新消息/你被 @：
<最近 3-5 条消息上下文，含作者与引用>
请判断：
- 若你有相关信息、职责关联、或正被 @：用 teamx_discussion_reply/say 参与（被 @ 响应不耗配额）。
- 若与你无关或没有增量信息：不要回复，保持安静（不产生消息即不消耗配额）。
```

配套：
- **已读水位**：该 AI 读完该讨论后，记录已处理 seq；未产生新触发前不再唤醒。
- **AI 不主动刷存在感**：prompt 明确"无增量信息就不回"，减少群聊噪音。

### 4.4 配额层（服务端为最后防线）

**配额判定**（`discussion.say/reply` 服务端）：
- 解析请求中 `reply_to`/quotes 来源，判断本次是否「被 @ 响应」。服务端只强制**主动发言**配额。
- 若该 `(member, discussion)` 主动发言计数 ≥ quota 且本次不是被 @ 响应 → 拒绝：`discussion quota reached for this topic (N/N)`。

**计数**：主动发言数 = 该 AI 在该讨论的 `discussion.message` 事件中「非被 @ 响应」的条数（事件即存储，服务端统计）。

> 插件/CLI 也可在发送前提示配额剩余（体验），但**服务端校验是硬边界**（防止多端绕过）。

### 4.5 成员配额配置

- 存储：`members.discussion_quota`（INT，默认 20；mention 回复不计数）。
- CLI/RPC：
  ```
  teamx member discussion-quota get --session s [--member m] [--team t]   # 读
  teamx member discussion-quota set --value <N> --session s [--member m] [--team t]  # 写（本人可改自己；owner/lead 可改任何人）
  ```
  RPC 方法：`member.discussion_quota`（`{action, value?, member?, team?}`，网络模式经 mTLS）。
- console：讨论页/成员视图提供配额显示与编辑入口（owner/lead 与本人）。
- **默认值迁移说明（方案 B）**：`DEFAULT 20` 仅对**新建数据库 / 新建成员**生效；既有数据库（`user_version >= 15`）列默认仍为 5，既有成员保留其现值（不做全量回填）。既有部署如需放宽，请用上述 RPC/CLI 显式设置。

## 5. 数据流示例

```
（caicai 在 console 回了一条，正文含 @zhaoz）
   → discussion.message 事件
   → 在线成员 digest：「caicai 在「Review round2」回复：…(@zhaoz 被提及)」
   → zhaoz worker inbox 出现「discussion:@zhaoz 被提及」条目（若 zhaoz 在线被 @ 高亮）
   → worker 冷却通过 → headless turn 唤醒 zhaoz：
       prompt「讨论 Review round2 有新消息，你被 @…请判断是否参与」
   → zhaoz 判断：这正是我负责的 extractor 范围 → 回复（被 @，不耗配额）
   → zhaoz 的 (zhaoz, Review round2) 主动发言计数未超限（若后续无人 @ 却主动插话，则每次 -1 配额）
```

## 6. 配额示例

| 场景 | 结果 |
|---|---|
| zhaoz 话题 T 主动发言 5 次（配额 5）后再次主动 say | 服务端拒绝（quota 5/5） |
| 之后有人 @zhaoz，zhaoz 响应 reply | 允许（不耗配额） |
| owner 把 zhaoz 配额改为 8 | zhaoz 话题 T 可再主动发言 ≤3 次 |
| zhaoz 在话题 U 主动发言 | 独立计数（U 从 0 起） |

## 7. 约束与兼容
- 新增 inbox `discussions` 类型不破坏现有 task/question/nudge 解析。
- 配额是 per-topic 软护栏：**服务端拒绝**只拦主动发言；被 @ 响应永不被拦。
- @ 解析 v1 用文本匹配 `@<display_name>`（与 43 的 @提醒 D5 一致）；精确成员匹配后续可做 mention 结构。
- AI 判断"该不该回"依赖模型判断质量；用水位+冷却+配额三重复合控制噪声。
- 文档中英双语（如落实）。

## 8. 待测试方案（开发前补齐）
- 服务端：@响应不耗配额 / 主动超限拒绝 / per-topic 独立 / quota 改大后恢复。
- inbox：discussion 条目出现/去重/水位不重复唤醒。
- 插件：discussion toast 文案、@我 高亮。
- worker：唤醒 prompt 注入 + 冷却。
- CLI/RPC：quota get/set 权限（本人/owner/lead）。

## 9. 相关文档
- `docs/43-design-discussion.cn.md`（事件/引用底层）
- `docs/44-design-console-discussion.cn.md`（console UI）
- `docs/45-test-plan-batch-20260907.cn.md`（既有测试方案）
- `docs/27-taskx.cn.md`（task/水位思想）
