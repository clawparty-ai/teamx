# 16 — Code Review 报告：透明代理与 DNS（2026-08-24）

> 范围：全项目评审，重点是 `7118e8c` 引入的透明代理/本地 DNS 工作及后续修复。共进行了三轮评审；每轮发现的问题在同一轮内修复并复审，直到不再出现新发现。
>
> 提交：`2601e14`（DNS fallback）、`dccae8e`（第一轮修复）、
> `607a729`（第二轮修复）。

## 评审方法

每一轮以不同视角走读代码的不同切片：

1. **第一轮 — 架构与正确性**：一次 DNS 查询和一条 TCP 连接经 dns_proxy → server RPC → exit → tun0 smoltcp → bridge 的数据流；资源生命周期（waiter、pipe、内存分配）。
2. **第二轮 — 安全与边界**：固定路径上的 symlink/TOCTOU、DNS 污染面（A vs AAAA）、无界增长、输入校验。
3. **第三轮 — 收敛**：重读所有修复，扫查其余模块（metrics、db migrations、table views、app entry）是否有遗漏。

## 发现与修复

### 第一轮 — 架构与正确性

| ID | 严重度 | 文件 | 发现 | 修复 |
|---|---|---|---|---|
| H1 | High | `dns_proxy.rs`, `tunnel_client.rs` | 没有 DNS 缓存：对被拦截域名的每次查询都要新建 tokio runtime + 与服务器做 mTLS 握手（约 1 秒）。 | `dns_proxy` 内加 60 秒 TTL 进程内缓存；重复查询直接由内存应答。 |
| H3 | High | `serve.rs` | `team.resolve_dns` 每个请求注册一个 oneshot waiter；若出口从不回复（6 秒超时），waiter 会永远留在 `resolve_waiters` 里 — 缓慢的内存泄漏。 | 超时/出错时 waiter 被完成并丢弃（`complete_resolve(sid, [])`）。 |
| H4 | High | `TeamxCore.run` (Swift) | 经典 pipe 死锁：先 `waitUntilExit()` 再排空 stdout/stderr。任何超过 64 KB 的 CLI 输出（大路由表）都会挂死应用。 | 先把两个管道读到 EOF，再等待退出。 |
| H5 | High | `ControlPanelController.refreshConnection` | 面板每 2 秒刷新一次，在主线程上做三次同步 mTLS curl 调用（每次最长 5–8 秒）— 服务器慢时 UI 冻结可达约 20 秒。 | 网络调用移到后台队列；只有 UI 更新跳回主线程。 |
| M2 | Med | `tun_stack.rs` | `TxToken::consume` 为每个发出的包（SYN-ACK、数据 — 高频）都堆分配新缓冲区。 | 在 `TunPhy` 中复用 scratch buffer（与 rx_buf/tun 分裂借用）。 |
| M3 | Med | `tun_stack.rs` | ICMPv4 校验和仍为 "ignored"，而 IPv4/UDP/TCP 已在 TX 计算；发出的 ICMP 会带校验和 0。 | 改为 `icmpv4 = Checksum::Tx`。 |
| L1 | Low | `tun_socks.rs` | CIDR 网络路由在启动时被添加两次（调用方 + `ip_route_loop`），产生重复的 "File exists" 噪音。 | 从 `ip_route_loop` 中移除（由调用方一次性添加）。 |

### 第二轮 — 安全与边界

