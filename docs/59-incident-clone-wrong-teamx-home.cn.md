# 59 · 事故复盘：clone 用错 TEAMX_HOME 导致邀请函 CA 不匹配

> 现象：成员 `team import` 后连不上 server，客户端报 `certificate signature failure`，服务器返回 TLS alert 51（`decrypt_error`）。
> 根因：在 **server 机上**执行 `teamx team clone` 时，`TEAMX_HOME` 解析到了**旧的 home（旧 CA）**，而不是正在运行的 server home。
> 影响：整支 clone 团队（`yongyan-dev`）的邀请函全部不可用 + 仓库落到错误的 home。
> 更新：2026-09-19

---

## 1. 现象

- 成员导入邀请函（`teamx team import`）返回 `status: stored`（正常，网络模式只落证书）。
- 但一旦连接 server 认领席位：
  - 客户端校验服务器证书失败：`certificate signature failure`
  - 即使跳过服务器校验，服务器也拒绝客户端证书：`TLS alert 51 (decrypt_error)`
- 即 **双向 mTLS 都过不去**，无法认领席位。

## 2. 诊断过程（可复现）

1. 对比「邀请函内嵌 CA」与「server 实际 CA」的指纹：

   ```bash
   # 邀请函里的 CA
   openssl x509 -in <letter_dir>/ca.crt -noout -fingerprint -sha256
   # server 实际使用的 CA（见 systemd 单元的 TEAMX_HOME）
   sudo openssl x509 -in /root/.teamx/ca/ca.crt -noout -fingerprint -sha256
   ```

   | 来源 | SHA256 指纹 | serial |
   |---|---|---|
   | 邀请函内嵌 CA | `E2:7E:A5:10…:E9:2E` | `3BBD560C91B2DC83555B43E0328947BFE2F9FFED` |
   | server `/root/.teamx/ca/ca.crt` | `2F:31:B8:DA…:84:96` | `2E2BB189F1609B705F332B16D552966F40844952` |

2. 确认服务器证书由哪个 CA 签发：

   ```bash
   openssl s_client -connect 95.40.221.159:443 -servername 95.40.221.159 </dev/null \
     | openssl x509 -noout -subject -issuer -serial
   # subject=CN=teamx-server  issuer=CN=teamx-ca  serial=14D3560B…
   ```

3. 用「server CA」验证「服务器证书」→ OK；用「邀请函 CA」验证 → `certificate signature failure`。

4. 在 server 机上找第二份 CA：

   ```bash
   sudo find /root /home /var/lib/teamx -name ca.crt
   # /root/.teamx/ca/ca.crt        ← 运行中 server 用（2F:31…）
   # /home/ubuntu/.teamx/ca/ca.crt ← clone 实际用（E2:7E…，与邀请函一致）
   ```

5. 查 DB 确认 clone 落库正常、只是证书签错 CA：

   ```bash
   sudo python3 - <<'PY'
   import sqlite3; c=sqlite3.connect("/var/lib/teamx/teamx.db")
   for r in c.execute("select id,name,state,owner_member_id,created_at from teams"): print(r)
   for r in c.execute("select id,team_id,role_key,cert_cn from invitations where team_id='<CLONE-TEAM-ID>'"): print(r)
   PY
   ```

6. 查仓库落点（`git_repos.path` 字段）——若指向 `/home/ubuntu/.teamx/repos/...` 即坐实用错 home：

   ```bash
   sudo python3 -c "import sqlite3;[print(r) for r in sqlite3.connect('/var/lib/teamx/teamx.db').execute(\"select name,path from git_repos where team_id='<CLONE-TEAM-ID>'\")]"
   ```

## 3. 根因

`teamx` 的 CA / 仓库 / 证书目录都由 **`TEAMX_HOME`** 决定（默认 `~/.teamx`）：

- 运行中的 server：`teamx.service` → `Environment=TEAMX_HOME=/root/.teamx`，`--db /var/lib/teamx/teamx.db`，CA = `2F:31…`。
- 该机器上还残留一个**旧 home** `/home/ubuntu/.teamx`（早期 server 以 `ubuntu` 运行时的 home），CA = `E2:7E…`。

执行 clone 的人**以 `ubuntu` 身份、在 server 机上直接跑 CLI**（如 `teamx team clone … --db /var/lib/teamx/teamx.db`），但没有显式指定 `TEAMX_HOME`，于是：

- `pki::issue_member_cert()` 读取的是 `/home/ubuntu/.teamx/ca` → 所有 client cert 由 **旧 CA `E2:7E…`** 签发；
- `git_repos.path` 也写成 `/home/ubuntu/.teamx/repos/<team>/…`。

DB（`/var/lib/teamx/teamx.db`）本身是共享的，所以 team / invitation 记录都正确落库 —— **只有证书和仓库目录落在了错误的 home**，造成「DB 有席位、但证书不被 server 信任」。

> 关键点：**server 上的任何写操作（clone / invite）都必须带 `TEAMX_HOME=/root/.teamx`**，否则会静默使用 `~/.teamx`（旧 CA）。

