# 需求文档：teamx 企业版协作与运维能力

- 状态：draft
- 所有者：owner
- 创建者：caishu（owner）
- 创建日期：2026-09-17

## 背景

teamx 企业版（`teamx-enterprise` 仓库）在开源内核（团队 / 目标 / 事件台账 / 任务契约）之上，补齐企业级能力：
mTLS 网络模式与成员/管理 console、流量抓包/分析/回放、团队文档与任务契约、IM 接入（远程操作与管理）。

团队遵循三条铁律：**设计优先 / 必须评审 / 测试优先**——每个交付物先设计、再评审、后测试，通过后才进入执行与交付。

## 目标

- 开发和完善 teamx 企业版。
- 以「设计 → 评审 → 测试 → 交付」闭环推进每个迭代，关键能力均有文档与测试覆盖。

## 范围

### 本期覆盖

1. **网络模式与 console**
   - `teamx serve`（HTTP RPC + WebSocket push + tunnels）与 mTLS（`crates/teamx/src/pki.rs`、`serve.rs`）。
   - **user-console**（`teamx console`，成员浏览器控制台）与 **admin-console**（`teamx ui`，owner 管理看板，读 server DB 的 activity/成本/节点）。
   - Console「File」面板：工作区文件浏览 + 预览（HTML iframe / 视频 / SVG / 语法高亮，含隐私收紧）。
   - Console「Documents」面板：TEAM.md `## 文档` 契约 + `.teamx/docs` 文档实例。
2. **流量抓包 / 分析 / 回放**（`docs/ENT-04/05/06`）
   - `capture`：SQLite `flows` / `stream_chunks` / `tls_keys` 存储；数据面 tap → 有界 channel → 后台批量写，转发不阻塞。
   - `analyze`：stats / HTTP 重建 / TLS 1.3 解密。
   - `replay`：mock serve / tcp replay / HAR / curl。
3. **IM 接入（远程操作与管理）**
   - 飞书双向对话（方案 1 sidecar，已上线）。
   - 交互式卡片 + 二次确认（审批/验收载体）。
   - 网关独立进程 B：service 证书 + 方法白名单 + 委托头永不携带 `member_id`（零特权分发）。
4. **团队文档与任务契约**
   - `TEAM.md` `## 文档` 契约 → `teamx teamfile sync` 生成 `.teamx/docs/_spec/*.json` 快照。
   - 文档实例 `.teamx/docs/<key>/<NNN-slug>.md` + `.meta.json`（git 跟踪，组织过程资产）。
   - `taskx` 任务文档：内容在 git，状态机在 `.meta.json`。

### 不在本期范围

- Swift macOS 客户端（`app/`）的大改。
- tun0 透明代理的跨平台扩展。

## 用户故事

- 作为 **owner**，我希望在一个看板里看到团队目标、成员进度与成本，以便掌握迭代状态。
- 作为 **成员**，我希望在浏览器里查看团队事件/任务/隧道并浏览工作区文件，以便不依赖 CLI。
- 作为 **owner/成员**，我希望能抓取、分析并回放流量，以便定位网络问题并做回归验证。
- 作为 **owner**，我希望通过 IM 远程审批/验收，以便不在电脑前也能推进流程。
- 作为 **团队**，我们希望需求/设计/测试方案/评审等过程资产沉淀在 `.teamx/docs`，以便可追溯、可复用。

## 验收标准

1. 关键能力均有设计文档（`docs/ENT-*`、`docs/0x-design-*`）与测试覆盖（`cargo test -p teamx` + `tests/*.sh`）。
2. `bash tests/run-all.sh` 全绿（部分步骤需 bun / root / 网络，按脚本 gate 处理）。
3. `.teamx/docs/` 下的文档实例可被 user-console「Documents」面板列出并查看正文。
4. 破坏性动作（如 goal close）有显式确认门槛（Issue #78 / PR #85）。

## 变更记录

| 日期 | 变更 | 作者 |
|------|------|------|
| 2026-09-17 | 初稿（draft）：按 TEAM.md `## 文档` 契约创建 | caishu |
