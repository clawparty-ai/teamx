# teamx 团队 Clone（fork 一支“同构、隔离、可立即接活”的新团队）

> 流程：需求 → 设计（本文件）→ 原型/实现
> 状态：**设计（已确认，可实施）**
> 目标读者：维护者 / 实现者
> 最后更新：2026-09-04（Review A/B 决策回填）

## 0. 决策摘要
1. **owner 不变**：clone 团队的 owner = 源 owner（同一人，但跨团队是新 member uuid）
2. **成员不变**：复制源 team 所有 active 成员（user 绑定、role、is_lead 一一对应）
3. **auto approve**：clone 邀请函对 worker/个人均 auto_approve，导入即 active
4. **空仓库**：为源每个仓库建同名空 bare 仓库，不复制内容
5. **每 clone 一套独立 workspace + 进程组**（节点/本机另开目录）
6. **名字用户输入**，必须显式提供且唯一
7. **完全独立**：不继承事件/任务/提问/nudge/隧道/目标历史/旧邀请函/git 内容

## 1. 背景与目标
一个 team 同一时间只能执行一个 goal（`teams.goal_id` 单指针）。当需要「同一批人 / 同构组织 / 干净上下文」并行执行新目标时，直接在旧团队开第二个 goal 会带来上下文/权限/管理混乱。

方案：**clone** —— 复制团队的结构（人 + 角色 + 空仓库），从零状态（forming、无 goal、无事件）起步，让 clone 去执行新目标。

## 2. 术语
- 源团队（source）：被 clone 的团队
- Clone 团队（clone）：新 fork 出的团队（全新 `team_id`）
- workspace：一个含 `.teamx` 的项目目录 = 一个团队成员身份（letter 证书）
- letter / 邀请函：单次使用、含成员证书的 `teamx-inv:v1:<base64>`

## 3. 关键机制结论（实现依据）
- `teams.owner_member_id` 是 TEXT、**无 FK 到 members** → clone 可在成员导入前就把 owner 字段指向「预分配的 owner seat uuid」。
- 邀请函即身份：每张邀请函预分配 `member_id`，证书 CN=`member:<id>:<role>`；成员 import 时按 CN 认领该 seat（auto_approve → active）。→ clone **不需要预插 member 行**，逐成员 mint 邀请函即可，导入即激活。
- 角色表 `roles` 是 per-team（`UNIQUE(team_id,key)`）→ clone 复制源 approved 自定义角色。
- 仓库表 `git_repos` per-team；`create_repo` 建空 bare repo。
- “一个 workspace = 一个团队成员身份” → 管理/运行 clone 需要独立 workspace。

## 4. 公开接口

### 4.1 CLI（1 个）
```
teamx team clone --name <NAME> [--team <SRC-ID>] [--server-url <URL>] [--yes] --session <S>
```
- `--name`：必填、新团队名、全库唯一
- `--team`：源团队 id（单团队会话可省略）
- `--server-url`：写入各邀请函的 server 地址（默认当前 server）
- `--yes`：跳过「成员/仓库摘要」确认直接执行

### 4.2 RPC（1 个）
- `team.clone`（serve dispatch 新增），参数映射自 CLI。

### 4.3 内部函数（3 个）
| 函数 | 作用 |
|---|---|
| `cmd_team_clone(...)` | 总流程 |
| `clone_copy_roles(conn, src_team_id, clone_team_id)` | 复制 approved 自定义角色 |
| `mint_invite(...)`（重构自 `cmd_team_invite`） | 一次性签发一张邀请函（letter） |

### 4.4 复用（不改行为）
`resolve_actor` / `ensure_owner` / `members_for_team` / `git_service::list_repos` / `git_service::create_repo` / `resolve_user` / `role_key_from_label` / PKI 签发。

## 5. `cmd_team_clone` 实现逻辑

```
输入：name, team_opt, server_url, yes, session
```

**(0) 校验**
- `resolve_actor` 定位调用者与源团队；`ensure_owner`（owner/co-lead 才可）
- name 非空且 `SELECT COUNT(*) FROM teams WHERE name=?` == 0

