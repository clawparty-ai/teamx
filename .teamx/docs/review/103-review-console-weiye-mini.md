# 评审：Teamx Cloud 控制台代码（用户/鉴权/my-teams/OAuth/UI）—— task-103

> 评审人：weiye-mini（harness 会话）· 日期：2026-09-23 · 对象：origin/main @ `b8594fc`
> 范围：`crates/teamx/src/platform_console.rs`（457 行）+ UI 单文件 + `tests/platform-console-test.sh`
> 参照：design/014 §5/§6.5/§8 + §11 裁决（O-1(a)/O-2/O-5、复用现有栈、OAuth 网络受限口径）
> **verdict：CHANGES-REQUESTED（1 必改 + 2 强烈建议 + 3 建议）**——骨架与安全设计总体扎实（见「亮点」），但存在一处存储型 XSS 必改。

---

## 必改

### M1 · UI 存储型 XSS：用户可控字段未转义直插 innerHTML（高危）
- **事实**：`INDEX_HTML` 的 `loadMine()` 用模板串拼 HTML 后 `innerHTML` 赋值，`${t.team_name||'未命名团队'}`、`${t.server_image||''}` 等**未转义**（platform_console.rs 内 INDEX_HTML ~929 行）。`team_name` 是**用户任意输入**（`api_my_teams_create` 仅校验非空，`create_my_tenant` 落库），`server_image` 同为请求可控。
- **攻击链**：用户 A 建 team 名为 `<img src=x onerror=fetch('/api/my/teams/bootstrap/…',{method:'POST',body:…})>` 之类的载荷 → 任何查看 my-teams 的会话（含**运维 launch-token 会话**，其 cookie 即平台全权凭据）渲染时执行——存储型 XSS，且受害者是高权限运维会话。
- **必改**：所有用户可控字段（team_name / server_image / namespace 等）入 HTML 前过统一 `esc()`（`&<>"'`），或改为 DOM `textContent` 构建；运维面 `/api/tenants` 渲染路径同查。
- **回归**：platform-console-test.sh 增断言：建 team 名含 `<img onerror>`，GET my-teams 响应体中该串须为转义形态（`&lt;img`）。

## 强烈建议（与必改同批处理）

### S1 · 会话 cookie 语义不一致 + 缺 Max-Age
- 密码登录 SET_COOKIE：`SameSite=Strict`、**无 Max-Age**（注释称"7 天会话"但浏览器按会话 cookie 处理，关浏览器即失效，与服务端会话 TTL 脱节）；OAuth 回调 SET_COOKIE：`SameSite=Lax`。**同一 cookie 名两种 SameSite**，后登录者覆盖前者——且 OAuth 回调的 Lax 会把此前的 Strict 也降级。
- **改**：统一 `SameSite=Lax`（OAuth 302 回跳必须 Lax）；用户会话 cookie 加 `Max-Age=604800` 对齐服务端 7 天；运维 launch-token cookie 语义单列（明确 Max-Age 或保持会话级并在文档声明）。

### S2 · launch token 非常量时间比较
- `guard()` 用 `t == ctx.launch_token`（`String == String`）——逐字节短路比较，理论上存在时序侧信道。launch token 为高熵随机值时风险低，但它是**平台全权凭据**且长期不变。
- **改**：constant-time 比较（`subtle` crate 或先 `sha256` 再比较）；顺手考虑 ops cookie 与用户会话 cookie **分名**（现同名 `teamx_platform_token`，两种凭据语义挤一个值，guard 靠前缀匹配区分，扩展性差）。

## 建议（非阻塞）

- **R1** `svc_cn = "console-{launch_token[..8]}"`：CN 派生自 launch token——token 轮换后 CN 变化，serve 侧方法白名单与 O-2 台账审计将出现新旧两 CN 断档。改用稳定 instance 标识。
- **R2** `with_conn` 每请求 `open + migrate`：v1 可接受；上量后迁移检查的写锁与 SQLite 并发值得 OnceLock 化（迁移只跑一次）。
- **R3** OAuth state 内存态（单次 + 10min，已有注释）——v1 单副本 OK（§11 已裁）；多副本部署时需共享存储，届时再议。

## 亮点（评审确认，非客套）

- **鉴权分层干净**：fail-closed guard（launch token 先比、会话后验）；用户面路由 `session_user` 对运维 token 显式返回 FORBIDDEN；**归属先于参数**（own_guard 403 早于 400，防参数侧信道探测）。
- **service 车道**：CN 前缀由签发函数强制（调用方不可自带）；方法白名单 serve 侧 fail-closed；**每次调用重签短 TTL 证书并落 O-2 台账**（失败不阻断但记日志）——与 O-2 裁决逐条对齐。
- **OAuth fail-closed 全链**：state 单次 + 10min + 泄漏自清（retain）；`TEAMX_GITHUB_BASE/API_BASE` 可覆盖（mock 测试已验证，G2 断言）；github_id 锚定不自动并户、禁用用户拒绝、secret 永不进日志。
- **O-1(a)「全新开始」入口完整**：配额 → 空壳 team（失败清孤儿行 a5865b9）→ 租户 infra → bootstrap（O-5 server_url 必填）→ 首团队 + owner 邀请。
- **测试覆盖扎实**：platform-console-test.sh 48 断言，含 mock OAuth 全流程、HTTP/2 拆分 cookie 回归。

## 结论

三处安全加固（M1/S1/S2）落地后即可转 **PASS**——均为局部改动（UI 转义 + cookie 头 + 比较函数），不动架构。控制台代码整体质量高于同期平均水平，O-2/服务车道实现可作为后续切片的参照实现。

---

## 勘误补记（同日 · 对象更新至 0a01831）

评审发出后 main 前进两个 style 提交（6e7c7de 卡片流、0a01831 全流式），platform_console.rs 457→1008 行。已在 **origin/main @ 0a01831** 逐项复核本评审：

- **M1（存储型 XSS）确认成立且范围扩大**：未转义 innerHTML 插值在最新 main 上共 5 处——`loadMine()`（924/926/931：team_name/server_image/namespace）之外，新增 **`loadTenants()`（934：ops 面租户表格，server_image 同样未转义）** 与 tokens/admins 表（936/937，来源为平台数据风险较低）。两个新提交是纯样式重写，**未引入转义**——M1 必改维持，修复面从 my-teams 扩到 my-teams + tenants 两段。
- **S1（cookie SameSite 不一致）确认成立**：187/221 行仍为 Strict，752 行 OAuth 回调仍为 Lax，同名单互覆盖。
- **S2/R1–R3** 不受这两个 style 提交影响，维持原结论。

结论不变：**CHANGES-REQUESTED**，M1/S1/S2 在 0a01831 上落地即转 PASS。
