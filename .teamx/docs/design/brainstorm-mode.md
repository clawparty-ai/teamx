# T1 设计：头脑风暴讨论模式（brainstorm-mode）

- doc: design
- id: brainstorm-mode（标准目录登记建议编号 006-t1-brainstorm-mode）
- author: weiye-mini（role-ef4e5f1a）
- created_at: 2026-09-19
- task: task-62（doc-only，不建 GitHub PR；实现切片后续另派，reviewer≠author）
- 裁决基线: 讨论 #7 owner 收敛 dsc_18d64a87037b8ab12222bc#4852（八条机制定稿）。贡献引用：weiye #3842/#3843/#3851/#3852（轻量反提案+机制化）、weiye-mini #3847（S1-S3 切片+测试集）、xiaods #3884（fork 式发散）、genedna #4266（插件调用面四点）、caicai #4734（实现/测试视角确认）。

## 0. 机制总览（P0/S1 分列）

| 切片 | 内容 | 性质 | 依赖 | 代码面 |
|---|---|---|---|---|
| P0a | 状态机：`discussion.created` 加 `mode:"brainstorm"` + `window_secs` + `min_ideas_per_member`（配置非状态）；phase 派生（diverging / harvested / 方向级 handed_off）；复用现有 open/closed | 纯加法 | 无 | discussion.rs + db meta |
| P0b | 参与规则：发散窗口内消息必填 `kind ∈ {idea,concern,question,reference}`；concern 必须 reply_to 某 idea 且默认折叠、不唤醒；下限 `min_ideas_per_member`（close 校验，owner 可豁免，nudge 激励非门禁） | 纯加法 | P0a | discussion.rs 校验 |
| P1a | 产出：复用 `discussion.closed` payload 扩 `directions[]`（并列无排序，每项 `source_message_ids[]` + `next_step`，**禁 score/rank/priority**）；与 `summary` 按 mode 互斥；**brainstorm 恒不生成 auto digest**（按 mode 分叉 digest_summary） | 行为分叉 | P0b | digest 派生 + close 校验 |
| P1b | 交接：owner/lead 触发（agent 不得自行收敛）；`brainstorm.handed_off{direction_id,target_kind,target_id}` 方向级链接，1:N，单向只读；原帖 harvested 不可变 | 新事件 | P1a | discussion.rs + 台账 |
| P1c | 成本：接 `budget.alert`，阈值动作 = **提前收窗进入 harvested**（非硬停）；唤醒纪律服务端强制（普通 idea 不触发 @唤醒）+ activity 打标 + 每成员回合上限 | 接线 | P0b | nudge/activity |

**零迁移承诺**：P0a/P0b 均为纯加法（mode 为新增可选字段，存量消息无 kind、读侧不校验——这是 S1 纯加法的前提）；无新表、无 schema 变更。

## 1. 机制设计要点

1. **状态机（P0a）**：不加新状态。`discussion.created` payload 增 `mode/window_secs/min_ideas_per_member` 三个可选字段；phase 由事件**派生**（有 harvested→harvested；now>window_secs→可聚类；否则 diverging），close 响应可带 `window_expired:true` 提示但**不得自动收窗**（自动收窗 = agent 自行收敛的变体，违反定稿第四条）；nudge 体系加「窗口已到期待收窗」提醒 owner/lead。
2. **产出（P1a）**：`directions[]` 为结构化并列清单（无排序），每项必填 `source_message_ids[]`（可回溯）与 `next_step`，**禁止 score/rank/priority 字段**（拒绝清单，防过早评判机制化回潮）；brainstorm 模式关闭**恒不生成** auto digest/summary（与普通讨论互斥）。
3. **参与规则（P0b）**：diverging 窗口内 `discussion.say` 必填 kind（四类）；concern 必须以 reply_to 挂在某条 idea 下、默认折叠、不触发唤醒——「禁止过早评判」的结构性摩擦而非禁令；`min_ideas_per_member` 为 close 时校验的下限（owner 可豁免），日常以 nudge 激励。
4. **交接（P1b）**：仅 owner/lead 可触发 harvested 收窗与 handed_off；agent 侧自我约束进 TEAM.md 豁免条款；方向级 1:N 单向只读，重复 handed_off 同 direction 拒绝。
5. **成本（P1c）**：budget.alert 触发 = 提前收窗（保产出、止发散），非硬停丢上下文；唤醒纪律服务端强制——普通 idea 消息不触发 @唤醒（仅显式 @ / 到期 nudge / 收窗动作三种），防头脑风暴放大智能体回合数。

