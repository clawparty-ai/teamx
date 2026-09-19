# teamx 团队 Clone（fork 一支“同构、隔离、可立即接活”的新团队）

> 流程：需求 → 设计（本文件）→ 原型/实现
> 状态：**已实现**（v3 + 全部 review 修复落地，171 单测全绿）
> 目标读者：维护者 / 实现者
> 最后更新：2026-09-04（review #1~#9 修复 + server_url A 方案回填）

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
- 角色表 `roles` 是 per-team（`UNIQUE(team_id,key)`）→ clone 复制源 approved 自定义角色，并 `seed_roles` 补齐内置角色目录。
- 仓库表 `git_repos` per-team；clone 用 `create_empty_repo` 建空 bare repo（不预授权限，`created_by` = owner_holder）。
- 仓库权限在**导入时**授予（auto-approve：owner → ADMIN，其余 → WRITE），因为 clone 时成员尚不存在（`git_repo_permissions.member_id` 有 FK 到 members）。
- “一个 workspace = 一个团队成员身份” → 管理/运行 clone 需要独立 workspace。

## 4. 公开接口

### 4.1 CLI（1 个）
```
teamx team clone --name <NAME> [--team <SRC-ID>] [--server-url <URL>] [--yes] --session <S>
```
- `--name`：必填、新团队名、全库唯一
- `--team`：源团队 id（单团队会话可省略）
- `--server-url`：写入各邀请函的 server 地址（网络模式默认从请求 Host 头推导，见 §9）
- `--yes`：跳过「成员/仓库摘要」确认直接执行

### 4.2 RPC（1 个）
- `team.clone`（serve dispatch，CN 特判：从证书 member_id 解析源团队与权限）。

### 4.3 内部函数
| 函数 | 作用 |
|---|---|
| `cmd_team_clone(...)` | 总流程（预签发证书 + 单个外层事务） |
| `build_seat_plan(...)` / `SeatPlan` | 为每个源成员构造座位计划（role/desc/user 绑定） |
| `role_meta(...)` | 查角色 label + description（roles 表 → 内置目录兜底） |
| `clone_copy_roles(tx, ...)` | 复制 approved 自定义角色（`&mut Transaction`） |
| `mint_invite_cert(...)` / `mint_invite_db(tx, ...)` / `mint_invite(...)` | 证书签发与 DB 写入分离，供批量单事务复用 |
| `git_service::create_empty_repo(...)` | 建空 bare repo + DB 行，不授权限 |

### 4.4 复用（不改行为）
`resolve_actor` / `ensure_owner` / `is_team_owner` / `members_for_team` / `git_service::list_repos` / `resolve_user` / `seed_roles` / PKI 签发。

## 5. `cmd_team_clone` 实现逻辑

```
输入：name, team_opt, server_url, yes, session, actor_member_id
```

**(0) 校验 + 解析调用者**
- CN 路径（网络模式）：`actor_member_id` 来自证书；源团队用 `team_opt`（serve 传入并已校验）或兜底 `m.team_id`；`is_team_owner` 校验 owner/co-lead。
- Session 路径（本地）：`resolve_actor` + `ensure_owner`。
- name 非空且 `SELECT COUNT(*) FROM teams WHERE name=?` == 0

**(1) 快照源数据（只读）**
- active members（state='active'，过滤 left/denied）
- approved 自定义 roles
- repos（`SELECT id,name FROM git_repos WHERE team_id=?`）
- 源 owner 定位：`find(m.id == src.owner_member_id)`；不在 active → Err

**(2) 构造 SeatPlan（稳定顺序：owner → co-lead → 其余）**
每个成员 `build_seat_plan` 解析：role_key（owner 用 'owner'）、label + desc（`role_meta`）、user 绑定（复用源 `user_id` + `user.display_name`）、is_lead / is_owner。

