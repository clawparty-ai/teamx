# 22 — Code Review 报告：DocSpec 文档生命周期体系（T1–T5，2026-08-28）

> 范围：`main` 分支上尚未 push 的 DocSpec 系列提交
> （`c3a107a` T1、`abb7230` T2、`4811e13` T3、`c3c1da3` T4、`3fa1f0c` T5，
> 以及设计文档 `0c9548c`、AGENTS 规则 `fa88cb8`）。
> 重点走读 `crates/teamx/src/{teamfile.rs, doc_flow.rs, commands.rs}` 与 `tests/teamfile-test.sh`。
>
> 目标：验证 `docs/05-design-teamfile-docs.cn.md` 的 T1–T5 落地正确性、安全性与可维护性。

## 评审方法

1. **第一轮 — 架构与正确性**：`doc.*` 事件从 CLI `publish` → `cmd_publish_doc` → `doc_flow::apply_event`（纯函数校验）→ 写台账/持久化 `.meta.json` → reactions 定向通知 的数据流；状态机语义。
2. **第二轮 — 安全与边界**：路径构建的输入校验、reactions 的 assignee 副作用、pending 成员处理、未知事件 fail-open。
3. **第三轮 — 收敛**：模板执行、文档注释一致性、解析歧义、审计字段精度。

---

## 第一轮 — 架构与正确性

| ID | 严重度 | 文件 | 发现 | 建议 |
|---|---|---|---|---|
| M1 | Med | `commands.rs:2415-2417` `cmd_publish_doc` | 文档根 `docs_root` 取自 `current_dir()`（`.teamx/docs`），但团队/台账在 `TEAMX_DB`。若会话在**非 TEAM.md 所在目录**执行 `publish`，会找不到 `_spec`/`meta.json`。文档与团队是**两套坐标**（文件系统 vs DB），多目录/多会话场景会静默失败。 | 文档根由团队注册的项目根决定（bootstrap 时记录 `teamx_dir` 到 teams 表），`cmd_publish_doc` 用团队的项目根而非 CWD。 |
| M2 | Med | `commands.rs:2466-2495` | 台账事件写（`emit_json`）与 `.meta.json` 落盘（`meta.save`）是**两次独立写**。中间崩溃会导致台账有事件、meta 缺失；且 meta 是整文件覆盖，并发 `publish` 可能相互覆盖。审计链完整性依赖"先写台账再写 meta"的顺序，无事务保护。 | 至少把 meta 写成临时文件再 `rename`（原子替换）；或在 meta 中记录已消费的最大 ledger seq，启动时对齐。 |
| M3 | Med | `doc_flow.rs:106-140` `validate_transition` | 向前转移只要求 `ti > fi`，**允许跳步**（如 `opened -> closed` 跳过中间态）。设计 §2 示例按步推进，reactions 按事件名匹配，`opened->closed` 会触发 `doc.closed` 反应但跳过 `triaged/assigned/...` 的反应。是缺省行为，但是否符合"按链推进"的语义需明确。 | 若要求相邻转移，改为 `ti == fi + 1`；若允许跳步，在设计与测试注释中明确（当前 TF-202 已规避跳步用例）。 |
| L1 | Low | `doc_flow.rs:209,222,242,263` | 审计 `MetaStep.by` 记录的是**角色**（`actor_role`）而非成员 id/display_name。多人同角色时无法区分"谁"操作。 | 记录 `actor.id` 或 `display_name`（保留角色作冗余）。 |

---

## 第二轮 — 安全与边界

| ID | 严重度 | 文件 | 发现 | 建议 |
|---|---|---|---|---|
| S1 | **High（安全）** | `commands.rs:2411,2418,2428`；`doc_flow.rs:58-60` `meta_path` | `doc_id`（来自 payload）与 `doc_key`（来自 TEAM.md `### key`）直接拼进路径，无字符校验。`meta_path = docs_root.join(doc_key).join("{id}.meta.json")`；`spec_path = docs_root.join("_spec").join("{doc_key}.json")`。`doc_id = "../../../etc/x"` 经 `Path::join` + 后续 `fs::write` 会**穿越出 `.teamx/docs`**（CWE-22）。`teamfile.rs:225` 已有 `is_safe_member_key` 对成员 key 做校验，但文档 key/id 未复用。 | 对 `doc_key`/`doc_id` 增加 `is_safe_member_key` 同类校验（禁止 `/ \ ..` 与控制字符；非空、不以 `.` 开头）；非法 id 直接拒绝 `doc.*` 事件。 |
| S2 | Med | `commands.rs:2498-2541` | reactions 以 `doc.reaction` 事件发出，且 payload 带 `assignee_member_id`。按插件自动执行规则（`assignee_member_id == my_member_id` 触发），目标成员会话会被唤醒并尝试执行该事件；但 `cmd_publish_doc` 对 `doc.reaction` 会走 `classify_event → Forward`，要求 `to` 状态，而 reaction payload 无 `to` → **执行失败/报错**。reactions 本应是"通知型任务"，不应被当作生命周期转移再发布。 | `cmd_publish_doc` 入口对 `publish_type == "doc.reaction"` 直接返回清晰错误（"系统通知，非生命周期事件"）；或在自动执行层把 `doc.reaction` 视为"执行 action 描述后自行发布转移"，不回灌 `doc.reaction`。 |
| S3 | Low | `commands.rs:2508` | reactions 通知过滤只排除 `left`/`denied`，**pending 成员仍会收到** `doc.reaction`。pending 成员无法行动，收到通知是噪音（且计入其台账）。 | 同时排除 `pending`（或仅 `active`/`idle`/`waiting` 可接收）。 |
| S4 | Low | `doc_flow.rs:158-165` `classify_event` | 未知事件名（如 `doc.foo`）落入 `_ => Forward`，fail-open 地要求 `to` 并推进状态。若拼写错误（`doc.review` 误写 `doc.reviwed`），会被当作 Forward 处理而非报错。 | 维护显式事件名白名单；未知事件名返回错误（或至少 warn 并拒绝）。 |

