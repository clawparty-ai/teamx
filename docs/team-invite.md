# team-invite 设计方案 v2（mTLS + Invitation Letter）

> 状态：**I0/I1/I2 已实现**（PKI + 强制 mTLS serve + `team invite`/`team import` + 证书身份接入 RPC + 吊销强制/主动断连），I3（插件 mTLS transport 集成，已基本完成）进行中
> 关联：`docs/network-mode.md`（网络模式）、`docs/v1-spec.md`
> 核心升级：把"可猜 token / 免审批邀请码"升级为 **mTLS 证书授权**；邀请码升级为 **Invitation Letter**（证书 + 地址 + 角色的一体化数据包）。

---

## 1. 目标

1. **mTLS 双向认证**：teamx serve **强制 mTLS**——server 验证成员证书，成员验证 server 证书，双向信任（无明文降级）。
2. **Invitation Letter**：owner 生成一个自包含"邀请函"数据包，包含：
   - owner 签发的**客户端证书**（用于 mTLS 连接）
   - **server 地址**（`wss://<owner-ip>:5781`）
   - 被邀请成员的 **job role + job description**
3. **一键入职**：成员导入 letter → 建立 mTLS 连接 → 自动获得角色 → 进入工作模式。
4. **仍需 owner approve**：证书不是免审批——即使持有有效证书，成员仍需 owner 审批后才能正式工作（**防证书意外泄露**，证书是"能连"，approve 是"能干活"）。
5. **证书有效期**：默认 **3650 天**（10 年）。

---

## 2. 用户流程

```
┌──────────── owner ────────────┐      ┌────────── member ──────────┐
│ 1. /team-serve start          │      │ 4. /team-import <letter>   │
│    （生成 CA + server 证书）    │      │    （导入 invitation letter）│
│        │                       │      │    │                       │
│        ▼                       │      │    ▼                       │
│ 2. /team-invite "测试工程师:    │      │ 5. 插件用 letter 里的客户端  │
│    负责测试，汇报结果与缺陷"      │      │    证书建立 mTLS 连接        │
│        │                       │      │    │                       │
│        ▼                       │      │    ▼                       │
│ 3. 生成 invitation letter      │      │ 6. 提交 join（pending）     │
│    （证书+地址+角色）→ 发给成员   │      │    + 自动携带角色申请        │
│        │                       │      │    │                       │
│        ▼                       │      │    ▼                       │
│ 7. owner approve → 成员 active  │◄─────│ 7. 进入工作模式             │
│    （防证书泄露，approve 必需）  │      │    + 等待任务并自动执行       │
└────────────────────────────────┘      └────────────────────────────┘
```

---

## 3. mTLS / PKI 架构

### 3.1 信任模型

```
                    ┌────────────────────────────┐
                    │  teamx CA（owner 私有）       │
                    │  自签根证书 ca.crt / ca.key  │
                    └──────┬─────────────┬───────┘
                           │ 签发         │ 签发
                    ┌──────▼─────┐  ┌──────▼──────┐
                    │ server.crt  │  │ member.crt  │  ← invitation letter 携带
                    │ server.key  │  │ member.key  │     （每个成员单独签发）
                    │ (serve 持有) │  └─────────────┘
                    └─────────────┘
```

- **CA**：`~/.teamx/ca/`（owner 私有），`teamx serve start --mtls` 时自动生成（不存在则创建）。
- **server 证书**：`ca` 签发给 server（CN=teamx-server, SAN=IP/DNS）。
- **成员证书**：每次 `team-invite` 签发一张，绑定 `member_id + role_key`（CN 或 SAN 中带角色）。

### 3.2 认证流程

```
member plugin ──TLS client hello──► teamx serve
    │ 出示 member.crt + member.key      │ 验证：member.crt 由 teamx CA 签发？
    │                                  │ 解析 CN/SAN → member_id + role
    ◄── server 出示 server.crt ────────│ 验证：server.crt 由 teamx CA 签发？
    │（成员侧校验 CA 指纹）              │ （mTLS 双向）
    │                                  │
    └──── mTLS 建立，身份 = 证书身份 ────┘
                │
                ▼
    成员 join 需 owner approve（防证书泄露）
    证书 = "能连上"；approve = "能干活"
```

