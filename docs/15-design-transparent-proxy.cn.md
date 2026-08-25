# 设计 15 — 透明代理（本地 DNS 代理 + 出口解析）

> 状态：已在 macOS（客户端）+ Linux（服务器/出口）上实现并验证。
> 日期：2026-08-24

## 1. 背景 / 问题

`tun0`（一个 root TUN 设备）拦截匹配的流量并经由 teamx **proxy exit**（例如海外节点上的 `egress2`）隧道转发。对"透明代理"（应用无需配置代理）而言，棘手的部分是 DNS：

- **放弃了 Fake-IP DNS。** macOS 的 `mDNSResponder` 无法可靠地接受来自 `198.18.0.0/15` 保留网段的响应，因此系统查询要么回落到被审查的解析器，要么超时。它还劫持了 tun0 自身与 teamx 服务器的连接（服务器主机名被解析成假 IP，破坏了桥接）。
- **仅按 IP 路由不够。** Google/YouTube 的 CDN IP 数量庞大、动态且按地域分布。预解析少数域名会漏掉大多数 IP，而且系统解析器返回的是**被 GFW 污染**的地址（实测观察到：`185.45.5.35`、`174.132.167.252`、`104.244.42.197`），根本不是 Google 的 IP。
- **在被审查的网络里公共 DNS 不可用：** `8.8.8.8`、`1.1.1.1` 的 UDP，以及 DoH 端点（`dns.google`、`cloudflare-dns.com`）全部被劫持或封锁。甚至服务器节点（`hub03`，阿里云，在 GFW 内）也会把 Google 解析成一个 Facebook IP。

**只有海外出口（如 AWS 东京上的 `egress2`，`35.79.166.197`）拥有未被审查的解析器并能直达 Google**（已验证：能解析出
`142.251.x`，`curl -I https://www.google.com` → 204）。

## 2. 选定的设计

在 `127.0.0.1:53` 上运行一个**本地 DNS 代理**（loopback，`mDNSResponder` 可以正常与之通信），并把系统 DNS 指向它。该代理按域名决策：

- **被拦截的域名**（匹配路由表的域名规则）→ **经由 teamx 通道到出口解析**（用 `egress2` 未被审查的解析器）；返回的真实 IP 作为主机路由加到 tun 设备上并应答给客户端。随后应用连接真实 IP，主机路由把它送进 tun0，tun0 经出口代理出去。
- **其余一切** → 原样转发给上游系统 DNS（正常/虽被审查但对国内站点无碍）。

这样应用就能透明、不受审查地访问被代理域名，**没有 DNS 劫持**且**没有假 IP**。

## 3. 组件映射

### 3.1 客户端（macOS，运行于 `teamx tun0 start` 进程中）

| 文件 | 职责 |
|---|---|
| `crates/teamx/src/dns_proxy.rs`（新增） | `127.0.0.1:53` 上的本地 DNS 服务器（专用阻塞线程）。对被拦截域名调用 `tunnel_client::resolve_dns`，通过 `tun_dev::add_ip_route` 为每个真实 IP 加路由，把 `ip -> domain` 记入共享 `ip_map`，并用 `build_a_response` 应答。未拦截的查询转发给上游 DNS。 |
| `crates/teamx/src/tun_dns.rs` | 新增辅助函数：`parse_dns_query`（QNAME + qtype + question 结束偏移）和 `build_a_response`（A 记录应答；注意 **TTL 是 u32**，不是 u16）。同时保留 fake-IP 响应器供可选的 `--fake-dns` 模式使用。 |
| `crates/teamx/src/tun_socks.rs` | `run_tun_proxy` 启动 DNS 代理，调用 `tun_dev::set_system_dns_single("127.0.0.1")`，并保留 `ip_route_loop`（周期性做 域名→IP 主机路由兜底）+ 面向大型 CDN 网段的 CIDR 网络路由。`resolve_target` 会查 `ip_map`，尽量让 tun0 按主机名拨号（保留 TLS SNI）。 |
| `crates/teamx/src/tun_dev.rs` | 新增：`set_system_dns_single`（单一 DNS，无 fallback；保存备份）、`system_dns_servers`、`add_ip_route`/`del_ip_route`。既有的 `set_system_dns`/`restore_system_dns`（fake-IP 模式）保留原 DNS 作 fallback 并备份到 `~/.teamx/dns-backup.json`。 |
| `crates/teamx/src/tunnel_client.rs` | 新增 `resolve_dns(server_url, exit, name)`（调用服务器 RPC）。**出口侧**增加了 `resolve` 指令处理器，用出口自己的解析器解析并回复 `resolve_result`。 |
| `crates/teamx/src/cli.rs` / `commands.rs` | 新增 `teamx dns` 命令：`dns list`（默认系统 DNS）和 `dns resolve <domain>`（经出口，不受审查）。另有 `Tun0Cmd::Start --fake-dns`（默认关闭）。 |

### 3.2 服务器侧（`teamx serve`，hub03）

`crates/teamx/src/serve.rs` + `crates/teamx/src/tunnel.rs`：

- `TunnelRegistry::resolve` 向指定 proxy exit 发送 `resolve` 帧，
  并注册 oneshot waiter（`resolve_waiters`）。
- `team.resolve_dns` RPC：找到调用者所在团队，转发给出口，
  至多等待 6 秒拿 `resolve_result`，返回 IP 列表。
