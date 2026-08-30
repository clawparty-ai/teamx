# 25 — 设计方案：用户身份（user）与隧道按用户隔离

> 状态：**设计定稿（已确认，进入实现）**
> 关联：`docs/20-manual-tunnel-proxy-cli.cn.md`（隧道手册）、`docs/26-research-web-terminal-opencode.md`（web terminal 调研）、`docs/03-design-network.*.md`（网络模式 mTLS）
> 日期：2026-08-30

## 0. 问题定义

网络模式下，一个「人」可能用多台设备/多个 opencode 会话协作（例如云端一台
跑 opencode，本地一台只跑 teamx + 浏览器）。现状把身份粒度钉死在「member =
一个 opencode 会话」，导致两个问题：

1. **无法表达「同一个人的多台设备」**：云端与本地是两个 member，没有任何
   关联，也没有「user」这一层身份。
2. **隧道访问控制过粗**：`teamx tunnel forward` 的鉴权只校验「属于拥有该
   隧道的 team」，于是**团队内任意 member 都能 forward 任意 member 的隧道**。
   无法做到「同一 user 的多台设备之间无缝互访、其他 user 不能访问」。

目标：引入 **user（person）** 身份层，让证书携带 user 信息；同一 user 的
多台设备（各自仍持有独立证书，保留未来按设备 ACL 的能力）可以零配置互访
对方隧道，其他 user 默认拒绝，owner/lead 保留访问权限。

## 1. 关键代码事实（决定实现落点）

| # | 事实 | 出处 |
|---|---|---|
| K1 | 最小身份单位是 member = 一个 opencode 会话；`members` 表无任何 user 字段 | `db.rs:69` members |
| K2 | 成员 mTLS 证书 CN = `member:<member_id>:<role>`；解析走 `parse_member_cn`（`splitn(3, ':')`） | `pki.rs:163` `issue_member_cert`、`pki.rs:255` |
| K3 | 服务端从证书提取 CN，注入 `PeerIdentity`，全部鉴权点经 `parse_member_cn` 取 member_id | `serve.rs:68/71/272/481/642/782/894` |
| K4 | 邀请签发：`cmd_team_invite` 生成随机 member_id + `issue_member_cert` + letter JSON，写入 `invitations`；CLI 与网络 RPC 共用此实现 | `commands.rs:1616`、`serve.rs:1153` `team.invite` |
| K5 | 领取：`claim_invitation` 校验「证书 CN 的 member == 邀请行 member」后落库 member | `commands.rs:1845` |
| K6 | 隧道注册表 `Tunnel` 只有 `provider_member_id`，无 user、无 ACL | `tunnel.rs:81` |
| K7 | 隧道鉴权：provider 侧要求「恰好属于一个 team」；consumer 侧只校验「属于该隧道所在 team」 | `serve.rs:306`、`serve.rs:513-545` |
| K8 | owner/lead 判定已存在（`owner_member_id` 或 `is_lead=1`） | `commands.rs:3144` `is_lead` |
| K9 | `instance_id` 是**机器**级标识，仅用于 activity 审计，不参与鉴权，且云端/本地不同机 | `db.rs:40` |

结论：**证书 CN 是唯一的身份载体与鉴权入口（K2/K3）**，把 user 放进 CN 即可
在单点（`parse_member_cn`）打通全部鉴权路径；`cmd_team_invite`（K4）是唯一
签发点，适合就地做 user 的自动创建/复用。

## 2. 身份模型

两层身份，都写入证书 CN：

| 概念 | 含义 | 粒度 | 证书表达 |
|---|---|---|---|
| member（既有） | 一个设备/进程上的一个 agent | 每张证书唯一 | `member:<member_id>:<role>` |
| user（新增） | 一个人（跨多台设备） | 多张证书共享 | CN 第 4 段 |

### 2.1 证书格式（决策：CN 扩展）

```
member:<member_id>:<role>[:<user_id>]
```

- 有 user_id：4 段；`user_id` 为空（未绑定）时仍签发老式 3 段 CN。
- 解析：`parse_member_cn` 改为 `splitn(4, ':')`，返回 `(member_id, role)`
  签名不变（忽略第 4 段），**5 个旧调用点零改动**；新增
  `parse_member_identity(cn) -> Option<(member_id, role, Option<user_id>)>`
  供隧道的两个 handler 使用。
- 运行时 ACL 以「证书里是否存在 user_id」为准（3 段 = 老成员 → 团队级；
  4 段 = 已绑定 → 用户级），老证书/老团队行为完全不变。

## 3. 数据模型（db.rs 下一版本迁移）

```sql
CREATE TABLE users (
  id           TEXT PRIMARY KEY,
  display_name TEXT NOT NULL UNIQUE,
  email        TEXT,
  created_by   TEXT NOT NULL,
  created_at   TEXT NOT NULL,
  updated_at   TEXT NOT NULL
);

ALTER TABLE members     ADD COLUMN user_id TEXT NOT NULL DEFAULT '';
ALTER TABLE invitations ADD COLUMN user_id TEXT NOT NULL DEFAULT '';
```

