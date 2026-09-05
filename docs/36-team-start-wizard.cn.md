# teamx Team 定义向导：交互式产出 TEAM.md（增强版 `/team start`）

> 流程：需求 → 设计（本文件）→ 实现
> 状态：**设计定稿（可实施）**
> 目标读者：维护者 / 实现者
> 会话记录：`docs/design/team-definition-wizard.md`（14 个 DQ 已 settled）
> 更新：2026-09-04

## 1. 背景与目标

用户/客户（可能完全不懂 teamx）要"定义一支团队"时，需要一个**友好、结构化的引导向导**，逐步把"团队怎么组"问清楚，最终产出**可被 `team create` 直接消费的 `.teamx/TEAM.md`**。

- team-grill：通用设计/需求访谈（任意团队成员可发起，产出 `docs/design/…`）。
- `/team start`（增强目标）：面向"定义团队"的**分步线性向导**，内部仍复用 DQ 决策记录方法，但呈现为普通人都能走完的步骤。

## 2. 定稿决策（14 DQ）

| DQ | 决策 |
|---|---|
| 0001 | 分步线性向导（每步一屏）；内部以 DQ 记录决策 |
| 0002 | 入口 = 增强 `/team start`（不新开命令名） |
| 0003 | 内置 4 模板 + 角色白话解释；允许自定义 |
| 0004 | 保存前强校验（`teamx teamfile validate`） |
| 0005 | 主产出 `.teamx/TEAM.md` + 会话记录 |
| 0006 | 已有 TEAM.md → 结束提示「更新(增量) / 覆盖」 |
| 0007 | 不自动建队；结束提示 `/team create` |
| 0008 | 白话术语表，跟随语言 |
| 0009 | 7 步：①团队名 ②背景 ③目标 ④角色 ⑤成员 ⑥文档契约 ⑦预览+校验 |
| 0010 | 模板：研发交付 / 内容·用研 / 通用协作 / 从零自定义 |
| 0011 | 校验规则与错误内联 |
| 0012 | 术语表词条与侧栏呈现 |
| 0013 | 记录 `docs/design/<team-slug>-team.md`，默认建，可 `--resume`/`--no-record` |
| 0014 | 结束页：预览+校验+更新/覆盖+一行 `/team create`，可选 `--then-create` |

## 3. 向导步骤（7 步）

每步一个界面/一问一答；内部每步对应一个稳定 DQ，可中途退出并在下次 `/team start --resume` 续做。

1. **团队名**：一个名字 + 一句"这是什么团队"。
2. **背景**：一段自由文本（为什么存在、服务谁）。
3. **目标**：产出 ≤3 条、尽量可验证。
4. **角色**：选模板或自定义 → 得到角色清单；每个角色给白话卡（做什么/要几个/常见 key）。
5. **成员分配**：对每个成员录 `姓名 / 角色 / 分工 / 技能 / 输出`（`### key` 用姓名拼音/英文 slug 或用户名）。
6. **文档契约**（可选）：需要哪些文档类型 + 状态流（默认 `draft→review→approved→done`）+ 谁创建/谁把关。
7. **预览 + 校验**：显示 `.teamx/TEAM.md` 预览；未通过项列清单，修正后允许保存。

任一步可"跳过/稍后"；结束页统一提示缺口。

## 4. 模板库（默认只给"角色建议"，成员留空待第 5 步）

| 模板 | 建议角色 |
|---|---|
| 研发交付团队 | owner / tech-lead(contributor) / contributor / reviewer |
| 内容·用研团队 | owner / research-lead / searcher / kb-engineer / extractor |
| 通用协作团队 | owner / contributor / reviewer |
| 从零自定义 | 无预置 |

角色 key 必须来自内置目录（owner/observer/supervisor/contributor/subtask-implementer/reviewer）或团队已 approved 的自定义角色；向导允许为模板建议自定义角色 key。

## 5. 校验（`teamx teamfile validate`）

解析 `.teamx/TEAM.md`（复用 `teamfile.rs`），规则：

- 团队名非空；
- 背景非空；
- 目标 ≤ 3 条；
- 至少 1 个 owner 成员画像；
- 成员 `### key` 唯一；
- 每个成员角色 key 在目录内（内置或 approved）；
- 文档契约（若有）：状态流是合法链（如 `draft→review→approved→done`），引用的角色存在。

错误**内联到对应步骤**；预览页列出全部未通过项；全部通过才允许"保存/写入"。

## 6. 已有 TEAM.md 的「更新 / 覆盖」

- 向导开始时若 `.teamx/TEAM.md` 已存在：以其为起点预填，允许改进/修订。
- 结束时询问：**更新（增量合并：保留既有内容，仅应用本轮改动并 diff 展示）** 还是 **覆盖**。
- 全程保留会话记录，便于复盘。

## 7. 结束页与建队衔接

结束页展示：

- TEAM.md 预览 + 校验结果；
- 若已存在 → 「更新 / 覆盖」选择；
- 缺口清单（如"还没有人当 owner"）；
- 一行可复制命令（**不自动执行**）：
  ```
  /team create <团队名>
  ```
- 可选 `--then-create`：想一步到底的用户在向导命令里带上，末尾直接建队。

## 8. 白话术语表（侧栏常驻折叠）

团队(team) / 目标(goal) / 角色(role) / 成员(member) / owner(团队的负责人) / co-lead(共同负责人) / contributor(干活的人) / reviewer(把关/验收的人) / goal 状态（proposed→shared→in_progress→achieved）/ TEAM.md（团队的"章程"，建队时 teamx 读它生成角色、AGENTS、工作目录）/ 邀请函(letter，让某个人加入团队的凭证)。

## 9. 记录与恢复

- 每个向导实例默认写一份 `docs/design/<team-slug>-team.md`（同 team-grill 记录 schema），可 `--resume` 续做；
- 可选 `--no-record` 不建过程记录（仍写产物 TEAM.md）。

## 10. 实现落点

| 模块 | 改动 |
|---|---|
| `opencode-plugin/assets/command/team-start.md`（+.en.md） | 重写为"分步向导"提示词（本文 §3-§8 作为 agent 执行依据） |
| 仓库数据 | 新增向导数据（模板、白话角色卡、术语表）便于 agent 引用：建议 `team-wizard/`（模板 yaml/md + 术语表） |
| `crates/teamx/src/teamfile.rs` | 新增 `teamfile validate` 入口（复用解析，输出结构化校验结果） |
| `crates/teamx/src/cli.rs` / commands | 增加 `teamx teamfile validate` 子命令（供向导/CI 调用） |
| 文档 | 本文件 + `docs/23` 使用说明补充 `/team start` 向导用法 |

## 11. 验收要点

1. 全新目录跑 `/team start` → 走完 7 步 → 产出可解析的 `.teamx/TEAM.md`，`teamx teamfile validate` 通过；
2. 已存在 TEAM.md → 预填 + 结束时可选「更新/覆盖」，diff 可见；
3. 校验不通过 → 步骤内联报错、结束页列缺口、不允许保存；
4. 结束页给 `/team create <团队名>`（不自动建队）；
5. 白话术语表/模板对不懂 teamx 的用户可读；
6. `--resume` 能续做上一次未完成的向导。

## 12. 关联

- `docs/design/team-definition-wizard.md`（grill 会话记录）
- `docs/34-team-clone.cn.md`、`docs/35-manual-member-cloud-node.cn.md`
- `protocols/grill-with-docs.md`、`docs/23-manual-grill-with-docs-usage.md`
- `crates/teamx/src/teamfile.rs`、`crates/teamx/src/commands.rs`