**(3) 预签发证书（纯文件系统，无 DB 写）**
对每个 SeatPlan 调 `mint_invite_cert`，得到 `MintedInvite { member_id, invitation_id, serial_hex, cn, letter }`。owner 用 `owner_holder` 作为 member_id。

**(4) 单个外层事务（DB 全有或全无）**
```
db::with_write(conn, |tx| {
  INSERT teams(id, name, owner_member_id=NULL, 'forming', ...)
  seed_roles(tx, clone_team_id)                    // 补齐内置角色目录
  clone_copy_roles(tx, src, clone, actor)          // 复制自定义角色
  emit team.created
  for (plan, mint) in minted { mint_invite_db(tx, ...) }   // 逐成员写 invitations + emit
  UPDATE teams SET owner_member_id = owner_holder
})
```

**(5) 空仓库（事务外，文件系统 + DB 行）**
源每个 repo：`create_empty_repo(clone_team_id, repo_name, "cloned from <src>/<repo>", owner_holder)`（空 bare，`created_by` = clone owner seat，不授权限）。

**(6) 确认机制**
- `yes=false`：**不落库**，仅返回预览摘要（源/目标/成员数/角色数/repo 数）并提示加 `--yes`
- `yes=true`：执行 (2)-(5)

**(7) 返回**
```
{ ok, team:{id,name,state:'forming',owner_member_id},
  source_team_id,
  roles_copied:N,
  members:[ { source_member_id, clone_member_id, display_name, role, is_lead,
              is_owner, user_id, letter:"teamx-inv:v1:…",
              note: <null|'unbound-seat'> } ],
  repos:[{source, name}],
  next:"import letters into per-team workspaces; use a separate workspace per team"
}
```

## 6. 重构：`mint_invite`（证书与 DB 写入分离）

现状 `cmd_team_invite` 内含“解析 actor/owner + mint letter + roles insert + invitations insert + emit”。clone 需要“无 actor 约束地向指定 team 批量签发”，因此拆成三阶段：

```
fn mint_invite_cert(team_id, team_name, role_key, label, desc, user, name_hint,
                    server_url, as_lead, auto_approve, member_id_override)
    -> Result<MintedInvite>          // 纯文件系统：签证书 + 构造 letter，不碰 DB

fn mint_invite_db(tx: &mut Transaction, mint, team_id, role_key, label, desc,
                  user, as_lead, auto_approve, created_by)
    -> rusqlite::Result<()>          // 事务内：roles insert + invitations insert + emit

fn mint_invite(conn, ...) -> (member_id, invitation_id, letter)
                                     // 包装：cert + with_write(db)
```

- 直接接收 role_key + label + desc（**不再 label→key 反推**）
- **不含**"仅 primary owner 可 as_lead"检查（clone 有意绕过，入口已授权）
- `server_url` 用 `resolve_server_url_with_host()`（网络模式从请求 Host 头推导，见 §9）
- `member_id_override`：owner 用预生成的 `owner_holder` uuid
- `MintedInvite` 同时返回 `invitation_id`（修复 `team invite` 返回空 id 的回归），letter 的 `user` 对象保留 `{id, name}`
- `cmd_team_invite` 重构为「解析 + ensure_owner + as_lead 权限检查 → mint_invite」，对外行为不变

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

### 9.1 server_url（A 方案）
clone 团队与源团队**共用同一 server（一个 ip:port、一个 DB）**，团队不靠 path 区分（协作层靠 mTLS 证书身份，git 层才走 `/git/{team_id}/{repo}`）。因此每张 clone 邀请函里的 `server.url` 必须填**公网可达地址**，否则远程成员连不上。

`resolve_server_url_with_host(explicit, host)` 优先级：
1. 显式 `--server-url`
2. 请求 `Host` 头（客户端实际拨的地址，serve dispatch 从请求提取）
3. `TEAMX_SERVER_URL` 环境变量 / 已导入 letter / 兜底 127.0.0.1

这解决了「server 端没有 letter、`discover_server_url` 返回 None → 错误兜底 127.0.0.1」的问题。