- 不再需要 `Authorization: Bearer <token>`——**TLS 层已完成身份认证**。
- 身份来源：证书 CN/SAN 解析出 `member_id` 与 `role_key`（server 侧从 `invitations` 表反查）。
- **approve 仍是必经步骤**：证书允许建立连接并提交 join 申请；owner approve 后才进入 `active` 工作模式（防止证书被意外泄露时直接可用）。

---

## 4. Invitation Letter 数据包

### 4.1 格式（自包含 JSON / PEM bundle）

```jsonc
{
  "teamx_invitation": {
    "version": 1,
    "invitation_id": "uuid",
    "team": { "id": "…", "name": "验收测试组" },
    "server": { "url": "wss://192.168.1.5:5781", "ca_fingerprint": "sha256:abcd…" },
    "member": { "name_hint": "" },
    "role": { "key": "tester", "label": "测试工程师",
              "description": "负责编写执行测试用例、汇报结果与缺陷。" },
    "issued_at": "…", "expires_at": null
  },
  "certificates": {
    "ca_cert":     "-----BEGIN CERTIFICATE-----…",  // 用于验证 server
    "client_cert": "-----BEGIN CERTIFICATE-----…",  // 成员身份
    "client_key":  "-----BEGIN PRIVATE KEY-----…"   // 成员私钥（仅 letter 内，不入库）
  }
}
```

> **私钥安全**：client_key 只在 letter 里，server/CA 侧不留存成员私钥。letter 传输渠道由 owner 自选（安全信道 / 线下拷贝 / 加密传输）。

### 4.2 便捷分发

- CLI 输出为**单行 base64**（`teamx-inv:v1:…`），便于拷贝/粘贴；也可 `--file letter.json` 落盘。
- 成员侧 `teamx team import <letter>`（或 base64）→ 插件解包、存 `~/.teamx/letters/<invitation_id>.json`（0600）、建立连接。

---

## 5. 新命令

| 命令 | 工具 | 说明 | 权限 |
|---|---|---|---|
| `teamx serve start --mtls` | teamx_serve_start | 生成 CA+server 证书，以 mTLS 起服务 | owner |
| `teamx team invite "<role>: <desc>"` | teamx_team_invite | 签发成员证书 + 生成 invitation letter | owner |
| `teamx team invite list` | teamx_team_invite | 列出已签发未使用邀请函 | owner |
| `teamx team invite revoke <id>` | teamx_team_invite | 吊销邀请函（更新吊销列表） | owner |
| `teamx team import <letter>` | teamx_team_import | 导入邀请函，建立 mTLS 连接 | 成员 |
| `teamx team join`（免 token） | teamx_join（扩展） | 导入后由插件自动 join | 成员 |

### 5.1 证书吊销

- 新增 `cert_revocations` 表（或复用 `invitations.revoked_at`）：server 在 TLS 握手时**检查吊销列表**，revoke 后立即拒绝。
- 简单方案：`invitations` 表加 `revoked_at`；server 加载已签发邀请的 CN→状态映射，握手校验。

---

## 6. DB v6 迁移

```sql
-- 邀请函（含证书映射）
CREATE TABLE IF NOT EXISTS invitations (
  id            TEXT PRIMARY KEY,
  team_id       TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
  role_key      TEXT NOT NULL,
  role_label    TEXT,
  role_desc     TEXT,
  cert_serial   TEXT,                 -- 成员证书序列号（吊销用）
  cert_cn       TEXT,                 -- 成员证书 CN（member_id）
  created_by    TEXT NOT NULL,
  created_at    TEXT NOT NULL,
  used_by       TEXT,
  used_at       TEXT,
  revoked_at    TEXT
);
```

---

## 7. 插件端（client.ts）改造

