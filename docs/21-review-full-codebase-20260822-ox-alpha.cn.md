# 21-review: 全库 Code Review（完整）

| 项目 | 内容 |
| --- | --- |
| Review 日期 | 2026-08-22 |
| Reviewer | LLM: **ox-alpha**（opencode 会话） |
| 范围 | Rust 核心 13 文件（~7300 行）、opencode-plugin、dsh-plugin（TS ~4600 行源码）、install.sh、tests/ |
| 方法 | 逐文件人工级精读 + 交叉验证调用链；`cargo test` / `clippy` / 两插件 `tsc --noEmit` / smoke·concurrency·cli-edge E2E 实测复跑 |
| 验证结果 | cargo test 41/41 通过；clippy 0 warning；typecheck 全过；smoke/concurrency/cli-test 全绿 |

---

## 总体评价

架构质量高于平均水平：append-only ledger + 状态机投影设计清晰，seq 分配与写入同事务保证单调；
mTLS 身份从证书 CN 派生而非自报 session 的决策正确；SOCKS5 解析边界严谨且有单测。

主要问题集中在：**隧道子系统的一致性/授权盲区**、**若干私钥文件权限疏漏**、
**dsh-plugin 与 opencode-plugin 行为漂移**。

---

## High

### H1 被吊销成员仍可使用隧道
- 位置：`crates/teamx/src/serve.rs` `handle_tunnel_ws` / `handle_tunnel_forward`
- `/ws` 在握手后检查 `is_revoked`（serve.rs:551），RPC 层也拦截（serve.rs:761），
  但两个隧道 WS 入口只查 `teams_for_member`（排除 left/denied），不查吊销。
  吊销 invitation 后证书仍通过 mTLS 握手，数据面完全绕过鉴权：
  可继续注册隧道 / 充当 proxy exit 出网。
- 修复：两个隧道 handler 在解析 member_id 后增加 `is_revoked` 检查，吊销即断开。
- 状态：✅ 已修复（本轮）

### H2 同一 provider WS 注册第二个隧道会泄漏第一个
- 位置：`crates/teamx/src/serve.rs` `handle_tunnel_ws`
- `owned: Option<String>` 在第二次 register 成功后被覆盖；断开时只清理最后一个名字，
  先前注册的隧道条目与 frp 监听端口永久泄漏（直至重启或显式 close）。
- 修复：改用集合跟踪本连接注册的全部隧道；断开逐一清理；unregister 同步移除；
  二进制帧路由按全局唯一 stream_id 定位所属隧道（不再依赖单一 owned 名字）。
- 状态：✅ 已修复（本轮）

### H3 TEAM.md 成员 key 未消毒 → 路径穿越
- 位置：`crates/teamx/src/teamfile.rs:129-131`（key 原样取自 `### <key>`）
  + `crates/teamx/src/commands.rs` `bootstrap_from_teamfile`（`members_dir.join(&m.key)`）
- `### ../../../evil` 会把 AGENTS.md / invitation.letter（含客户端私钥）写到项目外。
  克隆他人仓库后执行 `team create` 即自动触发 bootstrap。
- 修复：解析时校验 key 仅含 `[A-Za-z0-9._-]` 且不含路径分隔符/不以 `.` 开头，
  非法 key 报错（TEAM.md 解析错误会以 warning 呈现且不阻塞建队）。
- 状态：✅ 已修复（本轮）

---

## Medium

### M1 `team join` 可加入已 destroy 的团队
- 位置：`crates/teamx/src/commands.rs` `cmd_team_join`
- 只拒绝 `completed|archived`；destroyed 通过检查后成员永远 pending 且对所有命令不可见
  （`memberships_for_session` 过滤 destroyed 团队）。
- 修复：拒绝条件加入 `destroyed`。状态：✅ 已修复（本轮）

### M2 隧道拨号失败不通知对端，流半开挂起
- 位置：provider 侧 `crates/teamx/src/tunnel_client.rs` `run_expose`（dial 失败只打日志）
  + `opencode-plugin/src/tunnel.ts` `openStream`（连接错误只删 map）
- server 端 stream 条目与 consumer 无限等待；应向 server 发送 `close_stream`。
- 状态：✅ 已修复（Rust + TS 双侧）

### M3 frp 中继 bind 失败留下僵尸条目
- 位置：`crates/teamx/src/tunnel.rs` `run_tcp_relay`
- bind 失败返回 Err 但 registry 条目与端口保留，状态显示 active 实际不可用。
- 修复：bind 失败时 `registry.remove(team, name)` 回滚并释放端口。
- 状态：✅ 已修复（本轮）

### M4 私钥文件权限疏漏（4 处）
- `crates/teamx/src/pki.rs::write_pem`：先写后 chmod，存在 0644 窗口 → 改为 `OpenOptions.mode(0o600)` 一步创建。✅
- `crates/teamx/src/commands.rs::store_letter`：同上 → 同样改为原子 0600 创建。✅
- `crates/teamx/src/commands.rs::cmd_cert_issue --out`：member.key 完全没有 chmod → 补 0600（cert 也一并收紧）。✅
- `crates/teamx/src/commands.rs::bootstrap_from_teamfile`：invitation.letter（含私钥）写入项目目录无 chmod → chmod 0600，并在 `.gitignore` 增加 `.teamx/members/` 防误提交。✅
- 状态：✅ 全部修复（本轮）

