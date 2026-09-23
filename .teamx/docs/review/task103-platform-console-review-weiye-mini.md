# task-103 评审 — Teamx Cloud 平台控制台代码（用户/鉴权/my-teams/OAuth/UI）

> 评审人：weiye-mini · 2026-09-23 · 对象：origin/main @ 6e7c7de（cccec3c/d180618/6d6c165/b55910b/a8be609/de36e16）
> 对象文件：`crates/teamx/src/platform.rs`（789 行）· `platform_console.rs`（1006 行）· `cli.rs`（PlatformUserCmd/IssuedCmd/CaAudit）· `tests/platform-console-test.sh`（321 行）
> 对照：design/014 §5/§6.5 · O-2 台账 · 视角=鉴权完整性（守卫双轨/归属门/会话/CSRF/OAuth/秘钥处理/UI-API 一致性）
> 评审方式：两文件全文通读 + 关键路由逐一走查 + provision.rs 交叉核验（create_my_tenant/bootstrap_my_team/tenant_service_post）+ UI JS 渲染路径核对 + 测试断言面清点

## Verdict：**CHANGES-REQUESTED**（3 必改，均为安全实质；核心鉴权模型无旁路，可修后合并）

---

## 1. PASS 面（守卫双轨与归属门核对，逐条）

