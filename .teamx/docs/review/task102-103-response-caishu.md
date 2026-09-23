# 评审回复：task-102 / task-103 处置记录（作者 caishu）

- 处置人：caishu（作者方）· 2026-09-23 · 对应实现提交 `f70c105`
- 对象：`102-review-teamx-cloud-backend-weiye.md`、`task103-platform-console-review-weiye-mini.md`

## 结论摘要

| 意见 | 判定 | 处置 |
|---|---|---|
| 102-必改① 换发路径未落 O-2 台账 | ❌ **误报（附证据）** | 无需改动；台账在 `f02ce86` 已接 |
| 102-必改② 租户模式静默回落实例 CA | ✅ 成立 | 改 fail-closed 拒签 |
| 102-排期项 M0/G2/A 尽快合并 | ⏸ **转 owner** | 属 PR 合并决策，非实现方可单方执行 |
| 102-建议 1 `issued revoke` 语义边界 | ✅ 采纳（文档） | 已在 design/014 §4 记为"记账性吊销"；认证期查 revoked 列后续项 |
| 102-建议 2 私钥存储措辞 | ✅ 已在 §4 落实 | 三项可验证约束（无"加密"承诺） |
| 103-M1 存储型 XSS | ✅ 成立 | 渲染层统一 `esc()`；needle 断言覆盖 |
| 103-M2 用户面 `server_image` | ✅ 成立 | 用户路径剥离 + 断言忽略 |
| 103-M3 bootstrap `server_url` | ✅ 成立 | `validate_tenant_endpoint` 锚定 + 单测/E2E |
| 103-建议 1 cookie `Secure` | ✅ 采纳 | 4 处加 Secure；OAuth Lax 注明理由 |
| 103-建议 3 时序侧信道 | ✅ 采纳 | `DUMMY_HASH` 对齐耗时 |
| 103-建议 2 登录限速 / 建议 4 常量时间比较 | ⏸ 记为后续 | 未做，见"未处置"节 |

## 🔴 102-必改①：误报，附证据

评审依据是「`98f8059` grep `issued::record` 命中 0」。该提交**早于 O-2 台账本身**
（O-2 在 `f02ce86` 才引入），故必然为 0 —— 应查 HEAD 而非该中间提交。三条证据：

1. **代码**：HEAD `crates/teamx/src/provision.rs:728`，`reissue_member_certs` 循环内调用
   `issued::record(conn, &rot.serial_hex, &rot.cn, "member", &role, member_id, team_id, &ca_fp_here, CERT_DAYS, "platform")`。
2. **引入点**：该行由 O-2 提交 `f02ce86` 引入（`git log -S"issued::record" -- provision.rs` 可见；
   后续 `aa981fc` 触及同文件同函数）。
3. **实证**：`tests/tenant-e2e-test.sh` N11 断言「存在 `ca_fingerprint == 租户CA` 且 `kind == member` 的行」——
   该组合**只能**来自换发路径（N4 的 invite 走平台 CA，N8 的 reissue 走租户 CA）。
   该断言在 stub E2E 与台式机真集群均通过。若换发未记账，断言必然失败。

结论：轮换前置校验**不会**因换发漏记而假通过。建议 reviewer 复核当前 HEAD 后撤下该必改。

## 已实现的处置（`f70c105`）

- **102-必改② fail-closed**：租户模式（`TEAMX_TENANT_TEAM_ID` 非空）且 CA 材料缺失时直接报错拒签
  （`commands.rs` mint 路径），不再回落实例 CA；错误文案指明检查 CA Secret 挂载。
- **103-M1 XSS**：新增 `esc()`，对 `team_name` / `namespace` / `team_id` / `ca_fingerprint` /
  `server_image` / tokens / admins / audit 全渲染路径转义；单测 needle 断言覆盖。
- **103-M2**：`create_my_tenant` 去掉 `server_image` 参数（一律平台 pin）；用户路由不再读取该字段；
  E2E **D7** 断言"用户传 server_image 但 apply 产物中无该镜像"。
- **103-M3**：新增 `validate_tenant_endpoint`（仅 https + host 必须是本租户
  `teamx-serve-<short>.<ns>.svc` 或 loopback），单测 7 例（含跨租户/外部/非 https 负例）；
  E2E **D8** 断言外部端点被拒、非 https 400。
- **103-建议 1**：4 处 SET-COOKIE 加 `Secure`；OAuth 回调 `SameSite=Lax` 注明"跨站 302 必用 + state 防 CSRF"。
- **103-建议 3**：`authenticate` 用户不存在时跑 `DUMMY_HASH` 对齐耗时。

验证：单测 **406 passed**；clippy clean；platform-console E2E **25 断言**（新增 D7/D8）；
tenant/k8e/platform/node 套件全绿。

## 未处置（诚实记录）

- **103-建议 2 登录失败限速/锁定**：未做（600k PBKDF2 天然节流，且失败不写库防刷盘；
  限速需计数器与策略，单列后续项）。
- **103-建议 4 launch token 常量时间比较**：未做（16 字符本地 token，远程时序攻击不现实）。
- **102-建议 1 认证期查 revoked serial**：未做（当前 revoke 为记账性；密码学拒绝靠废 CA）。
- **102-排期项（M0/G2/A 合并）**：转 owner 决策；实现方不单方合并 PR。

—— caishu（作者方处置）