| ID | 严重度 | 文件 | 发现 | 修复 |
|---|---|---|---|---|
| F1 | **High（安全）** | `Privileged.swift`, `gui_panel.rs`, `TeamxCore.swift` | 提权（root）进程把日志写到固定路径 `/tmp/teamx-tun0.log`。`/tmp` 全局可写：任何本地用户都可以把它预创建为指向例如 `/etc/passwd` 的符号链接；shell 重定向会跟随符号链接，root 进程就会截断/覆写任意文件（CWE-61）。 | 日志移到 `$TEAMX_HOME/tun0.log`（export 进提权 shell；目录用 `mkdir -p` 创建）。面板的日志 tail 通过 `NSHomeDirectory() + "/.teamx/tun0.log"` 读同一位置。 |
| F2 | High | `dns_proxy.rs` | 服务器不可达时，每个被拦截域名的查询都要阻塞完整的约 15 秒 RPC 超时，且串行执行、没有负缓存 — 客户端等待后回落期间 DNS 完全停摆。 | 失败的解析也被缓存（TTL 10 秒；成功为 60 秒），因此每 10 秒窗口内每个不同域名最多付出一次超时代价。 |
| F2b | High | `dns_proxy.rs` | 被拦截域名的 AAAA 查询被转发到上游（被审查）解析器，返回被污染的 IPv6 地址；偏好 IPv6 的应用会绕过代理而失败。 | 对被拦截域名的非 A 查询现在返回**空的 NOERROR** 应答（`build_empty_response`）；客户端回落到代理的真实 IP A 记录。未被拦截的域名仍然转发上游。 |
| F3 | Med | `dns_proxy.rs` | 长会话中 `cache` 和共享 `ip_map` 无界增长。 | 加上限：cache ≥1024 条时修剪（丢弃过期项）；ip_map ≥8192 时清空。 |
| UX1 | Low | `DraggableTable.swift` | 表格在任意位置拖动都会 resize（不只是从把手拖动），且每个鼠标拖动帧都写 UserDefaults。 | 只有按住把手才开始拖动；高度在 `mouseUp` 时持久化一次。 |

### 第三轮 — 收敛

重读全部修复；扫查了 `metrics.rs`、DB 迁移、`SimpleTable`、
`main.swift` 以及 resolve 授权路径：

- `team.resolve_dns` 只解析调用者自己团队内的出口
  （`registry.get(team_id, name)` + `teams_for_member`）— 不存在跨团队访问。
- `build_dns_response` 的包长算术不会溢出（payload 受 2048 字节套接字缓冲限制）。
- 其余接受项（见下文记录）。

## 接受 / 暂缓项

以下问题已发现但有意暂不修改：

| 项目 | 暂缓原因 |
|---|---|
| 主轮询循环即使空闲也每 2 ms 醒来（软忙等循环）。 | 正确修法是用 `tokio::AsyncFd` 等待 tun fd；需要重构非 Send 的单线程 runtime。当前 CPU 开销很小。 |
| `open_tunnel_bridge(...).await` 在 tun0 主循环中内联运行；挂起的服务器连接会阻塞所有连接直到超时。 | 架构性（单线程 smoltcp 栈不是 `Send`）。已用 HTTPS 15 秒上限缓解；正确修法需要 spawn + channel 重设计。 |
| 多个 CDN 域名共享同一 IP；`ip -> domain` 映射保留最后写入者，TLS SNI 可能使用兄弟域名（例如对一个 `googleapis.com` 的 IP 拨号 `google.com`）。 | Google 边缘节点接受它自己的任意名字；未观察到影响。真正的修复需要从 client hello 透传 SNI。 |
| `MetricsRegistry::snapshot` 会重置计数器，两个并发 snapshot 会把计量的字节一分为二。 | 目前 snapshot 调用方实际上已被串行化；影响小。 |
| 服务器不可达时 `resolve_dns` 会阻塞唯一 DNS 线程至多 15 秒。 | 有负缓存后，每个域名每 10 秒最多阻塞一次；彻底修复与上述 AsyncFd/线程重构重叠。 |
| `mTLSEnvPrefix` 用环境变量拼接 shell 字符串（单引号转义，双引号再为 AppleScript 转义一次）。值来自用户自己的环境，所以只属于自伤型输入，但辅助二进制会比字符串拼出的提权更干净。 | 目前重构风险大于收益。 |

## 修复后的验证

```
cargo build -p teamx   → 0 warnings
cargo test -p teamx    → 73 passed (incl. new build_a_response answer-count test)
swift build            → 0 warnings
```

端到端（系统 DNS = `127.0.0.1` + 原 DNS fallback）：

- `dig www.google.com` → ANSWER: 8，真实 Google IP（经出口）
- `curl https://www.google.com/generate_204` → 204
- `curl https://www.baidu.com` → 200（未拦截，直连）
- 被拦截域名的 AAAA 返回 NOERROR/NODATA（没有被污染的 IPv6）

## 后续工作（future work）

1. 若 tun0 进程死亡且系统 DNS 指向 `127.0.0.1`，提供看门狗/自动恢复（当前作为已知限制记录）。
2. 基于 AsyncFd 的 tun 就绪通知，消除 2 ms 轮询。
3. 桥接任务从主循环 spawn 出去，避免挂起的 exit 阻塞 tun0。
