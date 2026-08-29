# teamx Document 体系设计：TEAM.md 驱动的项目/过程/组织过程资产

> 状态：**设计中**（main 分支）
> 目标读者：team lead（owner）、团队成员、实现者
> 关联：`crates/teamx/src/teamfile.rs`（TEAM.md 解析器）、`crates/teamx/src/events.rs`（事件台账）、`crates/teamx/src/commands.rs`（bootstrap）、`docs/05-design-teamfile.cn.md`（TEAM.md 基础设计）
> 目标：项目管理、过程管理、组织过程资产形成

---

## 0. 背景与目标

`TEAM.md` 目前描述团队的**背景 / 目标 / 成员**。本设计在其上新增 **`## 文档`** 章节，声明项目全生命周期需要用到的所有文档：

- **交付类文档**：需求文档、需求分析、设计（概要/详细设计）、测试方案与报告等；
- **流程控制类文档**：issue、PR/MR、release-note 等。

每份文档不只是"模板"，还声明**流转规则**：

- 什么角色可以**创建**文档；
- 文档的 **owner** 是谁；
- 文档状态如何**流转**（动态状态机）；
- 文档**发生变更时谁应做出什么反应**（事件 → 定向响应）。

设计目标：

1. **项目管理**：通过文档状态跟踪项目交付进度；
2. **过程管理**：固化"谁创建 / 谁审批 / 谁执行 / 谁关闭"的过程约束；
3. **组织过程资产**：`.teamx/docs/` 中沉淀可复用的模板、文档实例与审计链（事件台账）。

---

## 1. 核心决策（已确认）

| # | 决策点 | 结论 |
|---|--------|------|
| 1 | `.teamx/docs/` 是否 git 跟踪 | **是**。文档实例与模板是组织过程资产，应进版本库（与 `members/` 的私钥不同，members 保持 gitignore） |
| 2 | 事件命名空间 | 先用通用事件：`doc.created` / `doc.updated` / `doc.reviewed` / `doc.approved` / `doc.rejected` / `doc.reopened` / `doc.closed`，后续按需细分（如 `issue.opened` / `pr.merged`） |
| 3 | 模板机制 | 每份文档在 TEAM.md 中声明自己的 **模板**（必要章节列表）；`doc create` 按模板生成骨架；**无模板则用户自由创建** |
| 4 | 状态机 | **只做声明式**：状态流在 TEAM.md 中动态定义（`状态流: a -> b -> c`）。不同 team 的 TEAM.md 定义不同文档 → 状态机是**动态的**，不进 `state.rs` 硬编码 |
| 5 | CLI | **不新增 `teamx doc` CLI 子命令**。文档在 TEAM.md 中动态定义，CLI 只处理 teamx 核心功能；文档的创建/流转/反应由 **agent（成员会话）按 TEAM.md 契约执行**，通过现有 `publish` 事件机制驱动 |

---

## 2. TEAM.md 中 `## 文档` 章节格式

文件位置：**项目根 `.teamx/TEAM.md`**，新增 `## 文档`（`## Docs` / `## Documents`）章节。

```markdown
## 文档

### requirements
- 标题: 需求文档
- 用途: 定义产品需求与验收标准，是设计/开发/测试的依据
- 模板: 背景 | 目标 | 用户故事 | 验收标准 | 变更记录
- 创建者: [pm]
- 所有者: pm
- 审批者: [reviewer, owner]
- 状态流: draft -> review -> approved -> done
- 变更响应:
    - on created: 通知 pm 细化需求
    - on updated: 通知所有审批者复审
    - on approved: 通知 ui-dev 与 java-dev 开始设计

### issue
- 标题: 缺陷 / 改进请求
- 用途: 记录缺陷与改进请求，驱动修复流程
- 模板: 描述 | 复现步骤 | 影响范围 | 优先级
- 创建者: [任意成员]
- 所有者: team-lead
- 状态流: opened -> triaged -> assigned -> fixing -> verified -> closed
- 变更响应:
    - on created: team-lead 分析并分诊（triage）
    - on triaged: 按优先级指派开发者
    - on verified: 关闭并记录到 release-note

### pr
- 标题: 代码合并请求（PR/MR）
- 用途: 代码变更的评审与合并
- 模板: 变更描述 | 关联 issue | 测试说明
- 创建者: [contributor, developer]
- 所有者: 提交者
- 审批者: [reviewer]
- 状态流: opened -> reviewing -> approved -> merged
- 变更响应:
    - on created: 通知 reviewer 评审
    - on approved: 合并并关闭关联 issue
    - on merged: 更新 release-note

### release-note
- 标题: 发布说明
- 用途: 汇总每次发布的功能、修复与已知问题
- 模板: 版本 | 日期 | 功能 | 修复 | 已知问题
- 创建者: [owner]
- 所有者: owner
- 状态流: draft -> review -> released
- 变更响应:
    - on released: 广播给全体成员
```

