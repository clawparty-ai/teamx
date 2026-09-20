# 008 — 云主机开通与运维（design/006 实施 · 使用手册）

> 状态：**已实施并在真机跑通**（2026-09-20）
> 范围：enterprise 专属（`ENT-` 前缀）。覆盖「invite 时/按需给成员开通云主机 + 节点初始化 + 服务注册 + 回收」。
> 关联：团队文档 `.teamx/docs/design/006-invite-provision-node.md`（设计与裁决 A–V）、
> `.teamx/docs/design/004-t1-provider-external.md`（成本模型）、`docs/59-incident-clone-wrong-teamx-home.cn.md`（TEAMX_HOME 事故）
> 实现：`crates/teamx/src/provision.rs`、`commands.rs`（`cmd_node_*` / `cmd_provision_seat`）、`serve.rs`（`node.*` / `member.*` RPC）

---

## 1. 能力清单

| 能力 | 入口 | 说明 |
|---|---|---|
| 开通（invite 时） | `team invite <role> --provision-node [--provider aws --region .. --size .. --ttl .. --os-user ..]` | 建座位 + 签发邀请函 + 入队异步 provision job（默认关） |
| 开通（按需） | `teamx node provision --member <id> [...]` | 给已有座位补/复用主机 |
| 复用优先 | 自动 | 同 `(team,member)` 幂等；同 `user` 已有存活主机 → 复用，不重复开 |
| 节点初始化 | server 侧 runner | SSH 推 teamx 二进制（**按节点架构选**）→ 建 OS 用户 → 装 opencode/bun/ttyd/git → 投递邀请函 → **自认领座位** → 配 git mTLS → 注册 systemd 单元（**不启动**） |
| 审批后启动 | `team approve` | 裁决 V：approve 才启动该实例的服务组（异步执行，不阻塞 RPC） |
| 查看 | `teamx node list/status`、`team status`、console | 主机/IP/OS 用户/状态/工作区 |
| 设备命名与归属 | `member rename`、`member set-user` | 一人多设备：`xiaods-mac` / `xiaods-ec2` 同属 user `xiaods` |
| 运维 | `node ssh/stop/start/auto-stop` | 停/启实例、空闲 auto-stop（两级粒度） |
| 回收 | `node destroy` | 停服务 → 删 OS 用户 → terminate → 删 keypair → 清密文 → **吊销座位** |
| 证书吊销 | 自动（`destroy`） | 无 CRL：按 seat 状态在 RPC 派发前拦截（`left`/`denied` → 拒） |

---

## 2. 端到端流程

```text
owner/lead: team invite <role> --provision-node [--os-user xiaods]
   │
   ├─ 1. 建 seat + 签发邀请函（letter 落 server 本地 0600；node_secrets 只存引用）
   ├─ 2. 入队 provision job（异步，invite 立即返回）
   ▼  server 侧 runner：teamx node run-jobs
   ├─ 3. aws ec2 run-instances（Ubuntu LTS arm64 / t4g / gp3 / 安全组只放行 server IP 连 22）
   │      └─ 等公网 IP（RunInstances 返回时 PublicIpAddress 常为 null，需轮询）
   ├─ 4. 等 SSH 通（per-node keypair；校验 host key）
   ├─ 5. 探测节点架构 → 选二进制 → `ssh 'cat > /tmp/teamx'` 推送
   ├─ 6. bootstrap 脚本（stdin 管道执行）：
   │      建 OS 用户 → 装 git/unzip/bun/opencode/ttyd → 写 letter/CA → import
   │      → 自认领 seat（team.import RPC）→ teamx git setup → 注册 6 个 systemd 单元
   ├─ 7. 节点状态 → ready
   ▼
owner: team approve <member_id>
   ├─ 8. seat pending → active（+ 自动授予团队 git 写权限）
   └─ 9. post-approve（异步）：先启动单元 → 后台补 clone 团队 repo 到 workspace
```

关键设计点：

- **server 是执行者**，节点不持有 AWS 凭据；邀请函/私钥由 server 直推（**不进 cloud-init**）。
- **bootstrap 与 post-approve 都是脚本**：由 Rust 侧生成、经 SSH 的 stdin 管道执行（不落盘、不 scp 脚本），幂等可重放。
- **座位认领在脚本内完成**（`team.import` RPC），不依赖人工或插件自动认领。
- **裁决 V**：服务只在 approve 后启动；且**先起服务再补 clone**，避免慢 clone 挡住实例可用。

---

## 3. 使用方式

### 3.1 开通（一条命令）

```bash
# invite 时开通（推荐）：建座位 + 邀请函 + 异步开主机
teamx team invite "contributor: 成员说明" \
  --name-hint xiaods \
  --provision-node \
  --provider aws --region us-west-2 --size t4g.small --ttl 30 \
  --os-user xiaods            # 可选：指定节点 OS 用户（默认 <team-slug><4位数字>）
  --session <your-session>
```

