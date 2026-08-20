# teamx TEAM.md 驱动的团队初始化（设计）

> 状态：**设计中**（main 分支）
> 目标读者：实现者、team lead（owner）、团队成员
> 关联：`crates/teamx/src/commands.rs`（cmd_team_create/cmd_team_invite）、`crates/teamx/src/pki.rs`（letter 签发）、`opencode-plugin/src/serve.ts`（内置 serve）、opencode 的 `AGENTS.md` 约定

---

## 0. 目标

在项目仓库内提供一个 **`TEAM.md`** 文件，描述项目的背景、目标、团队成员、成员的角色/分工/技能/工作输出。当创建 team 时：

1. 若 `.teamx/TEAM.md` 存在，**自动读取并解析**；
2. 根据文件内容**自动初始化项目**：
   - 创建 team（团队名、目标 goal）；
   - 启动插件内置的 `teamx serve`（network-mode server）；
   - 为每个成员签发 **invitation-letter**；
   - 为每个成员生成专属 **AGENTS.md**（融合项目根 `AGENTS.md` + TEAM.md 中该成员的描述）；
   - 为每个成员创建工作目录 **`.teamx/members/[member-name]/`**。

---

## 1. 核心决策（已确认）

| 决策点 | 结论 |
|---|---|
| 触发时机 | `team create` 时检测 `.teamx/TEAM.md`；**其他命令如何复用 TEAM.md 留作 TODO**（owner 另行设计） |
| 工作目录 | 直接用 **`.teamx/members/[member-name]/`**（不引入 workspace 子目录） |
| 成员 AGENTS.md | **融合项目根 `AGENTS.md` + TEAM.md 中该成员的描述**，生成成员专属 AGENTS.md |
| letter 处理 | **既保存文件，也打印**（保存到 `.teamx/members/[name]/invitation.letter`，同时 CLI 打印输出） |
| 启动 serve | 复用插件 `serveStart`（`.teamx` 目录内已有 serve.json 记录）；CLI 侧提示 |

---

## 2. TEAM.md 文件格式

文件位置：**项目根 `.teamx/TEAM.md`**。

```markdown
# 企业数字化平台

## 背景
围绕 Team Goal 构建团队协作平台：支持任务分派、reverse tunnel、活动/成本分析。

## 目标
- 8 月底交付 v1.0：团队协作与活动分析
- 支持跨网络 reverse tunnel

## 成员
### owner
- 姓名: 企业数字化平台
- 角色: owner
- 分工: 架构设计、目标定义、代码审查
- 技能: Rust, TypeScript, 系统设计
- 输出: 架构文档、核心代码

### 小明
- 姓名: 小明
- 角色: contributor
- 分工: 前端开发、测试
- 技能: React, TypeScript
- 输出: 看板组件、测试用例

### 小红
- 姓名: 小红
- 角色: reviewer
- 分工: 代码审查、质量保障
- 技能: Rust, 代码评审
- 输出: 审查报告
```

### 2.1 解析规则

- **团队名**：`# 标题`（第一个 `# ` 一行）。
- **背景**：`## 背景` 章节正文。
- **目标**：`## 目标` 章节的列表项（`- ` 开头），拼接为 goal body（或第一条为 goal 标题）。
- **成员**：`## 成员` 章节下每个 `### <key>` 小节。小节内 `- 字段: 值` 行：
  - `姓名` / `名字` → display_name
  - `角色` → role key（contributor/reviewer/observer/…）
  - `分工` → role description
  - `技能` → 技能列表（AGENTS.md 用）
  - `输出` → 工作输出（AGENTS.md 用）
- 宽松解析：字段名支持中英文（`姓名`/`name`、`角色`/`role`、`分工`/`description` 等）；缺失字段可空。

### 2.2 数据模型