### 2.1 字段定义

| 字段 | 别名（中/英） | 类型 | 必填 | 说明 |
|------|--------------|------|------|------|
| `标题` | `title` | 文本 | 否 | 文档中文标题（缺省用 key） |
| `用途` | `purpose` | 文本 | 否 | 一句话说明文档目的 |
| `模板` | `template` | 章节列表 | 否 | 必要章节，用 `\|` 或 `、` 分隔；**空/缺失 = 无模板，用户自由创建** |
| `创建者` | `creators` | 角色列表 | 否 | 可创建此文档的角色（空 = 任意成员） |
| `所有者` | `owner` | 角色 | 是 | 文档 owner（变更默认接收方） |
| `审批者` | `approvers` | 角色列表 | 否 | 可推进状态（review/approve）的角色 |
| `状态流` | `states` | 状态链 | 是 | `a -> b -> c` 声明式状态机（**动态**，不进 state.rs） |
| `变更响应` | `reactions` | 规则列表 | 否 | 每个 `on <事件>` 对应一个动作 + 目标角色 |

### 2.2 变更响应格式

```markdown
- 变更响应:
    - on <事件名>: <动作描述>
    - on <事件名>: 通知 <角色> <动作描述>
```

`事件名` 对应文档生命周期事件（created/updated/reviewed/approved/rejected/reopened/closed）。`动作描述` 是人类可读的响应说明，**由 agent 按上下文执行**（如"分析 issue"、“指派开发者”）。若含 `通知 <角色>`，则向该角色发送定向事件。

---

## 3. 解析规则（teamfile.rs 扩展）

### 3.1 新章节识别

`parse_team_file_text()` 的 section 分发增加：

```rust
"文档" | "Docs" | "Documents" | "Document" => "docs"
```

### 3.2 小节解析

`## 文档` 下每个 `### <doc_key>` 是一个 `DocSpec`：

```rust
pub struct DocSpec {
    pub key: String,                 // ### key（唯一标识，事件 payload 引用）
    pub title: String,               // 标题（缺省 = key）
    pub purpose: Option<String>,     // 用途
    pub template: Vec<String>,       // 模板必要章节（空 = 无模板自由创建）
    pub creators: Vec<String>,       // 可创建角色（空 = 任意）
    pub owner: String,               // 所有者（必填）
    pub approvers: Vec<String>,      // 审批角色（空 = 仅 owner）
    pub states: Vec<String>,         // 声明式状态链（动态状态机）
    pub reactions: Vec<DocReaction>,
}

pub struct DocReaction {
    pub on: String,                  // 触发事件名（created/updated/...）
    pub to_role: Option<String>,     // 目标角色（"通知 <角色>"）
    pub action: String,              // 动作描述（agent 执行）
}
```

### 3.3 复用现有机制

- 小节分隔：复用 `### ` 前缀识别（与成员小节一致）；
- 字段行：复用 `- 字段: 值` 行解析与 `field_name()` 中英别名；
- 状态流：`状态流: a -> b -> c` 拆分为 `Vec<String>`（按 `->` / `→` / `，` 分割）；
- 列表字段（模板/创建者/审批者）：复用 `split_list()`（支持 `,` `，` `、` `;` `；` `|`）；
- 宽松解析：缺失 `标题`/`模板`/`审批者`/`变更响应` 不报错；缺失 `所有者`/`状态流` 时该文档标记为 `incomplete` 并跳过 bootstrap 实例化（但不阻塞 TEAM.md 整体解析）。