## 2. 实现切片与排序

```text
P0a mode 标记 + phase 派生（纯加法）      ← 可立即派发（零依赖、零迁移）
P0b kind 必填 + concern 折叠 + 下限校验   ← 依赖 P0a
P1a directions[] + digest 分叉            ← 依赖 P0b
P1b handed_off 方向级链接                 ← 依赖 P1a
P1c budget.alert 接线                     ← 依赖 P0b（可与 P1a/P1b 并行）
```

- 本任务（task-62）仅交付本设计文档（doc-only，不建 PR）；实现切片派发时走「设计→评审→测试→交付」铁律，reviewer≠author。
- 插件面（genedna 四点，随对应切片落地）：双插件 discussion 工具透传 kind（S1 隐藏改动——旧插件缺 kind 透传时服务端 fail-closed 显式报错，好性质）；digest/auto-execute 按 mode 过滤（P1a）；skill 表↔commands 防漂移单测（#95 已修首例，通用疫苗）；brainstorm 调用点清单进 S1 PR 模板（双插件工具签名 / 命令表 / digest 过滤 / console 渲染 kind 徽标 / im 查询面 / ws 唤醒注入，六处触达点）。

## 3. 测试设计与验收

**单测（全负例可写）**：
1. phase 派生三分支：有 harvested → harvested；now > window_secs → 可聚类；否则 diverging；
2. diverging 窗口无 kind 的 say 被拒（四类 kind 正例各一）；
3. concern 无 reply_to 被拒（必挂 idea）；
4. 非 owner/lead 调收窗/harvested 被拒（治理铁律落成代码负例）；
5. brainstorm 关闭不生成 digest（`summary_auto=false` 断言）；**普通讨论 closed 仍自动 digest（存量行为回归锚，防按 mode 分叉改坏现有路径——本模式最要紧的一条回归）**；
6. 同方向二次 handed_off 被拒；原帖 harvested 后再写被拒；
7. `directions[]` 结构断言：`source_message_ids[]`/`next_step` 必填、score/rank/priority 出现即拒；
8. 窗口内 `min_ideas_per_member` 未达 close 被拒 + owner 豁免路径可过。

**E2E（沿用现有 discussion/cli-test 惯例，temp DB/HOME + trap 清理）**：
建 brainstorm 讨论 → 三成员各发带 kind 的 idea → 一条无 kind 消息被拒 → 一条 concern（reply_to idea）默认折叠不唤醒 → owner/lead 收窗 harvested → 讨论打开显示方向清单（source_message_ids 可回溯）→ 交接一方向到新讨论/task → 断言原帖 phase 仍 harvested 且拒绝再写；全流程无 auto digest 产生。

## 4. 风险与开放项

1. **kind 字段存量兼容**：kind 为新增可选字段，非 brainstorm 线程不校验 kind——保证 P0a/P0b 纯加法；读侧展示对无 kind 消息留空即可。
2. **旧插件无 kind 透传**：服务端校验 fail-closed 会拒其消息——显式报错是好性质（提示升级插件），genedna 已列为插件面首项。
3. **窗口到期后的收窗责任**：到期只提示不自动收（nudge 提醒 owner/lead），owner 长期离线时风暴悬挂——可接受（风暴非阻塞路径），必要时 lead 可代收。
4. **实现依赖 discussion.rs 单文件集中**：P0a/P0b 与存量 discussion 路径同文件，回归锚（第 5 条单测）必须随每个切片跑。

## 5. 评审与后续

- reviewer≠author：本设计文档评审建议 owner 或 weiye（co-lead，机制讨论主要贡献者）。
- 实现切片派发顺序：P0a → P0b → P1a/P1c 并行 → P1b；每切片独立 PR、独立验收（沿用 IM/服务切片的「既有安全路径不与新增面同 PR」纪律）。
- 实现派发暂缓约束与 #6/#8 同口径：等 owner 合并队列（15 PR）瓶颈解除。
