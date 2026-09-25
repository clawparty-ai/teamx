---
status: active
protocol_version: 1
owner: caishu
created_at: 2026-09-25
updated_at: 2026-09-25
---

# 020 — yongyan-dev / powernano 团队迁移到 teamx cloud（运行手册）

> 输入：`~/Desktop/nano激光/deliver/yongyan-dev/docs/infra-inventory.md`（PowerNano 基础设施清单）
> 目标平台：**teamx cloud 生产环境** —— https://teamx.beer （AWS us-east-2 · `18.225.44.71` · k8e + Cilium）
> 源端：**AWS 香港 `95.40.221.159`**（`ip-172-31-18-83`，经 `nc-tokyo` 跳板）· `teamx.service`（mTLS :443）

## 0. 硬约束（owner 明确要求）

- **源主机（HK `95.40.221.159`）在迁移后不得销毁**；任何销毁/停机动作**必须得到 owner 明确同意**后才可执行。
- 本手册**只做加法**：云端搭建 + 数据复制 + 逐成员切换；**不改、不删源端任何东西**。
- 迁移期间源端保持**在线可用**（= 天然回滚目标）。

## 1. 源端清单（2026-09-25 实测）

| 项 | 值 |
|---|---|
| 控制面 | `teamx serve --addr 0.0.0.0 --port 443 --san 95.40.221.159 --db /var/lib/teamx/teamx.db`（User=root, `TEAMX_HOME=/root/.teamx`）|
| 团队数据 | `/var/lib/teamx/teamx.db` = **103 MB**（WAL 4.6 MB）· **`user_version = 20`**（与本仓新代码**同版本**，无需升级 schema）|
| 团队 | **active：`yongyan-dev`**（f2adafd3…）、**`powernano-v2`**（1da9a7f0…）；completed：powernano、starnet；destroyed：4 个历史团队（含 smoke/verify）|
| **共享 CA** | `/root/.teamx/ca/{ca.crt,ca.key,server.crt,server.key}` —— **一套 CA 签了全部成员证书** |
| 成员证书/信 | `/root/.teamx/letters/`（签发方记录） |
| **git 仓库** | `/root/.teamx/repos/<team_id>/` —— 5 个（按 team id 命名，团队 id 必须保持不变）|
| 看板 | `teamx-ui.service` → `teamx ui --addr 127.0.0.1 --port 9527 --db /var/lib/teamx/teamx.db --server-url https://95.40.221.159 --session yongyan-pnv2-owner` |
| 隧道 | `tx-expose-teamx-ui.service`（`teamx tunnel expose teamx-ui …`）+ inventory 里的 `powernano-console`(8091)、`powernano-api`(8080) |
| 守护 | `teamx-watchdog.timer`（TLS 探针 60s，连续 2 次失败重启 `teamx`）|
| 成员客户端 | 3 台云主机（zhongm `35.77.5.99` / caicai `184.34.85.90` / zhaoz `3.37.16.165`，均 `t4g.small`）+ owner 本机 Mac |

## 2. 目标端映射（建议）

| 源 | 云端 | 说明 |
|---|---|---|
| 整台 server（1 个 DB / 1 套 CA / 5 个 repo） | **1 个云端租户**（1 namespace + 1 tenant server Pod + PVC） | **不拆分**：拆分需切 DB、且一套 CA 拆成多套会让**所有成员信失效**；单租户完整保留历史与团队 id |
| 源 CA | **原样搬入租户**（Secret + `<home>/ca/`） | **成员现有证书/邀请函继续有效**（同一 CA），无需重新建立信任 |
| 源 server 证书 | **用源 CA 重新签发**，SAN 增加新端点域名 | 端点从 IP 变成域名，客户端校验需要 SAN 匹配 |
| `teamx.db`（103 MB） | 租户 PVC `/data/teamx.db` | 复制前在源端做**一致性导出**（`.backup` 或 WAL checkpoint）|
| `repos/<team_id>/` | 租户 `<home>/repos/` | team id 不变 → 路径天然对上 |
| 看板 / 隧道 | 云端 VM 重挂（或先留 HK 指向新 server） | 非阻塞项，可后置 |

## 3. 关键决策（**待 owner 裁决**）