```ts
// 连接配置来自 invitation letter（或 serve start 的返回）
type MtlsConfig = {
  serverUrl: string          // wss://<owner-ip>:5781
  caCert: string             // PEM
  clientCert: string         // PEM
  clientKey: string          // PEM
}
// Bun.fetch 支持 tls: { cert, key, ca } → 建立 mTLS HTTP/WS
```

- `TEAMX_SERVER_URL` 仍作默认指向；有 letter 时用 letter 的 serverUrl + mTLS。
- `runRpc` / WS 连接使用 mTLS 客户端证书；身份从证书解析，不再传 session/token。

---

## 8. 安全分析

| 威胁 | mTLS + letter 缓解 |
|---|---|
| 伪造成员 | 需持有 CA 签发的有效成员证书 |
| 重放/复用邀请 | letter 一次性（used_by）+ 证书可吊销 |
| 中间人 | server 证书由 CA 签发，成员校验 CA 指纹 |
| token 泄露 | 无 token；私钥仅存成员侧 |
| 私钥泄露 | 可 revoke 该邀请函证书 |

**边界**：
- V1 仍无真实鉴权（本地 CLI 模式不变）；mTLS 仅用于网络模式连接层。
- letter 传输渠道的安全由 owner 负责（设计文档标注）。

---

## 9. 里程碑

| 阶段 | 内容 | 验收 |
|---|---|---|
| I0 | Rust PKI：CA 生成、server/member 证书签发、**强制 mTLS** serve（rustls）；证书默认 3650d | ✅ 已完成 |
| I1 | `team invite` 签发 letter；`team import` 导入并 mTLS 连接；RPC 从证书 CN 解析身份 | ✅ 已完成（见 `tests/mtls-test.sh`） |
| I2 | **approve 流程**：join pending → owner approve → active；吊销（revoke）检查、一次性使用、角色自动授予 | ✅ 已完成（吊销检查 + 主动断连，见 `tests/ws-test.ts`） |
| I3 | 插件集成：mTLS transport + auto-execute 对接 | 🔄 部分完成（runRpc/runWs mTLS + 工具已实现） |
| I4 | 跨网络验证（局域网/公网） | ⬜ 待做 |

---

## 10. 风险与待决

| # | 问题 | 决策 |
|---|---|---|
| Q1 | 证书有效期 | **默认 3650 天（10 年）**；过期前插件提示续签 |
| Q2 | CA 私钥保护 | `~/.teamx/ca/` 0600；可选支持 HSM/钥匙串（后续） |
| Q3 | letter 传输渠道 | 线下/加密传输；CLI 仅输出 base64 不落盘私钥 |
| Q4 | 是否仍需 approve | **需要**——证书是"能连"，approve 是"能干活"；防证书意外泄露 |
| Q5 | 是否强制 mTLS | **强制**——网络模式一律 mTLS，无明文降级开关 |
| R1 | Bun.fetch TLS 配置支持 | **✅ 已验证**：`fetch(url, { tls: { cert, key, ca, serverName } })` 完整支持 mTLS 双向认证（node https server 强制 requestCert + rejectUnauthorized 验证通过；无证书客户端被拒） |
| R2 | rustls 依赖较重 | 接受；mTLS 是强制要求 |

---

## 11. ADR 摘要

1. **证书即身份**：mTLS 连接建立 = 身份认证完成，成员身份/角色从证书解析。
2. **Letter 即入职包**：证书+地址+角色一体化，成员导入即入职。
3. **证书 ≠ 授权**：证书允许连接 + 提交申请，仍需 owner approve 才进入工作模式（防泄露）。
4. **强制 mTLS**：网络模式无明文降级；证书默认 10 年。
5. **与 V1 并存**：本地 CLI 无鉴权不变；mTLS 仅网络模式连接层。
3. **吊销优先**：一次性 + 可吊销，安全边界清晰。
4. **与 V1 并存**：本地 CLI 无鉴权不变；mTLS 仅网络模式连接层。