> `--os-user` 校验：1–32 字符、`[a-z0-9_-]`、首字符为字母、拒绝 `root`/`ubuntu` 等保留账号。

### 3.2 推进 job（server 侧 runner）

```bash
# 在 server 上（TEAMX_HOME/TEAMX_DB 指向该 server）
TEAMX_SERVER_PUBLIC_IP=<server 公网 IP> \
teamx node run-jobs \
  --server-url https://<server>:5783 \
  --team-repo-url https://<server>:5783/git/<team_id>/<repo> \
  --ca-file <TEAMX_HOME>/ca/ca.crt
```

- 只处理 `requested`/`failed` 状态的 job；失败会累加 `attempts` 并记录 `last_error`（不自动 terminate，裁决 U）。
- **重复执行安全**：节点已有 `instance_id` 时复用实例，不会重复 RunInstances。

### 3.3 审批（启动服务）

```bash
teamx team approve <member_id> --session <owner-session>
```

approve 后：seat 转 active、授予 git 写权限、后台启动该实例的 systemd 单元并补 clone workspace。

### 3.4 查看

```bash
teamx node list                          # 本团队所有主机
teamx node status <node_id>              # 单台
teamx team status                        # 成员列表（含 workspace 列）
teamx node ssh <node_id>                 # server 侧免密登录（server 持 per-node 私钥）
```

### 3.5 一人多设备：命名与归属

同一用户的多台设备各自是一个 member，建议按设备命名并绑定到同一个「人」：

```bash
teamx member rename   --member <id> --name xiaods-mac     # 本人可改自己；owner/co-lead 可改任意
teamx member rename   --member <id> --name xiaods-ec2
teamx member set-user --member <id> --user-name xiaods    # 两台设备 → 同一个 user
```

- `rename`：1..=64 字符；同 team 内 **live 成员不得重名**；台账事件 `member.renamed`。
- `set-user`：绑定到 `users` 表的「人」；console 会按「用户」把同人的设备归到一组；台账事件 `member.user_bound`。
- 不绑定 user 的座位在 console 里**各自单独成组**（组头即该设备名）。

### 3.6 回收与吊销

```bash
teamx node destroy <node_id>
```

回收清单（逐项带 `done` 状态，可验证）：

| 步骤 | 行为 |
|---|---|
| stop-services-and-delete-os-user | 停服务组 + `userdel -r`（**先于 terminate**，机器还在时才能做） |
| terminate-instance | 终止实例；**同主机还有别的 team 实例时跳过**（§2-P，避免误杀） |
| delete-keypair | 删 AWS keypair |
| purge-secrets | 删 `node_secrets` + 本地邀请函 |
| revoke-member-cert | 该成员**已无存活节点**时把 seat 置 `left` → 其证书发来的 RPC 一律被拒 |

吊销语义：pki 无 CRL，采用 **seat 状态**判定 —— `serve::dispatch` 在派发前校验，`left`/`denied` 直接拒绝，未认领座位只放行 `team.import`。

### 3.7 运维

```bash
teamx node stop <node_id>     # 停止实例（保留磁盘）
teamx node start <node_id>    # 启动
teamx node auto-stop [--idle <node_id,...>] [--yes]   # 两级：实例级停服务组 / 整机级仅当全部 idle
```

---

## 4. 节点内部布局

| 项 | 约定 |
|---|---|
| OS 用户 | `--os-user`（如 `xiaods`）或默认 `<team-slug><4位数字>`；家目录 `/home/<user>`（0750） |
| workspace | `/home/<os_user>/work/team-repo`（团队 repo 克隆） |
| TEAMX_HOME | `<workspace>/.teamx`（letters/证书/worker 状态都在这里） |
| 邀请函 | `<workspace>/.teamx/letters/invite/invitation.letter`；import 后 store 到 `letters/<invitation_id>/` |
| 端口 | 由 `node_id` 稳定派生：opencode `4200–4599`、ttyd `+400`、fs `+800`（同主机多实例不撞端口） |
| systemd 单元（每实例 6 个） | `teamx-<user>-opencode-serve` / `-teamx-worker` / `-ttyd` / `-fs`；`tx-expose-<user>-ttyd` / `-fs` |
| 二进制 | `/usr/local/bin/teamx`（server 按节点架构推送；server 侧交叉产物命名 `teamx-<arch>`） |
| 隧道 | `ttyd-<os_user>-<node8>` / `fs-<os_user>-<node8>`（mode=local，团队成员用 `tunnel forward` 访问） |

---

## 5. 真机踩坑清单（FakeProvider 覆盖不到，均已在代码中修掉）