```rust
pub struct TeamFile {
    pub team_name: String,
    pub background: Option<String>,
    pub goals: Vec<String>,
    pub members: Vec<MemberProfile>,
}

pub struct MemberProfile {
    pub key: String,          // `### key`（目录名）
    pub display_name: String, // 姓名
    pub role: Option<String>, // 角色 key
    pub description: Option<String>, // 分工
    pub skills: Vec<String>,  // 技能
    pub outputs: Vec<String>, // 输出
}
```

---

## 3. `team create` 流程增强

`teamx team create <name> [--session S]` 检测到 `.teamx/TEAM.md` 存在时：

```
1. 解析 TEAM.md → TeamFile
2. 创建团队（复用现有 cmd_team_create；team name = TEAM.md 标题，或命令行 name 覆盖）
3. 自动 goal set：
   - 标题 = 团队名（或 TEAM.md 目标第一条）
   - body = 背景 + 全部目标
4. 启动内置 teamx serve（提示/插件自动）
5. 遍历每个 member：
   a. 签发 invitation letter（复用 cmd_team_invite，role/desc 来自 TEAM.md）
      - 保存到 .teamx/members/[member-name]/invitation.letter
      - 打印 letter 到 CLI 输出
   b. 生成成员专属 AGENTS.md：
      - 读项目根 AGENTS.md（若存在）
      - 融合 TEAM.md 中该成员的 角色/分工/技能/输出
      - 写入 .teamx/members/[member-name]/AGENTS.md
   c. 创建工作目录：.teamx/members/[member-name]/（目录已存在）
6. 输出：team id、goal id、每个成员 (name, role, letter 路径)
```

### 3.1 成员 AGENTS.md 内容（融合）

```
# AGENTS.md — <display_name>（<role>）

## 来自项目根 AGENTS.md
<项目根 AGENTS.md 的内容（若存在）>

## 团队角色
- 角色: <role>
- 分工: <description>
- 技能: <skills>
- 工作输出: <outputs>

## 团队上下文
- 团队: <team_name>
- 成员目录: .teamx/members/<name>/
- 工作方式: 通过 teamx 工具同步进度、查阅团队事件
```

---

## 4. 目录结构（生成结果）

```
<project>/
├── .teamx/
│   ├── TEAM.md                    # 团队定义（源文件，用户维护）
│   ├── serve.json                 # 内置 serve 记录（插件已有）
│   ├── teamx.db                   # 本地库（serve 用）
│   └── members/
│       ├── 小明/
│       │   ├── AGENTS.md          # 小明专属 agent 指令（融合）
│       │   └── invitation.letter  # 小明邀请函（待导入）
│       └── 小红/
│           ├── AGENTS.md
│           └── invitation.letter
└── AGENTS.md                      # 项目根 AGENTS.md（若存在，融合源）
```

---

## 5. 实现模块

| 模块 | 职责 |
|---|---|
| `crates/teamx/src/teamfile.rs` | TEAM.md 解析器（`parse_team_file(path) -> Result<TeamFile>`）+ 单测 |
| `crates/teamx/src/commands.rs` | `cmd_team_create` 集成：检测 TEAM.md → 调 teamfile 解析 → 生成 goal/letters/AGENTS.md/目录 |
| `crates/teamx/src/cli.rs` | `team create` 无新参数（自动检测），输出扩展（letters/agents 路径） |
| 插件（可选） | `serveStart` 已有；创建后自动启动（后续 TODO） |

---

## 6. 实施里程碑

| 阶段 | 内容 | 验收 |
|---|---|---|
| **T1** | `teamfile.rs`：解析器 + 单元测试 | 合法/非法 TEAM.md 均正确处理 |
| **T2** | `cmd_team_create` 集成：检测 TEAM.md → 生成 goal + letters + AGENTS.md + 目录 | 创建后 `.teamx/members/*` 齐全 |
| **T3** | CLI 输出：打印每个成员 letter + 文件路径 | 命令输出完整 |
| **T4** | 端到端测试（smoke） | `tests/run-all.sh` 全绿 |

---

## 7. 待确认 / 风险

| # | 问题 | 状态 |
|---|---|---|
| Q1 | 其他命令如何复用 TEAM.md | **TODO**（owner 构思） |
| Q2 | letter 导入后是否需要从 members 目录清理 | 暂不清理（保留作为审计） |
| Q3 | 成员角色必须是内置角色？ | 允许自定义 role key（复用 role propose 流程），创建时直接写入 |
| R1 | TEAM.md 解析容错 | 缺失章节/字段宽松处理，不阻塞创建 |
| R2 | 中文文件名目录 | 支持 UTF-8 目录名（macOS/Linux） |