- **D1 端点方案**：建议 **`t-<short>.teamx.beer` + nginx `stream` SNI 路由（TLS 直通）**
  - 443/80 由 nginx 接管：SNI=`teamx.beer` → `teamx site`（现有 LE 证书）；SNI=`t-*.teamx.beer` → 该租户 NodePort
  - **TLS 直通**（不做 TLS 终止）→ 租户自己用 mTLS 终止，**成员证书校验链路完全不变**
  - 顺带**关闭开放项 O-5**（正式租户端点）
  - 备选：**NodePort 直连**（`https://teamx.beer:30443`），更快但地址丑、端口管理麻烦
- **D2 是否只迁 active 团队**：建议**整库迁移**（含 completed 历史），保持 id 与 repo 完整；destroyed 的团队数据也一并带过去（只读历史）
- **D3 切换节奏**：建议**逐成员切换**（每次 1 人，验证后再下一个），不做"一次性全切"
- **D4 看板/隧道**：建议**后置**（迁移验收通过后再接）；迁移期间 owner 可继续用 HK 看板
- **D5 源端保留期**：建议**至少保留 30 天**（观察期），销毁另行审批

## 4. 分阶段计划

### P0 准备（只读，不影响源端）
1. ✅ 已核对：源库 `user_version=20` == 新代码版本 → 无需 schema 升级。
2. 源端做**一致性快照**：`python3 sqlite3 .backup`（或 `PRAGMA wal_checkpoint(TRUNCATE)` 后复制），产物存源端 `/tmp/migrate/`（只读操作，不动原库）。
3. 云端准备：nginx 安装 + `stream`/`ssl_preread` 配置骨架；DNS 计划（`t-<short>.teamx.beer` A → `18.225.44.71`）。
4. 记录源端 `teamx version`（0.2.5 / commit unknown）与**当前团队/成员/事件计数**（作为迁移后对账基线）。

### P1 云端租户搭建（不影响源端）
5. 在云端平台建一个团队与租户（新建 id，仅作为**承载容器**）：
   `teamx tenant create --team <new-team-id>` → 得到 namespace + PVC + 租户 server Pod。
6. **把源 CA 写入该租户**（替换平台生成的 CA）：
   - 更新 k8s Secret `teamx-ca-<short>`（`ca.crt`/`ca.key` = 源 CA）
   - 写入租户 `<home>/ca/`
7. **用源 CA 签发新 server 证书**（SAN 含 `t-<short>.teamx.beer` 与 `teamx.beer`），写入租户 `<home>/ca/server.{crt,key}`。
8. 确认租户 Pod 用**源 CA** 起 TLS（此时源成员证书即可完成 mTLS 握手 —— 这是"CA 已继承"的判定点）。

### P2 数据迁移（复制，不动源）
9. 复制 DB（一致性快照产物，~103 MB）→ 租户 PVC `/data/teamx.db`（先停租户 Pod，避免边写边覆盖）。
10. 复制 `repos/*` → 租户 `<home>/repos/`；复制 `letters/` → 租户 `<home>/letters/`。
11. 起租户 Pod；**只读校验**：
    - 用**源端某成员的现成证书**做 `curl --cert/--key` 打 `/api/me`（或 teamx 客户端 `teamx status`）→ 必须成功（证明 CA + 数据都对）
    - 团队数/成员数/事件数与 P0 基线**一致**
    - `teamx discovery directory`、repo 列表、`tcoin`/`credit` 数据可读

### P3 端点与路由（新增，不影响源）
12. DNS：加 `A` 记录 `t-<short>.teamx.beer` → `18.225.44.71`。
13. nginx `stream`：`ssl_preread on` + `map $ssl_preread_server_name` → 租户 NodePort；`teamx site` 迁到 `127.0.0.1:9443`（443 让给 nginx）。
14. 平台侧记录该租户端点：`teamx tenant set-endpoint <team> https://t-<short>.teamx.beer:443`。
15. 验证：`openssl s_client -servername t-<short>.teamx.beer -connect teamx.beer:443` → 应看到**源 CA 签发的服务器证书**；mTLS 握手成功。

### P4 逐成员切换（每人一步，可独立回滚）
16. 对每个成员：由**迁移后的租户 server** 重新签发一张邀请函（**同一 CA、新 URL**）→ 成员 `teamx team import` → 验证 `teamx status` 指向新端点、能收发事件。
17. 顺序建议：**先 zhaoz（waiting）→ zhongm → caicai → 最后 owner**（owner 最后切，保留随时回退与全局视野）。
18. 每切一人，做一次**双向可达性**检查（该成员 → 新端点；新端点 → 该成员节点任务）。