| # | 现象 | 对策 |
|---|---|---|
| 1 | AWS tag 简写空值 → `Expected '=', received '{'` | 改 JSON 形式 + 跳过空值 tag |
| 2 | RunInstances 返回时公网 IP 为 null → 只能拿到私网 IP，SSH 必超时 | 创建后轮询 describe-instances 等公网 IP |
| 3 | runner 私钥路径与落盘路径不一致 | `key_path_for()` 单一来源 |
| 4 | 安全组 Duplicate 被丢弃 → 退回 default SG（SSH 被挡） | 回查同名 SG 并挂上 |
| 5 | bootstrap 装 `/tmp/teamx` 但从未上传 | bootstrap 前推送二进制 |
| 6 | ARM 节点收到 x86_64 二进制（Exec format error） | `uname -m` 探测 + 按架构选二进制 |
| 7 | 状态机 `requested→bootstrapping` 非法 / `X→X` 非法 | 进入 SSH 阶段前归一为 `provisioning`；`set_node_state` 同态幂等 |
| 8 | scp 被远端非交互输出打断 | 改 `ssh 'cat >'` + stdin |
| 9 | 多行 CA PEM 直接内联**截断脚本** | base64 内联 + 远端解码 |
| 10 | `sudo -u <u> tee ~/…` 的 `~` 落成 `/home/ubuntu` | 在目标用户 shell 内 `cat >` |
| 11 | `team import` 缺 `--session`（被 `\|\| true` 吞掉）→ 座位从未认领 | 传 `--session net:<member_id>` |
| 12 | 认领 RPC 收到**双层 base64** | 用解一层后的 letter 原文 |
| 13 | `/home/<user>` 0750 → ubuntu 读不到证书/URL，认领与 clone **条件恒为假** | 认领整段 `sudo -u <u> env … bash -s`；读 URL 用 `sudo cat` |
| 14 | post-approve 里 `rm -rf <workspace>` 会**删掉 .teamx 里的证书** | 改为 clone 到临时目录后**并入** workspace |
| 15 | approve hook 同步跑 clone → **RPC 40s 超时** | 异步线程执行，RPC 立即返回 |
| 16 | `systemctl start 'teamx-<u>-*'` 对**未加载**单元无效 | 先 `list-unit-files` 枚举再逐个 start |
| 17 | 占位单元 `echo noop` + `Restart=always` → 无限重启 | 真实服务定义（本手册 §4） |
| 18 | `node.destroy` 误用桩函数：只 terminate，密钥/密文都没清 | 改走完整回收清单 |
| 19 | 销毁清单**谎报**"已吊销证书" | 按 seat 状态真正吊销 + 清单诚实标注 |
| 20 | console 把**所有未绑定 user 的座位**并成一个组，组头取第一个成员名（显示成 `teamx-ent`） | 无 user 的座位各自成组；并新增 `member set-user` 把设备绑到「人」 |

> 另注：`.teamx/` 若被 `TEAMX_HOME` 指错，会出现邀请函 CA 不匹配（`BadSignature`）——见 `docs/59`。

---

## 6. 真机现状（2026-09-20）

| 项 | 值 |
|---|---|
| server | `120.27.222.33`（teamx-ent / biz / poc 三服务，二进制随主仓 main 部署） |
| 存活主机 | 1 台：`i-0efbe399d08e77839` · us-west-2a · t4g.small · `16.145.86.98` |
| OS 用户 / workspace | `xiaods` · `/home/xiaods/work/team-repo`（31M，git 仓库） |
| 单元 | 6/6 active（opencode-serve / worker / ttyd / fs + 2 个 tx-expose） |
| 成员 | `xiaods-ec2`（user `xiaods`）active + online；与 `xiaods-mac` 同属一个 user |
| 隧道 | `ttyd-xiaods-ff03cb9f` / `fs-xiaods-ff03cb9f`（mode=local） |
| 验收 | 开通 job `attempts=0`（一次通过）；worker 正常轮询并处理团队讨论 |

---

## 7. 待办 / 已知边界

1. **`im-gateway` 单元未做**：CLI 目前没有节点侧 gateway 命令（设计里也是「可选」）。
2. **吊销只有 seat 级**：无 CRL/OCSP；证书在有效期内于 TLS 层仍被接受，靠派发前拦截。
3. **多 team 实例共享主机的隔离**：`nodes.os_user` 目前是单值，§3.1 的「一主机多 team 各一个 OS 用户」尚未完全落地。
4. **provision job 需要手动触发 runner**（`node run-jobs`）；未接入 serve 的定时器。
5. **AWS 凭据为 owner access key（server 本地 0600）**：裁决 A 的首选是 instance profile / KMS 加密存储，待补。