**(1) 快照源数据（只读）**
- active members（state='active'，过滤 left/denied）
- approved 自定义 roles
- repos（`SELECT id,name FROM git_repos WHERE team_id=?`）
- 源 owner 成员定位：`find(m.id == src.owner_member_id)`；不在 active → Err

**(2) 建 clone team（暂不插成员）**
```
team_id  = uuid v4
owner_holder = uuid v4            // 源 owner 的新 seat uuid（owner 邀请函的 member_id）
INSERT teams(id, name, owner_member_id=owner_holder, goal_id=NULL,
             state='forming', invite_token=uuid, created_at, updated_at)
emit team.created
```

**(3) 复制角色**
对源 approved roles：`INSERT OR IGNORE INTO roles(team_id, key, label, description, state='approved', proposed_by=actor)`

**(4) 逐成员签发邀请函**
稳定顺序：owner → co-lead(is_lead) → 其余 active 成员。
对每个成员构造 `SeatPlan { src_member_id, user_id, role_key, label, is_lead, is_owner }`：
- is_owner：role_key='owner'，as_lead=true；member_id 需 == `owner_holder`
- is_lead：as_lead=true
- user 绑定：成员有 `user_id` → `resolve_user` 复用同一 user；无 → None（letter 无 user 字段，note=`unbound-seat`）
- auto_approve 恒 true
调用 `mint_invite(clone_team_id, role_key, label, desc, user_id_opt, name_hint=display_name, server_url, as_lead, auto_approve=true, by_member=actor.id)`，
记录 `src_member_id → { clone_member_id, letter, ... }`

**(5) 写回 owner**
`UPDATE teams SET owner_member_id=owner_holder WHERE id=clone_team_id`

**(6) 空仓库**
源每个 repo：`create_repo(clone_team_id, repo_name, "cloned from <src>/<repo>", actor)`（空 bare）

**(7) 审计**
在 clone team emit：`team.created` + 每张 `invitation.created`（payload 同现有 invite）

**(8) 确认机制**
- `yes=false`：**不落库**，仅返回预览摘要（源/目标/成员数/角色数/repo 数）并提示加 `--yes`
- `yes=true`：执行 (2)-(7)

**(9) 返回**
```
{ ok, team:{id,name,state:'forming',owner_member_id},
  source_team_id,
  roles_copied:N,
  members:[ { source_member_id, clone_member_id, display_name, role, is_lead,
              user_id, letter:"teamx-inv:v1:…",
              note: <null|'unbound-seat'> } ],
  repos:[{source, name}],
  next:"import letters into per-team workspaces; use a separate workspace per team"
}
```

## 6. 重构：`mint_invite`
现状 `cmd_team_invite` 内含“解析 actor/owner + mint letter + roles insert + invitations insert + emit”。clone 需要“无 actor 约束地向指定 team 签发”，因此把后段抽成：

```
fn mint_invite(
  conn: &mut Connection,
  team_id: &str, role_key: &str, label: &str, desc: Option<&str>,
  user_id_opt: Option<&str>, name_hint: Option<&str>,
  server_url: &str, as_lead: bool, auto_approve: bool, created_by: &str,
  member_id_override: Option<&str>,
) -> Result<(String /*member_id*/, String /*letter*/)>
```
- 直接接收 role_key+label（**不再 label→key 反推**）
- **不含**"仅 primary owner 可 as_lead"检查（clone 有意绕过，入口已授权）
- `server_url` 用 `resolve_server_url()`（非 127.0.0.1:5781）
- `member_id_override`：owner 用预生成的 `owner_holder` uuid
- `cmd_team_invite` 重构为「解析 + ensure_owner + as_lead 权限检查 → mint_invite」，对外行为不变（既有 invite 测试全量回归）

## 7. 不复制 / 隔离边界
不复制：events / taskx / questions / nudge / tunnels / sessions / activity / 旧邀请 / goal 历史 / git 内容。
允许重复 clone：同一源可多次 clone（不同 name）。

## 8. 节点 / 本机接入（每 clone 一套 workspace + 进程组）