- `members.user_id` 仅做台账/审计，**不参与 ACL 判定**（判定看证书）。
- 不回填：老成员 user_id 留空 = 团队级（现状），零破坏。
- `users` 全局（跨 team）：同一人在不同 team 的设备共享同一 user_id。

## 4. 邀请流程（决策：owner 指定 user-name，自动建/复用）

```
teamx team invite "开发: 云端 opencode 设备" --user-name "张三"   # 设备1
teamx team invite "开发: 本地浏览器设备"     --user-name "张三"   # 设备2 → 复用同一 user_id
```

`cmd_team_invite` + `TeamCmd::Invite` + RPC `team.invite` 各加 `--user-name`
与显式 `--user <id>`：

- 给 `--user <id>` → 按 id 查 `users`，无则报错。
- 给 `--user-name <n>` → 按 `display_name` 精确查，存在则复用 user_id，
  不存在则 `INSERT users`（created_by = owner）再签发。
- 都不给 → 老行为（证书无 user，团队级）。

letter JSON 增加可选 `"user": {"id": ..., "name": ...}`，内部 version 提到 2
（`decode_letter` 兼容 1/2，`teamx-inv:v1:` 前缀不变）。`claim_invitation`
落库 `members.user_id`，并校验证书 CN 的 user == letter/row 的 user（类比
现有 member 校验，K5）。新增 `teamx user list`（owner/lead，审计）。

## 5. 隧道 ACL（决策：user-only + owner/lead 例外）

`Tunnel` 增加 `provider_user_id: Option<String>`（3 段老 CN → None）。

`handle_tunnel_forward` 在现有 team 校验之后，追加：

```
放行 ⟺  provider_user_id 为 None            （老隧道 → 团队级，兼容）
     ∨  consumer_user == provider_user_id    （同一 user 跨设备，零配置）
     ∨  consumer 是该 team 的 owner/lead     （复用 is_lead，保住 ui web terminal）
否则 拒绝 "tunnel 属于其他用户"
```

- 成员之间默认按 user 隔离；owner/lead 例外保证 `teamx ui` 的 owner web
  terminal（owner 通过隧道访问 member 的 `opencode serve`）不回归。
- provider 侧 `handle_tunnel_ws` 用 `parse_member_identity` 填 `provider_user_id`。
- `tunnel list/status` 输出 `provider_user_id` 与 `yours`（是否本 user 可直连）。

## 6. 改动文件清单

| 文件 | 改动 |
|---|---|
| `crates/teamx/src/pki.rs` | `issue_member_cert` 加 `user_id` 参数/CN；`parse_member_cn` 改 splitn(4)；新增 `parse_member_identity`；单测 |
| `crates/teamx/src/db.rs` | `users` 表 + 2 个 ALTER + 迁移号；单测 |
| `crates/teamx/src/commands.rs` | `cmd_user_list`；`cmd_team_invite` 加 user 解析；`claim_invitation` 落库+校验；invite-list 显示 user |
| `crates/teamx/src/cli.rs` | `TeamCmd::Invite` 加 `--user-name/--user`；新增 `teamx user` 子命令 |
| `crates/teamx/src/serve.rs` | RPC 映射；`handle_tunnel_ws` 填 provider_user_id；`handle_tunnel_forward` user 校验；status/list 输出 |
| `crates/teamx/src/tunnel.rs` | `Tunnel.provider_user_id`、`register` 参数、`list` 输出 |
| `docs/20-manual-tunnel-proxy-cli.cn.md` | 补 ACL 一节 |

## 7. 兼容性与迁移

- 老证书（3 段 CN）：无 user → 团队级访问，与现状一致。
- 老 letter：无 `user` 字段，`decode_letter` 兼容；领取时 user_id 留空。
- 老团队：所有 member 均无 user，隧道仍团队级，无行为变化。
- 已签发但未使用的邀请：`invitations.user_id` 留空，领取时按 letter 决定。

## 8. 已知限制

- `--user-name` 按名字精确匹配；同名不同人会合并为同一 user。如需严格区分，
  用显式 `--user <id>`（后续可加 email 维度）。
- 隧道「只读列出」仍团队级（成员可看到团队内所有隧道名），仅 forward 访问
  按 user 隔离；如需隐藏列表可后续加 ACL。

## 9. 验证

- `cargo build -p teamx`；`cargo test -p teamx`（新增用例全绿）；
  `cargo clippy --all-targets -- -D warnings` 干净。
- 新增单测：pki 3/4 段 CN 解析；invite `--user-name` 首建/复用；claim 落库+
  校验；隧道 ACL（同 user 放行 / 异 user 拒绝 / owner·lead 放行 / 老成员团队级）。
- 手动链路：建 user → 发两设备信 → 两端 import/批准 → 设备1 expose、设备2
  forward 互访；第三人 forward 拒绝；owner forward 放行；老证书行为不变。
