# teamx local-team：一键自举本地团队 server + 成员（auto-approve）

> 目标：在任意 workspace，`teamx local-team up` 让该目录成为一支**本机自包含团队**——没有 serve 就启动一个，有 serve（`TEAMX_SERVER_URL` 可达）就加入；从 `.teamx/TEAM.md` 自动建队、给每个 agent 成员发 letter + 导入 + **auto-approve**，然后为每位成员拉起 headless opencode + 网页 TUI，达成"一键开始工作"。
> 状态：**设计定稿（待测试方案 → 开发）**；更新：2026-09-09
> 前文：`docs/50-design-local-team.cn.md`（进程/人设/仓库/TUI）——本设计在其上增加**server 自举与成员身份激活**。

## 1. 为什么
`docs/50` 的 local-team 已能拉起"本机多 opencode 进程"，但那些进程**没有团队身份**，收不到团队任务。真正"一键开始工作"需要：
1. 一个团队 server（权威 DB / CA / 邀请函签发）
2. 每个 agent 成员以**有效成员身份**（letter → active）连上它
3. auto-approve，零人工审批

`team create` 已内置 TEAM.md bootstrap：goal + 每成员 letter（存 `.teamx/members/<key>/invitation.letter`）+ AGENTS + workdir。`local-team` 复用这条链，把缺的"serve 自举 + import + approve"补齐。

## 2. 模型

```
workspace (执行 local-team 的目录)
├── .teamx/
│   ├── teamx.db                 # 本地权威 DB（serve 使用）
│   ├── ca/                      # 本实例 CA（serve 首启生成）
│   ├── TEAM.md
│   ├── members/<key>/invitation.letter   # team create bootstrap 签发
│   └── local-team/<key>/        # 每成员：.teamx(证书)+AGENTS.md(+repo)
└── 后台 serve 进程 127.0.0.1:<port>（由 local-team up 拉起；pid 记入 server.pid）
```

## 3. `teamx local-team up` 流程（一次完成）

### 3.1 确保 server（复用或自举）
1. 读 `TEAMX_SERVER_URL`；若可达（`/health` ok）→ **加入**该 serve（外部/已有团队）。
2. 否则本 workspace **自举 serve**：找空闲端口（默认 5781 起自动递增），后台 `teamx serve --addr 127.0.0.1 --port <p> --db .teamx/teamx.db`；写 `.teamx/local-team/server.pid` 与 `server.url`。

### 3.2 建队（若尚无团队）
- owner 会话 `local-owner`（本地）`team create`（如已存在 team 则复用 team_id）。
- `team create` 自动检测 `.teamx/TEAM.md` → bootstrap：goal + 每成员 invitation letter 落盘 + AGENTS + workdir（既有逻辑）。

### 3.3 成员身份激活（对每个非 owner 成员）
1. 读 `.teamx/members/<key>/invitation.letter`
2. 在成员工作区 `.teamx/local-team/<key>/.teamx` 执行 `team import <letter>`（本地 DB claim 认领 / 存证书）
3. **auto-approve**：owner 本地 `team.approve <member_id>` → 成员 active（零人工）
4. 成员目录含 `.teamx/letters` → agent headless opencode 启动即用该身份连 serve（网络模式收任务）

### 3.4 起 agent（复用 docs/50）
persona + headless opencode + ttyd + 可选 repo clone。

### 3.5 汇总
每成员 `{key, role, active, port, ttyd_url, pid, member_id}`。

## 4. CLI 增补
- `up` 新增 `--server-url`（复用现有 `--server`? 统一用 `--server-url`，缺省 `TEAMX_SERVER_URL` → 本 ws 自举）；`--session <owner>`（缺省 `local-owner`）。
- `ps` / `stop` 保持；`stop` 额外停自举的 serve（若 server.pid 属于本 ws）。

## 5. 约束与降级
- **外部 serve 已存在**：up 加入它（在它的 DB/CA 下操作）。要求 TEAM.md 的 owner 恰好是当前可用会话；若不是，报清晰错误提示。
- **agent 无网络/无 opencode**：进程仍可起（headless 失败则提示），身份已 active。
- 本 ws 自举 serve 时：serve 以 `--db .teamx/teamx.db` 独立运行，不影响 `~/.teamx` 全局。

## 6. 相关
- `docs/50-design-local-team.cn.md`（进程与人设层）
- `docs/04-design-team-invite.cn.md`（邀请函/审批）
- `commands.rs bootstrap_from_teamfile`（已有链）