### 8.1 本地（lead / 个人）
```
mkdir <parent>/<clone> && cd <clone>
team import "<personal clone letter>"
team status          # 应为 clone 团队（state forming）
team console         # 查看 clone 的 Members/tunnels
```

### 8.2 worker 节点（每 clone 一套 systemd）
- workspace：`/home/<user>/work-<clone>`（含 `.teamx`）
- 服务与隧道（端口基线 base，第 k 个 clone 取 base+k）：

| 服务 | 端口 | 隧道 |
|---|---|---|
| `opencode-serve-<clone>` | 4198+k | `opencode-<clone>` |
| `ttyd-<clone>` (attach) | 7681+k | `ttyd-<clone>` |
| `teamx-fs-<clone>` | 4197+k | `fs-<clone>` |
| `teamx-worker-<clone>` | — | — |

- 每套单元：`WorkingDirectory=work-<clone>`、`TEAMX_HOME=work-<clone>/.teamx`、`User=<user>`。
- 交付脚本 `scripts/teamx-clone-node.sh <node> <user> <clone> <port_base>`：
  scp/import letter → 生成/启动上述单元 → 校验隧道在线。
- lead 侧经 clone workspace 的 console 访问（隧道走 per-user ACL）。

## 9. 安全与运维
- 仅 owner/co-lead 可 clone
- 邀请函单次使用；无 user 绑定席位保留向后兼容（`note: unbound-seat`，可由 owner 后续补 user）
- `--yes` 机制防误操作（默认先看摘要）
- 导入失败：邀请函仍有效，可在 clone 的 `invite-list`/`invite-revoke` 管理

## 10. 测试方案

### 10.1 单元（内存 DB）
> **前置**：测试需设临时 `TEAMX_HOME` + `pki::ensure`（否则会往真实 `~/.teamx` 签证书）。
> **事务**：外层单事务（一个 `db::with_write` 包裹建队+角色+invitations+owner 回写）；PKI 证书为文件副作用（失败可能残留孤儿证书文件，DB 行可回滚）。
| # | 用例 |
|---|---|
| 1 | 普通成员 clone → Err；co-lead → Ok |
| 2 | clone 后 owner 保留：owner_member_id==minted owner seat，letter 带 as_lead+auto_approve |
| 3 | 复制 active 成员：3 active(owner/lead/worker)+1 left → clone 3，role/is_lead/user_id 对齐；left 不入 |
| 4 | 每张 letter 解码后 auto_approve==true |
| 5 | 自定义角色复制成功；built-in 不重复 |
| 6 | 源 2 repo → clone 2 同名空 bare repo |
| 7 | 独立性：源事件 seq 不增长；clone 事件仅在其 team_id |
| 8 | 同名二次 clone → Err |
| 9 | `yes=false` 只返回预览、不落库（无新 team/repo） |
| 10 | 源 owner 非 active → Err |
| 11 | 重构回归：既有 `team invite` 测试保持绿 |

### 10.2 e2e（人工/脚本）
1. clone 源团队
2. lead 新本地 workspace import personal letter → 看到 clone、owner 正确
3. 节点 `work-<clone>` import worker letter → worker 服务（新端口）在 clone 内自主执行
4. lead clone console：Members/tunnels/📁 文件浏览器正常

## 11. 文件改动
- `crates/teamx/src/cli.rs`：TeamCmd + `Clone`
- `crates/teamx/src/commands.rs`：`cmd_team_clone`、`clone_copy_roles`、重构 `mint_invite`、TeamCmd 分派、单测
- `crates/teamx/src/serve.rs`：dispatch `team.clone`
- `scripts/teamx-clone-node.sh`：节点接入 runbook
- `docs/26-manual-cloud-host-best-practice.cn.md`：追加 clone 接入段

## 12. 关联
- `docs/26-manual-cloud-host-best-practice.cn.md`（节点 workspace 部署模式）
- `docs/33-server-taskx-rpc.cn.md`（同仓库文档序列）
- `crates/teamx/src/commands.rs`（`cmd_team_invite`、`cmd_team_create`）