> ⚠️ **在 server 机上执行 clone 时必须显式 `TEAMX_HOME=/root/.teamx`**（运行中 server 的 home）。
> 否则 clone 会落到 `~/.teamx`（例如残留的 `/home/ubuntu/.teamx` 旧 home），用**旧 CA** 给所有邀请函签证书
> —— server 只信任自己 home 的 CA，导致**整队邀请函 mTLS 握手失败**（`certificate signature failure` / TLS alert 51）。
> 仓库也会建到错误 home。排查/修复见 `docs/59-incident-clone-wrong-teamx-home.cn.md` 与 `docs/38-clone-runtime-design.cn.md` §7。

### 9.2 仓库权限（#6）
- clone 建空仓库时**不预授权限**（成员尚不存在，`git_repo_permissions.member_id` 有 FK 到 members）；`created_by` = `owner_holder`。
- 成员导入（auto-approve）时授权：**owner → ADMIN，其余 → WRITE**（`cmd_team_import`）。owner 导入后即可管理仓库/授予他人权限。

## 10. 测试方案

### 10.1 单元（内存 DB，共 13 条 clone 测试 + 全量 171 测试）
> **前置**：测试需设临时 `TEAMX_HOME` + `pki::ensure`（否则会往真实 `~/.teamx` 签证书），并用全局 `Mutex` 串行化（`TEAMX_HOME` 是进程级 env，并行测试会竞争）。
> **事务**：单个外层 `db::with_write` 包裹建队+seed_roles+复制角色+全部邀请函+owner 回写；PKI 证书为文件副作用（失败可能残留孤儿证书文件，DB 行可回滚）。

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
| 11 | 内置角色目录补齐（seed_roles）：clone 团队含全部 6 个内置角色 |
| 12 | clone letter 保留同一用户绑定（`user.id` + `user.name`） |
| 13 | clone letter 保留自定义角色 description |
| 14 | clone 仓库不预授权限：`created_by`=owner_holder、无 `git_repo_permissions` 行 |
| 15 | 重构回归：既有 `team invite` 测试保持绿 |

### 10.2 e2e（人工/脚本）
1. clone 源团队
2. lead 新本地 workspace import personal letter → 看到 clone、owner 正确
3. 节点 `work-<clone>` import worker letter → worker 服务（新端口）在 clone 内自主执行
4. lead clone console：Members/tunnels/📁 文件浏览器正常

## 11. 文件改动
- `crates/teamx/src/cli.rs`：TeamCmd + `Clone`
- `crates/teamx/src/commands.rs`：`cmd_team_clone`、`build_seat_plan`/`SeatPlan`、`role_meta`、`clone_copy_roles`、重构 `mint_invite`（cert/db 分离）、导入 auto-approve 授权（owner→ADMIN）、TeamCmd 分派、单测
- `crates/teamx/src/serve.rs`：dispatch `team.clone`（CN 特判 + Host 头推导 server_url）
- `crates/teamx/src/git_service.rs`：`create_empty_repo`（不预授权限）
- `scripts/teamx-clone-node.sh`：节点接入 runbook
- `docs/26-manual-cloud-host-best-practice.cn.md`：追加 clone 接入段

## 12. 关联
- `docs/26-manual-cloud-host-best-practice.cn.md`（节点 workspace 部署模式）
- `docs/33-server-taskx-rpc.cn.md`（同仓库文档序列）
- `crates/teamx/src/commands.rs`（`cmd_team_invite`、`cmd_team_create`）

## 13. 使用指南（What & How）

### 13.1 Clone 是什么
`team clone` 把一支团队「复制」成一支**同构、隔离、可立即开工**的新团队：

- **同构**：新团队拥有和源团队完全相同的成员（同一批人）、角色目录、lead 结构，以及**同名的空 git 仓库**（内容不复制）。
- **隔离**：新团队是全新的 `team_id`，从零状态起步（`forming`、无 goal、无事件/任务/提问/nudge/隧道历史）。
- **可立即开工**：clone 完成后，把各自的邀请函分发给成员，导入即可加入新团队并行执行新目标。