- provider 的 `/tunnel` 处理器识别 `resolve_result` 并完成 waiter。

### 3.3 出口侧（`teamx proxy exit`，egress2）

`crates/teamx/src/tunnel_client.rs` 的 `expose_once` 增加了 `resolve` 指令处理器
（用出口的系统解析器解析 — 不受审查）。

## 4. `teamx dns` CLI

```
teamx dns list                # 默认系统 DNS（macOS 上即 scutil --dns）
teamx dns resolve <domain>    # 经默认出口解析（不受审查）
teamx dns resolve <domain> --exit <name>
```

## 5. 控制面板重设计（Swift 应用）

`app/Sources/TeamxApp/ControlPanelController.swift` 被重构为**上下布局**：

- **底部**：终端风格日志（等宽 `NSTextView`，固定高度），带复制/清空操作。
- **顶部**：`NSSegmentedControl` 标签栏，在 8 张卡片间切换：
  1. 连接状态 (server/member presence + metrics table)
  2. 虚拟网卡 (tun0 start/stop/restart + status)
  3. SOCKS5 代理 (proxy start/stop + status)
  4. 默认出口 (default exit picker)
  5. 隧道 (tunnel table, read-only)
  6. tun0 路由规则 (route table)
  7. **路由表** (默认路由 + **traceroute** 查询框 → `traceroute <host>`)
  8. **DNS** (默认 DNS + **域名解析**查询框 → `teamx dns resolve`)

新增卡片辅助方法：`buildCards`、`buildRouteTableCard`、`buildDNSCard`、
`makeTermScroll`、`showCard`、`tabChanged`。

## 6. 一并完成的其他改动

- **菜单冻结修复**：点击托盘图标打开菜单时，`menuNeedsUpdate` 在主线程上同步执行 `tunnel list`（一次服务器往返），导致输入冻结。`defaultExit()`/`listExits()` 现在读取后台刷新的缓存（`TeamxCore.refreshExitCache`，每 5 秒 + 启动时各刷一次）。
- **自动登录取消**：移除了 `~/Library/LaunchAgents/io.flomesh.teamx.plist`；
  `build-teamx-app.sh` 只在带 `--install-agent` 时才安装它。
- **tun0 核心修复**（整条链路工作所需）：
  - smoltcp `TxToken` 现在真正把数据包写回 tun fd（此前应答被静默丢弃）。
  - TUN fd 设为非阻塞；`rx_buf` 不再被截断（截断会缩小读缓冲并截断大 SYN 包）。
  - macOS utun 会把 TCP SYN 截掉 4 字节但保留 `total_len` → 解析前补齐到 `total_len`。
  - 校验和：RX 跳过验证，TX 上**计算**（`Checksum::Tx`）；TCP 校验和为 0 会被主机协议栈拒绝。
  - `take_new_connection` 只在 `State::Established` 触发；握手进行中的 socket 不再被当作 EOF 重置。
  - `open_tunnel_bridge`：`stream_open` 等待期间读出的 `stream_id` 被调用方消费掉，生成的泵任务永远看不到 → `sid` 总是 `None`，所有出站数据被无限缓冲。现在由调用方捕获并传入。
  - tun 轮询循环改为异步睡眠（同步 `phy::wait` 会饿死 current-thread 运行时，桥接任务永远得不到执行）。

## 7. 部署说明

```
# client (macOS) — build & copy into the app bundle
cargo build -p teamx
./scripts/build-teamx-app.sh            # no --install-agent → no auto-login

# server (hub03, x86_64) + exit (egress2, aarch64)
rsync -az --delete --exclude target --exclude dist --exclude app \
  --exclude .git --exclude docs --exclude node_modules --exclude '*.db' \
  ./ ubuntu@<host>:~/teamx/
ssh <host> 'export PATH=/home/ubuntu/.cargo/bin:$PATH; cd ~/teamx && cargo build --release'
ssh <host> 'sudo systemctl stop <svc>; cp ~/teamx/target/release/teamx ~/.local/bin/teamx; sudo systemctl start <svc>'
```
- hub03: `teamx-serve` (+ `teamx-proxy-exit`)，端口 8888。
- egress2: `teamx-proxy-exit2` → `start-exit2.sh`。

## 8. 验证（2026-08-24）

在 `tun0` 运行且系统 DNS = `127.0.0.1` 时：

```
networksetup -getdnsservers Wi-Fi        → 127.0.0.1
dig www.google.com A                     → ANSWER: 8 (142.251.x, real Google IPs)
curl -skI https://www.google.com/generate_204 → 204
curl -sI https://www.baidu.com           → 200 (non-intercepted, direct)
teamx dns list                           → 192.168.31.1
teamx dns resolve www.google.com         → 142.251.150.119 …
```

## 9. 已知限制

- 若 tun0 进程死亡，系统 DNS 停留在 `127.0.0.1`，网络不可达，直到执行 `teamx tun0 stop`（恢复 DNS）或手动复位。看门狗/自动恢复是后续工作。
- `dns resolve` 只应答 A 记录；AAAA 转发上游。
- IP 主机路由每 5 分钟刷新一次（CDN 轮换）；实际使用中 DNS 代理每次查询都会重新加路由，覆盖接近实时。
- traceroute 在客户端运行（不经过出口）；显示的是客户端路径，可能部分被阻断。
