# Review: T1 设计文档 brainstorm-mode.md（#7 头脑风暴模式）

- reviewer: caicai（非作者；作者 weiye-mini，task-62 doc-only）
- target: `.teamx/docs/design/brainstorm-mode.md` @ 台账 commit a123319
- 裁决基线：讨论 #7 owner 收敛 dsc_18d64a87037b8ab12222bc#4852（八条机制定稿）
- date: 2026-09-19
- verdict: PASS（建议通过；实现派发按文档 §5 暂缓口径等合并队列）
- 本机无 cargo；doc-only 设计评审，以收敛结论逐项比对 + 测试可写性核验为准。

## 结论

文档与 owner 收敛八条逐项一致，无阻塞项。0 新状态、mode 标记、close payload 扩 directions[]、kind 必填四类、concern 挂 idea+折叠不唤醒、无投票、方向级 handed_off 1:N 单向只读、budget.alert 提前收窗非硬停——全部落字。测试设计含最关键的存量 digest 回归锚。建议通过。

## 核验点（对照收敛八条）

1. 状态机：PASS。P0a 零新状态；created 增 mode/window_secs/min_ideas_per_member 三可选字段（配置非状态）；phase 派生；复用 open/closed；到期只提示不自动收窗（nudge 提醒 owner/lead）✓。
2. 产出：PASS。directions[] 并列无排序；source_message_ids[] + next_step 必填；禁 score/rank/priority（拒绝清单）；与 summary 按 mode 互斥；brainstorm 恒不生成 auto digest ✓。
3. 参与规则：PASS。窗口内 kind 必填（idea/concern/question/reference）；concern 须 reply_to 某 idea、默认折叠、不唤醒；min 下限为 close 校验 + owner 可豁免 + nudge 激励（非门禁）✓。文档未引入任何投票/排序机制，与定稿一致（默认成立）。
4. 交接：PASS。仅 owner/lead 触发；agent 不得自行收敛；handed_off 方向级 1:N 单向只读；原帖 harvested 不可变；重复 handed_off 同 direction 拒绝 ✓。
5. 成本：PASS。budget.alert 阈值动作 = 提前收窗进 harvested（非硬停）；唤醒纪律服务端强制（仅显式 @ / 到期 nudge / 收窗三种）；activity 打标；每成员回合上限 ✓。
6. 零迁移承诺：PASS（设计自洽性成立）。mode/kind 均为新增可选字段；非 brainstorm 线程不校验 kind；无新表、无 schema 变更——P0a/P0b 纯加法的前提成立。
7. 测试设计：PASS。单测 8 条全为可写负例；第 5 条同时锁死「brainstorm 不生成 digest」与「普通讨论仍自动 digest」（存量回归锚——本模式最要紧的一条）；E2E 全流程闭环（建风暴→拒无 kind→concern 折叠→收窗→回溯→交接→原帖不可变→无 auto digest）✓。
8. 插件面：PASS。genedna 四点全部随切片落地（kind 透传 / mode 过滤 / 防漂移单测 / 调用点清单进 PR 模板）+ 六处触达点清单；旧插件缺透传时服务端 fail-closed 显式报错定为好性质 ✓。

## 真问题

无（阻塞项无）。

## 建议（均不阻塞，供作者采纳）

1. **phase 名词统一**：§1.1/§3.1 用「now>window_secs→可聚类」，而 §0 与收敛定稿的 phase 只有三值（diverging/harvested/方向级 handed_off）。请明确「可聚类」是 diverging 的到期子态描述还是第四个 phase 取值，避免实现时枚举漂移。
2. **切片命名统一**：收敛第六条与 weiye-mini #3847 用 S1–S3，本文用 P0a–P1c，且 §2 插件段又写「S1 PR 模板」。建议补一行 S1–S3 ↔ P0a–P1c 对应关系（S1≈P0a+P0b？S2≈P1a？S3≈P1b？），防派发时对号错位。
3. **P1c 缺行为断言**：8 条单测无一覆盖 budget.alert→提前收窗（阈值动作是 P1c 的核心语义）。建议加一条：budget.alert 触发 → phase 进入 harvested 且上下文保留（非硬停可断言）。
4. **handed_off 目标存在性**：只定义了重复拒绝，未定义 target_kind/target_id 是否必须存在。建议明确悬空链接策略（拒绝创建 vs 允许后补），否则回溯断链。
5. **显式引用 TEAM.md 豁免条款**：§2/§5 已写「走铁律」，建议把收敛第八条（brainstorm 产出非设计非决策 + 豁免条款）原文引用一处，防后续实现者误把方向清单当设计直接开工。
6. **E2E「不唤醒」断言口径**：建议显式为「台账无唤醒/auto-execute 事件」，否则「不唤醒」不可证伪。

## 缺失测试 / 风险

- 作者未附实测（doc-only，按任务定义正常）；上述建议 3/6 补齐后测试矩阵即完整。
- 风险见文档 §4 四条，已覆盖主要项；补充一点：§4.3「owner 长期离线风暴悬挂」可接受，但建议 harvested 前的 nudge 升级路径（nudge owner 无响应 → 提醒 lead）在 P0b 一并写死，否则 lead 代收依赖人工发现。
- reviewer≠author 满足（作者 weiye-mini，我为非作者）。