## 4. 影响面

- 该团队**全部**邀请函的 client cert 由旧 CA 签发 → 所有成员都连不上。
- 该团队**全部** git 仓库建在错误 home（功能上 root 仍可读写，但不符合规范，且易被误清理）。

## 5. 修复

### 5.1 快速修复：按 server CA 补签证书（保留 team_id / invitation_id）

对每个受影响的 invitation，用 **server home** 重新签发一张 client cert，再重建邀请函：

```bash
# 1) 用 server home 签发（注意 TEAMX_HOME）
sudo env TEAMX_HOME=/root/.teamx teamx cert issue <MEMBER_ID> <ROLE_KEY> --out /tmp/reissue --json
#    → /tmp/reissue/member.crt、member.key，CN=member:<MEMBER_ID>:<ROLE_KEY>[:<USER_ID>]

# 2) 用 server CA + 新证书重建 letter（保持原 invitation_id / team / role / user 绑定）
#    组装 JSON：{ teamx_invitation: {…原元数据…, server.ca_fingerprint: sha256:<server CA fp>},
#                 certificates: { ca_cert: /root/.teamx/ca/ca.crt, client_cert: …, client_key: … } }
#    base64 后得到 teamx-inv:v1:<base64>
```

> 注意：`teamx cert issue` **不支持 user 绑定**（CN 只到 `member:<id>:<role>`）。若原邀请函绑定了 user（CN 为 4 段 `member:<id>:<role>:<user>`），用 CLI 补签会退化为 team-scoped；要保留 user-scoped 隧道 ACL，请改用 **5.2 或经 RPC 重发**（`team.invite` 的 `user`/`user_name` 参数会带上 user id）。

### 5.2 彻底修复：用正确的 home 重新 clone

在 server 机上（务必带 `TEAMX_HOME`），以源团队 owner/co-lead 身份：

```bash
sudo env TEAMX_HOME=/root/.teamx teamx team clone --name <clone-name> \
  --team <SRC-ID> --yes --session <owner-session> \
  --server-url https://95.40.221.159 --db /var/lib/teamx/teamx.db
```

- 若同名团队已存在（坏的那支），先 `team destroy` / 改名，或换名；
- 新 team_id 会变，旧坏团队的邀请函作废。

### 5.3 迁移仓库到正确 home

```bash
sudo mkdir -p /root/.teamx/repos/<TEAM-ID>
sudo mv /home/ubuntu/.teamx/repos/<TEAM-ID>/* /root/.teamx/repos/<TEAM-ID>/
# 同步 DB 中的 path
sudo python3 - <<'PY'
import sqlite3
c=sqlite3.connect("/var/lib/teamx/teamx.db")
c.execute("update git_repos set path=replace(path,'/home/ubuntu/.teamx/repos/','/root/.teamx/repos/') where team_id='<TEAM-ID>'")
c.commit()
PY
```

### 5.4 推荐：直接用 RPC 重发邀请函（最稳）

当 owner 已激活（哪怕只有 owner 一张好信）后，经 server RPC 重发最省事——证书、CA、user 绑定都由 server 进程按自身 home 处理：

```
team.invite { team, role_desc, name_hint, user|user_name, as_lead, auto_approve, server_url }
```

重发后建议把旧（坏 CA）邀请函 `team.invite_revoke` 掉，避免误用。

## 6. 预防

1. **server 上的写操作一律显式 `TEAMX_HOME=/root/.teamx`**：`clone` / `invite` / `cert issue` / `serve` 都是。
2. **清理残留 home**：server 上不要保留 `/home/ubuntu/.teamx`（旧 CA）这类会「劫持」默认解析的目录；如需保留，改名或移出，避免 `~/.teamx` 命中。
3. **clone 后先自检**（见 §7）：`git_repos.path` 是否在 `/root/.teamx/repos/`、邀请函 `ca_fingerprint` 是否等于 server CA 指纹。
4. **不要把 CA 指纹当成装饰**：`teamx_invitation.server.ca_fingerprint` 就是用来提前发现这类不一致的——导入前可比对。

## 7. 验证（导入前 / 导入后）

```bash
# server CA 指纹
sudo openssl x509 -in /root/.teamx/ca/ca.crt -noout -fingerprint -sha256
# 邀请函 CA 指纹（base64 解出 certificates.ca_cert 后）
openssl x509 -in <letter_dir>/ca.crt -noout -fingerprint -sha256
# 两者必须一致

# 端到端（用该 workspace 的证书直连 server RPC）
# 期望：team.list 返回该团队；team.import 返回 status=active
```

## 8. 相关文档

- `docs/34-team-clone.cn.md`（clone 功能实现）
- `docs/38-clone-runtime-design.cn.md`（clone 运行设计，§3/§8 已加本坑提示）
- `docs/39-clone-runtime-checklist.cn.md`（clone 检查清单）
- `docs/35-manual-member-cloud-node.cn.md`（member 云节点部署、TEAMX_HOME/TEAMX_SERVER_URL 约定）
