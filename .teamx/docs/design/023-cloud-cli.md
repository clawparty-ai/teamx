# 023 · teamx cloud CLI：把所有 teamx cloud 操作收敛到 `teamx cloud …`

> 状态：设计稿（待实施）。目标：**消除对 sqlite / kubectl / console-HTTP / AWS CLI 的手工依赖**——
> 对 teamx cloud（平台 + 租户 + 云主机）的日常操作全部有 CLI 子命令。
> 维护：owner 台账仓 `.teamx/docs/design/023-cloud-cli.md` + 本仓镜像（两处 diff 必须为 0）。

## 0. 背景与目标

现状：teamx cloud 的操作面很分散——
- **已有 CLI**：`platform user/admins/audit/bootstrap`、`tenant create/destroy/reissue-members/ca-audit/owner-letter/set-endpoint`、`node provision/provision-cloud/stop/start/destroy`、`discovery`、`git`、`tcoin/credit`。
- **仍靠手工**：平台 `teams` 行、`tenant_owners`、`team_join_requests`（补录）、租户内**建席位+发信+激活成员**、**换 CA**、**启停租户**、**暴露子域**、成员 Pod 运维、EBS 快照/SG/EIP——本会话大量用 sqlite/kubectl/console-HTTP/AWS 完成。

目标：**一个 `teamx cloud` 入口**，覆盖上述全部，且：
- 人机双模（默认表格，`--json` 机器可读）；
- **远端优先**（有登录态走平台 HTTP API），基础设施类自动走 kubeconfig/AWS（"operator 模式"）；
- 破坏性操作需 `--yes`，`destroy` 默认先快照/备份；支持 `--dry-run`；
- 幂等；`--team` 接受 name 或 id。

## 1. 认证与会话

```
teamx cloud login [--server https://teamx.beer] [--name caishu]
                  [--password <pw> | --password-stdin | --token <t> | --github]
teamx cloud logout [--server <url>]
teamx cloud whoami [--json]
teamx cloud status              # 版本 / 登录态 / 租户数 / 节点数
```

- 登录 → `POST /api/login/password`，会话 token 存 `~/.teamx/cloud.json`（按 server 分组，0600）。
- 覆盖：`TEAMX_CLOUD_TOKEN` / `TEAMX_CLOUD_URL`。
- 远端命令无登录态时报错并提示 `teamx cloud login`；基础设施命令无登录态但本机有 kubeconfig 时标注并继续（operator 模式）。

## 2. 命令树

```
teamx cloud
├── login | logout | whoami | status
├── user      # 平台用户（人）
│   ├── list
│   ├── create <name> --password <pw> [--quota <n>]
│   ├── passwd <user> --password <pw>        # 重置（吊销其会话）
│   ├── disable <user> | enable <user>
│   └── quota <user> <n>
├── team      # 平台 team 元数据（teams 表）
│   ├── list [--state active|destroyed…]
│   ├── show <team>
│   ├── create <name> [--owner <user>] [--visibility …] [--join-policy …]
│   ├── rename <team> <new-name>
│   ├── set-owner <team> --user <user>
│   └── destroy <team> [--yes]
├── tenant    # 租户 = 一个 team 的运行环境
│   ├── list | show <team>
│   ├── create <team> [--owner-user <u>] [--pvc-size 20Gi] [--server-image <img>]
│   ├── start <team> [--members none|owner|all]
│   ├── stop  <team> [--keep-server]
│   ├── status <team>
│   ├── destroy <team> [--yes] [--snapshot]
│   ├── endpoint <team> [--set <url> | --clear]
│   ├── expose <team> [--domain <d>] [--node-port auto|<n>] [--tls auto|none]
│   ├── unexpose <team>
│   ├── ca
│   │   ├── show <team>
│   │   ├── import <team> --ca-cert <pem> --ca-key <pem>   # 换外部 CA（复用旧信）
│   │   └── audit <team> [--server-url <url>]
│   └── member
│       ├── list <team>
│       ├── add <team> --name <n> [--role <r>] [--user <user>] [--auto-approve]
│       ├── activate <team> --member <id>
│       ├── remove <team> --member <id>
│       ├── letter <team> --member <id> [--save <file> | --print]
│       ├── grant-repo <team> --member <id> [--repo <name>] [--perm read|write|admin]
│       └── provision <team> (--member <id> | --all) [--pvc-size 30Gi] [--image <img>]
├── join      # 加入申请（含迁移补录）
│   ├── list <team> [--status pending|approved|left]
│   ├── import <team> --user <u> --member <id> [--status approved] [--decided-by migration]
│   ├── approve <req-id> | deny <req-id> [--reason <r>]
│   └── withdraw <req-id>
├── node      # AWS 云主机（= 现 teamx node）
│   ├── list | show <id> | ssh <id>
│   ├── provision <member> [--provider aws] [--region …] [--size …] [--ttl …]
│   ├── stop <id> | start <id>
│   ├── snapshot <id> | snapshot --all       # 销毁前 EBS 快照（含完成等待）
│   ├── release-eip <alloc-id>
│   └── destroy <id> [--yes] [--snapshot]
├── bootstrap # 平台初始化
│   ├── token | redeem <token> --user <u> [--name <n>]
│   └── admins | admin-revoke <user>
└── console   # 打开平台控制台
```