### M5 dsh-plugin RPC slots 表缺项，网络模式命令损坏
- 位置：`dsh-plugin/src/client.ts` `cliArgsToRpc` slots
- 相比 opencode-plugin 缺 `'loopx.report': ['project']`、`'tunnel.status'/'tunnel.close': ['name']`
  → 这三个命令在网络模式下位置参数被丢弃而报错。两份手抄表已经漂移。
- 修复：补齐缺失 slot。状态：✅ 已修复（本轮）；长期建议抽共享模块（未在本轮做）

### M7 forwardTunnel 端口 0 时上报错误端口
- 位置：`opencode-plugin/src/tunnel.ts` `tryBind`
- `bound = port` 在 listen 前赋值；port=0 时实际绑定随机端口但 ready() 返回 0。
- 修复：成功回调 resolve `server.address().port`；错误分支透出 errno 信息。
- 状态：✅ 已修复（本轮）

### M8 expose/forward 的 ready() 竞态
- 位置：`opencode-plugin/src/tunnel.ts`
- 若 ack/bind 完成早于调用方调用 `ready()`，结果被丢弃，ready() 空等 10s 后误报失败。
- 修复：保存最近一次已知结果，`ready()` 若已有结果立即返回。
- 状态：✅ 已修复（本轮）

### M9 opencode-plugin WS 收到吊销 close 后无限重连
- 位置：`opencode-plugin/src/ws.ts`
- 对 `{type:"close",code:"revoked"}` 哨兵无特殊处理，onclose 一律重连永不停止
  （dsh 版已正确处理 gaveUp）。修复：收到 error/close 哨兵按 code 判定是否终止重连。
- 状态：✅ 已修复（本轮）

### M10 插件默认以 0.0.0.0 启动 server，与 CLI 安全默认相悖
- 位置：`opencode-plugin/src/serve.ts` `serveStart` 默认 `addr="0.0.0.0"`（CLI 默认 127.0.0.1）。
- mTLS 能兜底 RPC，但会把 frp 隧道端口暴露到 LAN。
- 修复：默认改 127.0.0.1，需要 LAN 时显式传 addr。
- 状态：✅ 已修复（本轮）

### M6 全局单锁串行化所有 RPC（本轮不修，记录为后续工作）
- 位置：`crates/teamx/src/serve.rs` 单个 `Mutex<Connection>` 包住全部请求含只读。
- 建议：r2d2_sqlite 连接池 + WAL 并发读；涉及架构调整，单独排期。

---

## Low

| # | 位置 | 问题 | 本轮处理 |
| --- | --- | --- | --- |
| L1 | `broadcast.rs::subscribe` | 同一 member 第二条 WS 连接覆盖旧 sender，旧连接静默失联；unbounded channel 慢消费者积压 | ✅ 订阅键加每连接唯一后缀，多连接共存；close 哨兵语义不变 |
| L2 | `events.rs` row_to_event / emit | payload 损坏静默变 None/空串 | ✅ 解析失败 eprintln 告警（不改变返回语义） |
| L4 | `state.rs` vs `publish_plan` | 团队级 `(Blocked,PublishStart)=>Active` 与目标级规则表不一致（该边经 publish 实际不可达） | ⏸ 不改语义；仅在此存档说明 |
| L5 | `tunnel_client.rs` env_mtls | 坏 PEM 直接 panic!；https_post 无超时/不查状态码 | ✅ panic 改为 Err 返回；https_post 加超时与状态码检查 |
| L6 | `loopx.rs` 超时线程泄漏 | timeout 后 detached 线程继续等子进程 | ⏸ 未处理（影响小） |
| L7 | `cmd_ask` 等 | pending 成员可发起提问（publish 有拦截）；`ensure_owner` 死参数；cliArgsToRpc 三处拷贝 | ⏸ 未处理 |
| L8 | `install.sh` / `serve.ts` | 版本回退硬编码；clearRecord 写 {} 而非 unlink | ⏸ 未处理 |

## Nit（未处理，备忘）

- `serve.rs` IPv6 bind 判断 `contains(':')` 的报错信息不友好
- `main.rs print_human` 嵌套对象降级 JSON 输出
- `cmd_sync` 跨团队事件按 per-team seq 排序无意义
- dsh client.ts maxBuffer 1MB 上限
- `TunnelCmd::*` 的 `--session/--team` 为从未使用的死参数（身份来自证书），误导用户

---

## 做得好的地方

1. Ledger + 状态机投影：seq 在 Immediate 事务内分配，跨进程并发实测正确（15 并发 seq 严格递增）。
2. sync cursor 用 `MAX(last_seq, excluded.last_seq)` 单调推进并有回归测试。
3. 身份模型：授权一律走证书 CN；`role set owner` 防篡夺；吊销后 RPC 拦截。
4. SOCKS5 解析器纯函数 + 完整边界测试；store_letter 的 UUID 校验防穿越意识好。
5. 测试文化：41 个单测 + 13 类 E2E 场景脚本。

## 本轮修复后的验证

- `cargo test --workspace`：全过
- `cargo clippy --workspace`：0 warning
- opencode-plugin / dsh-plugin：`tsc --noEmit` 通过、bundle 构建通过
- `tests/smoke.sh`、`tests/concurrency.sh`、`tests/cli-test.sh`：全绿
- 新增单测：TEAM.md key 消毒（合法/穿越用例）、join destroyed 拒绝