**典型场景**：一个 team 同一时间只能跑一个 goal。当「同一批人」要并行开一个新目标时，clone 一支新团队去执行，避免在旧团队上叠加上下文/权限混乱。

### 13.2 前置条件
- 你是源团队的 **owner 或 co-lead**（`ensure_owner` 校验）。
- 新团队名**全库唯一**。
- 源团队的 owner 必须是 `active` 成员（否则 clone 失败）。
- 有 server 权限（网络模式）或本地团队（本地模式）。

### 13.3 快速上手（本地 CLI）
```bash
# 1. 预览（dry-run，不落库）
teamx team clone --name my-team-v2 --session <my-session>

# 2. 确认执行（返回每张成员邀请函）
teamx team clone --name my-team-v2 --session <my-session> --yes
```

输出里 `members[]` 是**每个成员的邀请函**（`letter` 字段，`teamx-inv:v1:<base64>` 单行字符串），`repos[]` 是 clone 出来的空仓库，`team.owner_member_id` 是 clone 团队 owner 座位。

### 13.4 网络模式（RPC）
```json
{ "method": "team.clone", "args": { "name": "my-team-v2", "yes": true } }
```
- 单团队 owner 可不传 `team`；多团队需传 `team`。
- `server_url` 不必显式传：serve 会从请求的 `Host` 头推导公网地址写入邀请函（§9.1 A 方案）；也可显式传 `server_url` 覆盖。

### 13.5 分发邀请函 & 成员导入
clone 返回的每张 `letter` 对应一个成员（含 owner）。**把各自的 letter 发到对应的人/设备**，然后在各自的 workspace 导入：

```bash
# 成员侧（在独立 workspace 目录下）
teamx team import "teamx-inv:v1:…" --session <my-session>
```

- 所有 clone 邀请函 `auto_approve`，导入即 `active`，无需 owner 逐个审批。
- **同一用户**：clone 保留了源成员的 `user_id` 绑定，所以同一个人在源/clone 两队是同一个「人」（隧道身份打通）。
- owner 导入后自动获得 clone 仓库的 **ADMIN** 权限；其余成员获得 **WRITE**（§9.2）。

### 13.6 节点接入（worker）
clone 团队是独立身份，节点上要**另开一套 workspace + 进程组**（不能复用源团队的 `.teamx`）。详见 §8：

- 本地/lead：`mkdir <clone> && cd <clone>` → `team import "<letter>"` → `team console`
- worker 节点：`/home/<user>/work-<clone>` + 一套端口错开的 systemd 单元（opencode-serve / ttyd / teamx-fs / teamx-worker + 隧道）

### 13.7 注意事项 / FAQ
- **同名仓库**：clone 的仓库和源仓库同名但**内容为空**（bare），不是 fork 内容。
- **完全独立**：clone 后源团队不受影响；同一源可 clone 多次（不同 name），互不干扰。
- **owner 未导入前**：clone 团队 `state=forming`、owner 座位尚未激活，直到 owner 导入其 letter。
- **无 user 绑定的成员**：letter 不带 `user`，返回里 `note="unbound-seat"`，导入后 owner 可后续补绑 user。
- **销毁**：不满意可用 `teamx team destroy --team <clone-id>`（owner）。
- **吊销邀请函**：在 clone 团队里 `invite-list` / `invite-revoke` 管理未使用的邀请函。
- **邀请函连不上（CA 不匹配）**：若在 server 机上 clone 时忘了 `TEAMX_HOME=/root/.teamx`，证书会用旧 home 的 CA 签发，
  成员 import 后报 `certificate signature failure` / TLS alert 51。先用 `ca_fingerprint` 比对 server CA，再按
  `docs/59-incident-clone-wrong-teamx-home.cn.md` 补签 / 重 clone / 经 RPC 重发。