---

## 第三轮 — 收敛 / 功能缺口

| ID | 严重度 | 文件 | 发现 | 建议 |
|---|---|---|---|---|
| G1 | Med | `commands.rs:944-959`；`doc_flow.rs` | `模板`（template）字段被解析并存入 `_spec`，但**从未被强制**：`apply_event(Created)` 不检查模板章节是否齐备，也不生成骨架文件。设计 §3 写明"doc create 按模板生成骨架；无模板则自由创建"——生成骨架能力未实现，模板目前仅为装饰性元数据。 | 要么实现"创建时按模板生成 `<id>.md` 骨架"，要么在设计与 `_spec` 注释中说明模板仅作提示（非强制）。当前至少应记录为已知缺口。 |
| G2 | Low | `doc_flow.rs:11` 模块注释 | 注释写 "load_spec / save_spec"，但模块只实现 `load_spec`，**`save_spec` 不存在**（bootstrap 在 `commands.rs` 手写 JSON 写出）。死文档引用。 | 删除注释中的 `save_spec`，或抽出 `save_spec` 复用。 |
| G3 | Low | `teamfile.rs:198-205` `split_states` | 状态流按 `->`/`→`/逗号 切分。若作者写成 `状态流: draft -> review, approved -> done`，会被切成 4 个顺序状态（`draft,review,approved,done`），逗号被当分隔符。语义与"箭头链"预期不符，易误用。 | 状态流只按 `->`/`→` 切分，逗号报错或忽略（文档示例统一用 `->`）。 |
| G4 | Low | `doc_flow.rs:202,218,236,257` | 一次事件内多次调用 `crate::events::db_now()`（meta.updated_at、MetaStep.at、可能不同值），时间戳可能相差 1ms，且与台账事件时间戳不一致。 | 事件入口取一次 `now`，贯穿 meta/step/台账。 |
| G5 | Low | `teamfile.rs:101-103` `is_incomplete` | 仅检查 `owner`/`states` 非空；`creators`/`approvers` 引用了不存在的角色也不报错（运行时才在 `can_advance` 表现为"无人能推进"）。 | bootstrap 时对 creators/approvers 引用的角色做存在性检查（仅告警，不阻断）。 |

---

## 接受项 / 设计取舍（非缺陷）

- **`can_advance` 用角色而非团队 owner**：团队的 owner（session role `owner`）能否推进某文档完全取决于该文档 `owner`/`approvers` 字段是否含 `owner` 字面量。这是"文档 owner 是角色名、与团队归属解耦"的刻意设计（TF-201 实测已验证）。需在文档中明确，避免混淆。
- **向前跳步（M3）**：当前被允许且测试已规避；若产品要求严格相邻，再收紧（见 M3）。
- **reactions 用 `on` 匹配事件名**（`created`/`reviewed`/`approved`）而非目标状态：与设计 §6.3 一致，TF-201 实测 `on created → 通知 team-lead`、`on reviewed → 通知 reviewer` 正确。

## 正面评价

- `apply_event` 为**纯函数**、校验失败零副作用，严格符合设计 §6.4（先 `teamx_sync` 再行动、失败不写台账/meta）。
- `DocSpec.key` 加 `#[serde(alias = "doc")]` 兼容 T2 `_spec` 快照字段名，契约前后向兼容（TF-108 验证）。
- 测试覆盖扎实：T1 单元 17 例、T3 `doc_flow` 9 例、T4/T5 集成 TF-201~205（创建/推进/权限拒绝/reactions/完整生命周期/多类型独立/未注册拒绝）。`cargo test` 89 全绿，clippy 基线 21 无新增。

## 优先级建议

1. **S1（High）** 必须先修：路径穿越是真实安全漏洞，加 `is_safe_member_key` 类校验即可低成本消除。
2. **S2 / M1 / M2（Med）** 下一轮修：reactions 回灌失败、文档根与团队坐标解耦、meta 原子写。
3. **G1（Med）** 明确模板语义（实现或文档化）。
4. 其余 Low 随维护清理。