**兼容**：`teamx tenant …` ≡ `teamx cloud tenant …`；`teamx node …` ≡ `teamx cloud node …`；
`teamx platform user …` ≡ `teamx cloud user …`。旧命令保留，内部转调并打印一行 deprecation 提示（`--quiet` 可关）。

## 3. 关键命令与示例

```bash
# 登录
teamx cloud login --name caishu
teamx cloud whoami --json

# 建队 → 建租户 → 暴露 → 成员
teamx cloud team create starnet --owner caishu
teamx cloud tenant create starnet --pvc-size 40Gi
teamx cloud tenant expose starnet --domain starnet.teamx.beer
teamx cloud tenant member add starnet --name alice --role contributor
teamx cloud tenant member list starnet --json
teamx cloud tenant member provision starnet --all
teamx cloud tenant status starnet

# 迁移补录
teamx cloud join import yongyan-team --user caishu --member <seat-id> --status approved
teamx cloud tenant member activate yongyan-team --member <seat-id>
teamx cloud tenant member letter yongyan-team --member <seat-id> --save ./alice.letter
teamx cloud tenant member grant-repo yongyan-team --member <seat-id> --repo powernano-v2 --perm write

# 启停
teamx cloud tenant stop starnet
teamx cloud tenant start starnet --members all

# 换 CA（复用旧信迁移）
teamx cloud tenant ca import starnet --ca-cert ./hk-ca.crt --ca-key ./hk-ca.key

# 销毁（先快照）
teamx cloud node snapshot --all
teamx cloud node destroy <id> --yes
teamx cloud tenant destroy starnet --yes --snapshot
```

## 4. 与现有实现的关系

| 新命令 | 复用/新增 |
|---|---|
| `login/logout/whoami/status` | 新增（平台 HTTP `/api/login*` + `~/.teamx/cloud.json`） |
| `user *` | 复用 `platform user *` |
| `team *` | 新增（平台 `teams` / `tenant_owners` CRUD） |
| `tenant create/destroy/reissue-members/ca-audit/owner-letter/set-endpoint` | 复用 `provision::*` |
| `tenant start/stop/status/expose/unexpose` | 新增（k8s scale/Pod + NodePort/nginx/certbot 封装） |
| `tenant ca show/import/audit` | 新增（Secret 换 CA + server cert 重签 + 平台指纹同步） |
| `tenant member add/letter/provision` | 复用服务车道 `team.invite`/`team.reissue-letter` + `node provision-cloud` |
| `tenant member activate/grant-repo/list/remove` | 新增（members/git_repo_permissions + RPC） |
| `join *` | 新增（`team_join_requests` 补录 + `discovery` 审批复用） |
| `node *` | 复用 `teamx node`，新增 `snapshot`/`release-eip` |
| `bootstrap *` | 复用 `platform bootstrap-token/redeem/admins` |

## 5. 约定

- **输出**：默认表；`--json` 统一 `{ok, data|error}`；错误码稳定（`NOT_FOUND/CONFLICT/FORBIDDEN/…`）。
- **破坏性**：无 `--yes` 时打印影响并要求确认；`destroy` 默认先快照/备份（`--no-snapshot` 可关）。
- **幂等**：`create/start/stop/expose/import` 重复调用安全。
- **审计**：写操作在平台或租户台账留痕（`platform::audit` / `events::emit`）。
- **远端/本地**：`--server` 走 HTTP API；基础设施命令自动探测 kubeconfig，标注 operator 模式。
- **命名解析**：`--team` 接受 `name|id`；`--tenant` 为别名。

## 6. 实施分期

- **P0**：`cloud login/logout/whoami/status`（认证 + config）。
- **P1**：只读——`team/tenant/user/member/join/node` 的 `list/show/status`。
- **P2**：加法写——`user create/passwd/quota`、`team create/rename/set-owner`、`tenant create/start/stop`、`member add/activate/letter/grant-repo/provision`、`join import`。
- **P3**：基础设施——`tenant expose/unexpose`、`tenant ca import/audit`、`node snapshot/release-eip`。
- **P4**：破坏性——`team/tenant/node destroy`（快照 + 确认）。

## 7. 验收

- `teamx cloud` 覆盖本文件条目；旧命令兼容（转调 + deprecation）。
- 单测：CLI 参数解析、config 读写、幂等、`--dry-run`/`--yes` 行为；现有 473 测试保持全绿、clippy `-D warnings` 干净。
- 真机：用 `teamx cloud` 重放本会话的关键操作（建租户/发信/启停/暴露/换 CA/补录 join/铺成员），全程不手敲 sqlite/kubectl。
