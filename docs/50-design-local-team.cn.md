# teamx local-team：从 TEAM.md 一键拉起本地单机多 agent

> 目标：在含 `.teamx/TEAM.md` 的工作区，`teamx local-team` 用一条命令把 TEAM.md 定义的全部成员在本机拉起为**开箱即用**的 AI 成员（headless opencode + 网页 TUI + 仓库 + 人设），组成一支"单机多 agent"的本地虚拟团队。
> 状态：**设计定稿（待测试方案 → 开发）**；更新：2026-09-09

## 1. 为什么

`teamx agents` 负责进程级 spawn/list/stop 单机多 `opencode serve`；`teamfile` 解析 `.teamx/TEAM.md`。缺一个把它们**组合成"开箱即用团队"**的一键入口：用户不想逐个建目录、写人设、克隆仓库、起 ttyd——只想 `teamx local-team` 后整支队伍能干活。

## 2. 目标 / 非目标

**目标**：一条 `teamx local-team`（可子命令）从 TEAM.md 完成：
1. 解析成员（复用 teamfile）
2. 为每个 agent 成员建独立工作目录并写 persona（`AGENTS.md`）
3. 克隆团队共享 git 仓库到工作区（若仓库可 clone / 已配置身份）
4. 启动该成员的 headless `opencode serve`（复用 agents spawn 逻辑）
5. 启动网页 TUI（ttyd → `opencode attach`）以浏览器操作
6. 汇总状态：每成员 name / port / dir / ttyd URL / pid

**非目标**：不做云/多机编排（那是 clone+节点）；不做成员身份联网自动 join（每工作区如需团队身份仍走既有 `team import` / `git setup`）；不取代 `teamx agents`（local-team 是更高层的"团队一键启动"）。

## 3. 复用与新增

| 资产 | 复用点 |
|---|---|
| `teamfile.rs` | `load_and_validate_team_file` → TeamFile.members（含 key/display_name/role/description/skills/outputs） |
| `agents.rs` | spawn/list/stop 逻辑（内部 fn 需**暴露 pub 包装** `spawn_agent` / `list_agents` / `stop_agent`） |
| `git_client.rs` | clone 到工作目录（若存在可用仓库/身份） |
| opencode + ttyd | headless serve + ttyd attach（本机需有二进制；缺则提示降级为仅 headless + `opencode` 直连说明） |

## 4. CLI 设计

顶层子命令 `teamx local-team`：

```
teamx local-team up     [--team-file <path>] [--repo <name>] [--server <url>] [--only <k1,k2>]
                        [--port-base 4200] [--ttyd-port-base 7680] [--no-ttyd] [--no-git]
teamx local-team ps              # 列出 local-team 成员（含 headless/ttyd 状态与 URL）
teamx local-team stop [--name <key>] [--all]
teamx local-team logs [--name <key>]
```

- 默认读取工作区 `.teamx/TEAM.md`；`--repo`/`--server` 用于克隆共享仓库（缺省则跳过 git，只在 dir 写 persona）。
- 成员过滤：默认起 TEAM.md 中 `role != owner` 的成员（owner 由主机会话充当）；`--only` 可指定任意 key（含 owner）。
- 每个成员的布局：
  ```
  <workbase>/local-team/<member.key>/
      AGENTS.md            # persona: 你是 <display_name>（<role>），职责/技能/输出 + 团队背景/目标/规则
      <repo>/             # 若 --repo 且可 clone
  ```
- headless 端口：从 `--port-base` 起自动避让；ttyd 端口：从 `--ttyd-port-base` 起（每成员 +1 且查空闲）。
- 输出 JSON 或人类可读：每成员 `{key, role, dir, port, ttyd_url, pid}`。

## 5. persona 生成（AGENTS.md）

在每个成员目录写：
- 头部：`你是 <display_name>（角色 <role>）`
- 从 TEAM.md：背景 + 该成员分工 description + 技能 skills + 输出 outputs
- 团队工作规则摘录（若 TEAM.md 有 `## 工作规则`）
- 工作区提示（仓库路径 / 提交约定）
这样 headless opencode 一启动就知道自己是谁、干什么、产出放哪。

## 6. 组合到 opencode plugin

- `agents up` 拉起的仍是进程；local-team 是"建好一个可开工工作区的进程"。
- 若工作区已 `team import`（成员身份 letter），实例经 plugin 自动成为 teamx 成员收任务；local-team 不重复做身份导入。
- plugin 端无需新增：local-team 产出的是标准 headless opencode 工作区（AGENTS.md + 可选仓库），plugin 正常识别。

## 7. 约束与降级
- 本机缺 `ttyd`：`--no-ttyd` 或自动降级为仅 headless + 提示 `opencode attach http://127.0.0.1:<port>`。
- 本机缺 `opencode`：报错并提示安装。
- 无 TEAM.md：报错。
- TEAM.md 无成员：报错。
- 仓库 clone 失败：不阻塞（警告并继续，仅无 repo 目录）。

## 8. 相关
- `docs/42-design-teamx-agents.cn.md`（agents 进程管理）
- `docs/30-cloud-node-opencode-headless-tui.cn.md`（headless+ttyd 架构参考）
- `teamfile.rs`