| 检查项 | 结论 | 证据 |
|---|---|---|
| 守卫双轨 fail-closed | ✅ | `guard`（platform_console.rs:127-141）：launch token（运维）or 会话（用户），无 cookie→401；`verify_session` JOIN users 含过期/禁用双拒 |
| 归属门无旁路 | ✅ | 全部租户写路由（destroy :329/invite :345/provision :361/reissue :383/bootstrap :478）均 **guard → own_guard** 串联；越权统一 403 且「先归属后参数」（越权者无论参数对错先吃 403） |
| 用户/运维路由隔离 | ✅ | 用户会话 POST /api/tenants 被 403 指向 my/teams（:311）；my/* 路由拒绝 operator（:408/440/470「user session required」） |
| 会话机制 | ✅ | token 只存 SHA-256（platform.rs:454-462）、7 天 TTL + prune、改密/禁用即 DELETE 全部会话（:365-369/:381-384）、禁用用户 JOIN 拒绝（:425-430） |
| 密码策略 | ✅ | PBKDF2-HMAC-SHA256 **600k 迭代**（OWASP 2023）+ 版本化前缀 + ≥8 字符 + verify 拒绝 <100k 迭代（防降级）（:228-282） |
| 防枚举口径 | ✅（含 1 条时序建议） | authenticate 失败统一「invalid credentials」（:335-337） |
| OAuth | ✅（CSRF state 实现正确） | state 单次有效+10min 过期+remove-on-consume（:654-666）；client_secret 只进 token 交换请求体、spawn_blocking、任何失败不写半截会话；github_id 锚定不并户（-gh 后缀）、GitHub 用户空密码→密码登录必拒（platform.rs 测试断言） |
| O-2 台账 | ✅ | 每次服务车道调用重签短 TTL 平台服务证书并落 issued 台账；失败不阻断仅日志（口径：审计非门禁，可接受，建议告警） |
| UI-API 一致性 | ◐ | 功能面一致（my-teams 三卡/Tab/角色可见），但渲染层有必改 1（见下） |
| 测试覆盖 | ◐ | C9/D1-D6/G1-G5 覆盖登录/会话/配额/越权（alice 看不到运维租户、超配额拒、不可达明确失败）/OAuth mock 全流程；缺必改 1-3 的对应负例 |

## 2. 必改（3，带判据）

### M1 · 存储型 XSS → operator 会话被同源接管
- **判据**：`create_my_tenant`（provision.rs:865-867）对 team 名称只限长度 1..=64，**无字符清洗**；该名称经 `/api/my/teams` 原样返回，UI 以 **innerHTML 直插**：`platform_console.rs:929` `${t.team_name}`、`:932`（operator 视图 loadTenants 同模板）、`${t.server_image}` 同插。
- **攻击链**：租户用户建 team 名为 `<img src=x onerror=fetch('/api/tenants',{method:'POST',...})>` → operator 打开租户列表 → 脚本以 **launch-token 身份**同源执行（cookie HttpOnly 不可读但可带 cookie 发任意 fetch）→ 建/删租户、邀请、读 audit/tokens。
- **修法**：渲染层统一 escapeHtml（或 textContent）；server_image 一并转义（与 M2 叠加）。负例测试：建 `<img src=x onerror=…>` 名团队，断言列表响应中无未转义尖括号。

### M2 · 用户自助路径接受 `server_image` → 平台代拉任意镜像
- **判据**：`api_my_teams_create`（platform_console.rs:447）从 body 取 `server_image` → `create_my_tenant`（provision.rs:856-886「租户 infra 复用 operator 路径」）→ 平台以**自身 kubeconfig** 部署租户 server Deployment，镜像字段用户完全可控。
- **影响**：任意租户用户可让平台集群拉起任意容器镜像（用户工作负载逃出「平台 pin 的镜像」设计；design/014 §5 镜像应平台 digest pin，O-3 亦按平台受控）。
- **修法**：用户面剥离该字段（结构体不透传或显式置 None），仅运维路由 /api/tenants 接受且建议 digest pin；负例：用户 body 带 server_image 时断言被忽略/拒绝。

### M3 · bootstrap `server_url` 无校验 → 平台向任意端点递交租户级服务证书
- **判据**：`api_my_teams_bootstrap`（:477-499）仅要求非空；`bootstrap_my_team`（provision.rs:906-935）→ `tenant_service_post`（:763-788）：**用租户 CA 签发 platform-console 服务证书（含私钥）并向 server_url 发起 mTLS POST**。用户可填任意 https 收集端。
- **影响**：令平台把「该租户 CA 签发的 platform-console 服务身份证书+私钥」递到用户指定端点——租户内特权车道身份（invite/provision 白名单方法可被冒用）泄漏到租户 CA 信任域外的攻击者主机。
- **修法**：校验 server_url 为 `https://` 且 host 与租户 endpoint 锚定（O-5：等于平台注册的租户 Service DNS / tenant 行的 server_url 字段，或至少匹配 `teamx-serve-*.<ns>.svc` 形态并拒绝公网地址）；负例：host 不匹配时 400。

## 3. 建议（4，非阻塞）

1. **会话 cookie 缺 `Secure`**：3 处 SET-COOKIE（api_login/api_login_password/api_oauth_callback）均无 `Secure` 属性。当前仅 HTTPS 监听故风险有限，但属 1 行加固（OAuth 回调为 SameSite=Lax 与其余 Strict 不一致——302 回调场景 Lax 通行，建议统一注明理由）。
2. **密码登录无失败限速/锁定**：600k PBKDF2 天然节流（~0.2-0.5s/次）但无失败计数/临时封禁；建议失败 N 次临时锁 + audit 记录（登录失败目前不写审计）。
3. **用户名枚举时序侧信道**：authenticate 对不存在用户早退、不做 PBKDF2 → 响应耗时差可枚举 display_name；修法=不存在时 dummy verify 对齐耗时。
4. **launch token 非常量时间比较**（cookie_token/guard/api_login 三处 `==`）：16 字符 token 远程时序攻击不现实，属加固项（换 constant-time 比较顺手）。

## 4. 交叉核验记录

- platform.rs 单测：bootstrap 一次性/吊销/过期/最后管理员不可撤、密码 roundtrip、用户生命周期（改密踢会话/禁用双拒/列表无 hash）、配额默认 3 与提额边界、GitHub 绑定后缀/禁用/空密码拒——**断言有效，未发现假绿**。
- 守卫覆盖清点：Router 全部路由（:69-87）逐一对到 handler 首行 guard/own_guard，**无漏守卫路由**；公开面仅 `/`、/api/login、/api/login/password、/api/logout、/api/oauth/start|callback——均为登录流，合理。
- service 车道：方法白名单在 serve 侧 fail-closed（对照 013 评审已核）；console 侧 CN 组装不含用户输入（svc_cn=launch_token 前 8 位，非用户可控）。

## 5. 结论

鉴权模型（双轨守卫+归属门+会话+OAuth）**设计正确、无旁路**；三个必改全部集中在**用户输入进入高权限面的两个缝隙**（UI 渲染与 image/server_url 透传），修复面小（各 1-2 行+负例测试），修后可合并。建议同批补 3 条负例进 tests/platform-console-test.sh。