### P5 观察期（源端保留）
19. 保持**源端与云端双跑**（源端继续 443 对外；已切成员走新端点，未切成员仍走旧端点）。
20. 观察项：事件写入连续、activity 上报、tunnels、看板数据、成本统计。
21. 观察期建议 **≥7 天**（D5 的 30 天为销毁前置）。

### P6 收尾（**仅在 owner 书面同意后**）
22. 未切成员全部切换完毕 + 观察期无异常 → 提交**销毁审批**给 owner。
23. owner 同意后：停源端 `teamx.service`（先停服不删机）→ 再观察 → 才谈主机下线。
24. **未经同意：源端一律保持运行。**

## 5. 回滚方案

| 阶段 | 回滚动作 |
|---|---|
| P1–P3 | 直接放弃云端租户（`teamx tenant destroy`，**仅云端对象**），源端零影响 |
| P4（成员逐个切） | 该成员重新导入**源端签发的原邀请函**（原 URL）→ 回到源端 |
| P5 | 停止切换、源端继续服务；已切成员按上一条回退 |
| P6 之后 | 源端仍在（未销毁）→ 可把端点 DNS 指回源端（若已改）|

**回滚的硬前提**：源端 `teamx.service` 与 `teamx.db` 始终保持在线与可用（本手册不触碰）。

## 6. 验收清单

- [ ] 云端租户用**源 CA** 提供 mTLS；源成员**原证书**可握手成功
- [ ] 团队数 / 成员数 / 事件数 / activity 数与基线一致（逐项对账）
- [ ] `yongyan-dev`、`powernano-v2` 两个 active 团队在云端可正常读写（事件、任务、讨论）
- [ ] git 仓库可 clone/push（mTLS + 权限）
- [ ] `t-<short>.teamx.beer` 证书链为**源 CA 签发**、SNI 路由正确
- [ ] 已切成员：`teamx status` 正常、能收发事件；未切成员：仍走源端无感
- [ ] 回滚演练：任选一个已切成员回退到源端并再切回，全程源端无中断

## 7. 风险与对策

| 风险 | 对策 |
|---|---|
| 复制 DB 时源端仍在写 → 数据不一致 | 源端做**一致性快照**（`.backup` / WAL checkpoint）后再传；只在快照上操作 |
| 新旧 server 同时写同一团队（双写冲突） | 云端租户在 P2 后**只做只读校验**；P4 起**每团队同一时刻只有一个活跃端点**（成员逐个切）|
| 端点变更导致成员证书 SAN 不匹配 | 用**源 CA 重签**带新 SAN 的服务器证书；客户端证书不受影响 |
| 云端 443 被 `teamx site` 占用 | P3 用 nginx 接管 443/80，`teamx site` 退到 `127.0.0.1:9443`（TLS 直通保留 mTLS）|
| k8s/NodePort 暴露面 | NodePort 仅经 nginx SNI 路由对外；安全组后续可收紧到仅 443/80 |
| 看板/隧道依赖旧 server | 后置（D4）；迁移期沿用 HK 看板 |
| 误删源端 | **硬约束**：销毁需 owner 书面同意；手册全程只做加法 |

## 8. 待 owner 决策（阻塞项）

- **D1** 端点方案：`t-<short>.teamx.beer` + nginx SNI（建议）／NodePort 直连？
- **D2** 迁移范围：**整库**（建议，含历史团队）／仅 2 个 active 团队？
- **D3** 切换节奏：逐成员（建议）／一次性全切？
- **D4** 看板与隧道：后置（建议）／随迁？
- **D5** 源端保留：≥30 天后另行审批销毁（建议）？

## 9. 不做的事（明确边界）

- 不修改、不停用、不删除源端 HK 主机及其 systemd 单元/数据
- 不迁移 owner 本机 Mac 上的运行时 server（`127.0.0.1:5781`，团队 `yongyan-backend`）—— 如需一并迁移，另开子任务
- 不动企业版 server（`120.27.222.33:5783`）
- 不迁移 3 台成员云主机本身（它们只是客户端；切换 = 改端点）