---

## 4. 数据模型扩展

`TeamFile` 增加：

```rust
pub struct TeamFile {
    pub team_name: String,
    pub background: Option<String>,
    pub goals: Vec<String>,
    pub members: Vec<MemberProfile>,
    pub docs: Vec<DocSpec>,          // 新增
}
```

---

## 5. 文档实例工作目录（组织过程资产）

每个文档**实例**落在 `.teamx/docs/<doc_key>/` 下（与 `members/` 并列）：

```
<project>/
├── .teamx/
│   ├── TEAM.md                       # 契约声明（含 ## 文档）
│   ├── docs/
│   │   ├── requirements/
│   │   │   ├── 001-order-flow.md     # 文档实例（按模板生成骨架）
│   │   │   └── .meta.json            # 状态: draft + owner + 关联事件 seq
│   │   ├── issues/
│   │   │   ├── 042-slow-upload.md
│   │   │   └── .meta.json
│   │   └── _spec/                    # （可选）从 TEAM.md 导出的文档契约快照
│   │       └── requirements.json
│   └── members/                      # （gitignored，含私钥）
└── AGENTS.md
```

### 5.1 git 跟踪规则

| 路径 | 跟踪 | 原因 |
|------|------|------|
| `.teamx/docs/**` | **是** | 组织过程资产：模板快照、文档实例、审计链 |
| `.teamx/TEAM.md` | 是 | 团队契约源 |
| `.teamx/members/**` | **否** | 含 invitation.letter 私钥（现有 `.gitignore: .teamx/members/`） |
| `.teamx/*.db` | 否 | 运行库（现有 `*.db` 规则） |

### 5.2 `.meta.json` 格式

```json
{
  "doc": "requirements",
  "id": "001-order-flow",
  "state": "draft",
  "owner": "pm",
  "created_at": "2026-08-28T...",
  "updated_at": "2026-08-28T...",
  "history": [
    { "state": "draft", "by": "pm", "at": "2026-08-28T...", "event_seq": 12 }
  ]
}
```

状态变更与审计链以事件台账（`events.rs`）为准，`.meta.json` 只是便捷镜像（可从事件重建）。

---

## 6. 运行时流转（声明式 · 动态状态机）

### 6.1 原则

- **不新增 `state.rs` 硬编码状态机**。每个文档的状态流来自 TEAM.md 的 `状态流` 字段 → 动态状态机；
- 状态推进通过 **`publish` 事件**（`doc.reviewed` / `doc.approved` / ...）记录，agent 依据 TEAM.md 契约执行动作；
- **变更响应 = 定向事件 + auto-execute**：复用现有 `publish --assignee` 机制，唤醒目标角色会话。

### 6.2 事件命名空间（决策点 2）

| 事件 | 语义 | 典型触发者 |
|------|------|-----------|
| `doc.created` | 文档实例创建 | 创建者 |
| `doc.updated` | 内容更新 | 成员 |
| `doc.reviewed` | 评审意见 | 审批者 |
| `doc.approved` | 通过 | 审批者 |
| `doc.rejected` | 驳回（回到前序状态） | 审批者 |
| `doc.reopened` | 重新打开 | owner |
| `doc.closed` | 关闭（终态） | owner |

payload 统一：

```json
{
  "doc": "issue",
  "id": "042-slow-upload",
  "state": "triaged",
  "from": "opened",
  "by": "team-lead",
  "note": "指派给 java-dev1"
}
```

后续可按文档类型细分（`issue.opened` / `pr.merged` 等），作为向后兼容的事件别名。

### 6.3 流转闭环示例（issue）

```
1. 任意成员 创建 issue（按模板生成 042-slow-upload.md，写 .meta.json=draft）
   → 事件 doc.created → 通知 team-lead（publish --assignee team-lead）
2. team-lead 分析后分诊 → doc.reviewed（state: opened -> triaged）
   → 通知开发者（publish --assignee <developer>）
3. 开发者修复 → doc.updated + doc.closed（state: fixing -> closed）
   → 通知 team-lead 验证
4. team-lead 验证 → 记录到 release-note
```

### 6.4 校验（声明式契约）

- **创建权限**：创建者角色 ∈ `创建者` 列表（空 = 任意）；
- **推进权限**：状态推进者 ∈ `审批者`（或 owner）；
- **合法转移**：`from -> to` 必须存在于声明状态流（否则拒绝并提示）；
- 校验失败 = 返回错误，不写事件、不改 `.meta.json`。

---

## 7. 与现有系统衔接

| 现有能力 | 衔接方式 |
|---------|---------|
| `events.rs` 事件台账 | 所有 doc 事件写入 team ledger，seq 单调 → 完整审计链 |
| `broadcast.rs` + `publish --assignee` | 变更响应 → 定向通知目标角色 → auto-execute 唤醒 |
| 角色系统（`state.rs` + TEAM.md 成员角色） | 创建者/审批者/所有者全部映射到角色 key |
| `goal` 状态机 | （后续可选）`on approved` 时可推进关联 goal |
| TEAM.md bootstrap（`commands.rs`） | `team create` 时解析 `## 文档`，生成 `_spec/` 契约快照 |

### 7.1 agent 如何按契约执行

- 成员会话加载 TEAM.md（含 `## 文档`）→ 获取完整契约；
- 创建文档：按模板章节生成骨架到 `.teamx/docs/<key>/`，写 `.meta.json`，`publish` `doc.created`；
- 收到 `doc.*` 定向事件 → 按 `变更响应` 动作执行；
- 推进状态：更新 `.meta.json` + `publish` 对应事件（权限/合法性由声明式校验）。

---

## 8. 实施里程碑

| 阶段 | 内容 | 验收 |
|------|------|------|
| **T1** | `teamfile.rs`：`## 文档` 解析 + `DocSpec` 模型 + 单元测试 | 合法/非法/缺失字段 TEAM.md 均正确处理 |
| **T2** | bootstrap 集成：`team create` 解析文档契约，生成 `.teamx/docs/_spec/` 快照 | 输出含 `docs` 信息 |
| **T3** | 文档生命周期事件模型 + 声明式校验（创建/推进权限、合法转移） | 非法操作被拒绝且无事件写入 |
| **T4** | 变更响应 → 定向事件（`publish --assignee`）闭环 | issue 全流程可跑通 |
| **T5** | 端到端测试（teamfile-test.sh 扩展：TF-2xx 文档章节用例） | `tests/run-all.sh` 全绿 |

---

## 9. 待确认 / 风险

| # | 问题 | 建议 |
|---|------|------|
| Q1 | 文档实例编号规则 | `NNN-<slug>.md` 顺序编号，或按标题 slug；由创建者/agent 决定 |
| Q2 | `_spec/` 快照何时刷新 | 每次 `team create` 刷新；后续可提供"重新同步"命令 |
| Q3 | 无模板文档的状态元数据 | 仍写 `.meta.json`（状态/owner），只是不生成骨架章节 |
| Q4 | 事件细分（`issue.opened` 等）时机 | 通用事件稳定后再加别名 |
| R1 | 不同 team 状态流差异 | 声明式天然支持（每份 TEAM.md 独立定义），无迁移成本 |
| R2 | agent 不遵循契约 | 校验在 publish 层执行（权限/合法转移），不依赖 agent 自觉 |
| R3 | 文档数量增长 | `.meta.json` 按目录组织；后续可加索引 |

---

## 10. 关联文档

- `docs/05-design-teamfile.cn.md` — TEAM.md 基础设计（背景/目标/成员）
- `crates/teamx/src/teamfile.rs` — 解析器实现
- `crates/teamx/src/events.rs` — 事件台账
- `crates/teamx/src/commands.rs` — bootstrap 集成
- `templates/01-product-dev-team.TEAM.md` — 官方模板（可扩展加入 `## 文档`）
