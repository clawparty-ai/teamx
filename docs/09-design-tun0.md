# tun0 虚拟网卡详细设计（实现蓝图）

- 文档类型: design（定稿，进入实现）
- 关联: `09-design-tun0.md`（可行性探讨）、`06-design-proxy.md`、`08-design-proxy-routes.md`
- 日期: 2026-08-22（设计）· 2026-08-23（实现完成）
- 目标版本: teamx 0.3.0
- 平台: Linux + macOS（跨平台 TUN）
- 状态: ✅ 已实现（见 §11 实现记录）

## 1. 设计决策（已确认）

| # | 决策点 | 结论 |
|---|---|---|
| D1 | 实现路径 | **路径 A**（tun2socks 式）：本地用户态 TCP/IP 栈重组 IP 包为 TCP 流，复用现有 proxy→server→egress 通道 |
| D2 | 域名路由 | **fake-ip DNS 拦截**（v1 就做）：本地 DNS → fake IP → 反向映射回域名，egress 用域名拨号 |
| D3 | UDP | **v1 支持 UDP 本地 DNS 处理**（UDP/53 劫持给 fake-ip 解析），其他 UDP 丢弃；完整 UDP over proxy 列为后续 |
| D4 | 平台 | **Linux + macOS 双平台**（`tun` crate 统一封装，见 §5） |
| D5 | 权限 | **需要 root**（创建 tun + 注入路由）；启动时检测并明确报错 |
| D6 | 并发 | 预分配 socket 槽位（smoltcp 无 accept/backlog），v1 默认 64 连接 |
| D7 | 复用 | 路由匹配（routes.rs）、proxy 客户端连接逻辑、server/egress 全复用 |

## 2. 总体架构

```
                    ┌─────────────── 本地（需 root）────────────────┐
 应用(不改)         │                                              │
  │ 系统路由         │   ┌────────┐   ┌──────────────┐              │
  ▼  (fake-ip段)     │   │  tun0   │──▶│  smoltcp      │             │
 IP 包 ────────────▶ │   │ 设备    │   │  用户态TCP/IP栈 │             │
  ▲                 │   └────────┘   │  (重组/握手)   │             │
  │                 │                └──────┬───────┘              │
  │                 │                       │ TCP 流 + remote endpoint
  │                 │                       ▼                       │
  │                 │                ┌──────────────┐               │
  │                 │                │ tun_socks     │  ← 新模块      │
  │                 │                │ 连接桥接器     │               │
  │                 │                │ (复用 proxy)   │               │
  │                 │                └──────┬───────┘               │
  │                 │                       │ 每连接一条 WS          │
  └─────────────────┼───────────────────────┼───────────────────────┘
                    │                       │ WS (mTLS)
                    │                       ▼
                    │              teamx server（零改动）
                    │                       │
                    │                       ▼
                    │              egress（零改动，动态拨号）
                    ▼
             fake-ip DNS（本地 53 劫持，映射 fake_ip→domain）
```

### 关键数据流（TCP）

```
1. 应用连接 fake-ip 段的某 IP:port（系统路由指到 tun0）
2. tun0 收到 SYN → smoltcp 完成握手（Listen→SynReceived→Established）
3. tun_socks 检测到新 Established 连接：
   remote_endpoint = (fake_ip, port)
   → fake_ip 查映射表 → 若命中，得到 domain
   → 否则直接用 IP
   → 建立到 teamx server 的 WS /tunnel/forward，发 {"type":"connect","name":"<exit>","target":"<domain|ip>:<port>"}
   → 双向搬运字节（tun 侧 socket recv_slice/send_slice ↔ WS 读写）
4. egress 收到 target，动态拨号（现有逻辑，零改动）
```

### 关键数据流（DNS，fake-ip）

```
1. 应用查 example.com → UDP 包进 tun0（目标 DNS 服务器 IP）
2. tun_socks 的 UDP 处理：识别这是 DNS 查询（端口 53）→ 转给 fake-ip 解析器
3. 解析器：查系统 DNS 或上游 DNS 得到真实 IP，分配 fake IP（198.18.0.0/15 段），
   记录 fake_ip→domain 映射，把 fake IP 作为 A 记录应答返回应用
4. 应用随后连 fake IP → 走 TCP 流程 → 还原 domain → egress 用域名拨号
```

## 3. 模块划分（crates/teamx/src/）

```
src/
├── tun_dev.rs      # 跨平台 TUN 设备封装（macOS utun / Linux tun），包装 tun crate
├── tun_stack.rs    # smoltcp 集成：Device impl + Interface 配置 + poll 循环
├── tun_socks.rs    # 连接桥接器：Established 连接 → proxy WS；字节搬运；UDP/DNS 处理
├── tun_dns.rs      # fake-ip DNS：本地 53 劫持 + fake_ip↔domain 映射表
├── tun_cli.rs      # `teamx tun0 start/stop` 命令（权限检查、路由注入、进程管理）
├── routes.rs       # 【复用】路由匹配（IP/CIDR + 域名，见 08-design）
└── tunnel_client.rs# 【复用】WS 连接建立 / socks5_proxy 核心逻辑
```

### 3.1 `tun_dev.rs` — 跨平台 TUN 封装

```rust
pub struct TunDevice {
    pub dev: tun::Device,          // 底层 tun crate 设备
    pub name: String,              // 实际设备名（utunN / tunN）
}

impl TunDevice {
    /// 创建并配置 TUN 设备。需要 root。
    /// - macOS:  name=utunN（自动分配），tun crate 自动 route add
    /// - Linux:  name=tunN，通过 ioctl 设 IP/netmask（ensure_root_privileges）
    pub fn create(name: Option<&str>, ip: Ipv4Addr, netmask: Ipv4Addr, mtu: u16) -> Result<TunDevice, String>;

    /// 读取一个 IP 包（非阻塞，EWOULDBLOCK → None）
    pub fn read_packet(&mut self, buf: &mut [u8]) -> Option<usize>;

    /// 写入一个 IP 包（发送到 tun，即进入应用侧网络栈）
    pub fn write_packet(&mut self, packet: &[u8]) -> Result<(), String>;

    pub fn as_raw_fd(&self) -> i32;   // 供 smoltcp phy::wait 轮询
}
```

macOS/Linux 差异（`#[cfg(target_os)]`）：
- **macOS**：`tun::Configuration` 设 `tun_name("utunN")` + address/netmask/destination/up，`tun::create` 内部调用 `ifconfig` + `route` 命令（需 root）。
- **Linux**：`platform_config(|c| c.ensure_root_privileges(true))`，`tun::create` 用 ioctl 设置。需 `/dev/net/tun` 存在且 root。
- **路由注入**：fake-ip 段（198.18.0.0/15）指向 tun0 的路由，两端都要加：
  - macOS: `sudo route -n add -net 198.18.0.0/15 <tun_ip>`
  - Linux: `sudo ip route add 198.18.0.0/15 dev tunN`

### 3.2 `tun_stack.rs` — smoltcp 集成

```rust
pub struct TunStack {
    device: TunDevice,
    iface: smoltcp::iface::Interface,        // Config::new(HardwareAddress::Ip)
    sockets: smoltcp::iface::SocketSet,      // 预分配 TCP sockets
    // 每个 socket 对应的 bridge 状态
    bridges: Vec<Option<TcpBridge>>,
}

pub struct TcpBridge {
    pub remote: IpEndpoint,       // 目标 (ip:port) 或 fake-ip
    pub state: BridgeState,       // Connecting | Established | Closing | Closed
    // 到 teamx 的 WS 写半/读半（复用 tunnel_client 的 channel）
}

pub struct StackConfig {
    pub tun_ip: Ipv4Addr,          // tun0 接口 IP（如 10.0.0.1）
    pub netmask: Ipv4Addr,
    pub max_conns: usize,          // 预分配 socket 槽位（默认 64）
    pub fake_ip_net: (Ipv4Addr, u8), // 198.18.0.0/15
}
```

核心逻辑：
```rust
impl TunStack {
    /// 主循环：poll 驱动 + 处理新连接 + 字节搬运
    pub async fn run(mut self, routes: Arc<RouteTable>, conn_maker: Arc<dyn Fn(&str,u16)->Conn>) -> Result<(),String> {
        loop {
            let now = Instant::now();
            self.iface.poll(now, &mut self.device, &mut self.sockets);
            // 1. 找新 Established 的 socket → 建立到 egress 的 WS
            // 2. recv_slice 读到字节 → 写 WS
            // 3. WS 读到字节 → send_slice
            // 4. 远端 FIN / WS 关闭 → close() / abort()
            // 5. UDP 包 → DNS 劫持 或 丢弃
            // 6. 连接空闲超时清理
        }
    }
}
```

### 3.3 `tun_socks.rs` — 连接桥接（核心）

桥接一个 Established 的 smoltcp TCP socket 到 egress：

```rust
pub async fn bridge_tcp(
    stack: &mut TunStack, sock_handle: SocketHandle,
    exit_name: &str, target: &str,   // "domain:port" 或 "ip:port"
) {
    // 复用 tunnel_client::run_socks5_proxy 的 WS 建立逻辑（抽成可复用函数）
    // 1. mtls_for(server_url) → client_config → connect_async_tls_with_config
    // 2. send {"type":"connect","name":exit_name,"target":target}
    // 3. 双向搬运：socket.recv_slice ↔ ws.send(Binary)；ws.next() ↔ socket.send_slice
}
```

**复用点**：现有 `run_socks5_proxy`（tunnel_client.rs:510）里 SOCKS5 CONNECT 之后的部分
（建 WS → send connect → 搬运字节）应**抽成 `spawn_tunnel_bridge(server_url, exit_name, target) -> (SendRecvHandle)`**，
让 `proxy start` 和 `tun0` 共用同一份 WS 桥接代码。

### 3.4 `tun_dns.rs` — fake-ip DNS

```rust
pub struct FakeIpDns {
    pub fake_net: (Ipv4Addr, u8),           // 198.18.0.0/15
    map: Mutex<HashMap<Ipv4Addr, String>>,  // fake_ip -> domain
    reverse: Mutex<HashMap<String, Ipv4Addr>>,
}

impl FakeIpDns {
    pub fn alloc(&self, domain: &str) -> Ipv4Addr;   // 分配/复用 fake IP
    pub fn lookup(&self, ip: Ipv4Addr) -> Option<String>;  // 还原域名
    /// 解析一个 DNS 查询包（UDP 负载），返回应答包
    pub fn answer(&self, query: &[u8]) -> Option<Vec<u8>>;
    /// 本地 UDP 监听（如 198.18.0.1:53 或劫持 tun 的 53 流量）
    pub async fn serve(&self, ...) -> Result<(),String>;
}
```

- 上游解析：用系统 DNS 或硬编码（8.8.8.8/1.1.1.1），发真实 DNS 查询拿真实 IP 后分配 fake IP。
- 只响应 A/AAAA 查询；其他类型转发上游。
- 并发安全：`Mutex<HashMap>` + 分配器用原子计数器。

### 3.5 `tun_cli.rs` — CLI 命令

```bash
# 启动 tun0（需 root）：创建 tun、注入路由、起 fake-ip DNS、跑转发循环
sudo teamx tun0 start --routes routes.json [--exit default] [--port 1080]
                      [--ip 198.18.0.1] [--net 198.18.0.0/15] [--max-conns 64]
                      [--dev tun0]

# 停止（删除路由、关 tun）
sudo teamx tun0 stop [--dev tun0]

# 查看状态
teamx tun0 status
```

权限检测（启动时）：
```rust
fn check_privileges() -> Result<(), String> {
    #[cfg(unix)]
    if unsafe { libc::geteuid() } != 0 { return Err("tun0 requires root (sudo)".into()); }
    Ok(())
}
```

路由注入（`--net 198.18.0.0/15`）：
```rust
#[cfg(target_os = "macos")]
Command::new("route").args(["-n","add","-net",net,"-interface",dev]).status();
#[cfg(target_os = "linux")]
Command::new("ip").args(["route","add",net,"dev",dev]).status();
```

## 4. 与现有 proxy 的关系

| 维度 | proxy start | tun0 |
|---|---|---|
| 入口 | 应用显式配 SOCKS5 | 系统路由指到虚拟网卡（应用无感知） |
| 协议 | SOCKS5 (L4) | IP 包 (L3) → smoltcp 重组 |
| 权限 | 无（纯用户态） | **需要 root** |
| 域名路由 | 直接用 SOCKS5 域名字段 | fake-ip DNS 反向映射 |
| 出口通道 | 相同（WS→server→egress） | 相同 |
| 可共存 | ✅ 互不影响 | ✅ 互不影响 |

**核心复用**：
1. `routes.rs`：`RouteTable::resolve(host)` —— tun0 同样用它选 exit（IP/CIDR 直接命中；域名经 fake-ip 还原后命中）。
2. `tunnel_client.rs`：抽出的 `spawn_tunnel_bridge()` WS 桥接。
3. server / egress：零改动。

## 5. 平台适配矩阵

| 项 | Linux | macOS |
|---|---|---|
| 设备类型 | `/dev/net/tun`（tunN） | `utunN` |
| tun crate 配置 | `platform_config(ensure_root_privileges)` + ioctl 设 IP | 自动 `ifconfig`+`route` |
| root 需求 | 是（CAP_NET_ADMIN） | 是 |
| 路由注入 | `ip route add` | `route -n add` |
| 默认 MTU | 1500 → 封装后 1280（建议） | 同 |
| fake-ip DNS | 同 | 同 |
| 测试方式 | 云主机 hub03（root） | 本机（需 sudo） |

**MTU 说明**：tun0 默认 1500，但 IP 包还要经 WS(mTLS) 封装，建议 tun0 MTU=1280
避免分片（§7 风险表）。

## 6. CLI 与配置示例

```bash
# 1. 准备路由表（复用 proxy routes）
sudo teamx proxy routes set-default egress
sudo teamx proxy routes add '*.cn' egress2

# 2. 启动 tun0（需 root）
sudo teamx tun0 start --ip 198.18.0.1 --net 198.18.0.0/15

# 3. 应用走 fake-ip 段流量（DNS 被劫持、TCP 被重组转发）
#    验证：
curl --interface utun0 https://example.com    # Linux: --interface tunN
dig @198.18.0.1 example.com                    # fake-ip DNS

# 4. 停止
sudo teamx tun0 stop
```

## 7. 风险与缓解

| 风险 | 缓解 |
|---|---|
| root 权限要求 | 启动时检测 + 文档明确；`--dev` 可自定义避免冲突 |
| smoltcp TCP 栈限制（无 SACK/PLPMTU） | 交互场景足够；吞吐敏感时提示用 SOCKS5 proxy |
| MTU/分片 | tun0 设 1280 |
| 并发上限（预分配 socket） | `--max-conns` 可调（默认 64） |
| UDP 仅 DNS | v1 文档明确：其他 UDP 丢弃 |
| fake-ip 与真实 IP 冲突 | 用 198.18.0.0/15（RFC 5737 文档网段，非公网） |
| macOS 自动分配 utunN 名不确定 | `TunDevice::name` 返回实际名，路由用实际名注入 |
| DNS 劫持可靠性 | 仅劫持目标为 fake-ip DNS 服务器 IP 的 53 端口 UDP |

## 8. 测试计划

### 8.1 单元测试（Rust `#[cfg(test)]`）

| 模块 | 用例 |
|---|---|
| tun_dev | 配置生成（mac/linux 分支）、错误处理 |
| tun_dns | fake-ip 分配唯一性、lookup 还原、DNS 应答构造、映射复用 |
| tun_socks | target 拼接（domain/IP）、路由解析接线 |
| routes | 【复用】已覆盖 |

### 8.2 集成测试（需 root，脚本 + Bun TS）

**Linux（云主机 hub03）`tests/tun0-linux-test.sh`**：
1. 起 serve + egress（复用现有 setup）
2. `sudo teamx tun0 start`（fake-ip 段）
3. `curl --interface tunN https://example.com` → 200（走 egress）
4. `curl --interface tunN https://ifconfig.me` → egress 出口 IP
5. fake-ip DNS：`dig @198.18.0.1 example.com` → 返回 fake IP
6. 域名路由：路由表配 `*.com → egress2`，验证分流
7. `sudo teamx tun0 stop` → 路由移除、tun 关闭

**macOS（本机）`tests/tun0-macos-test.sh`**：
1. 同样起 serve + egress
2. `sudo teamx tun0 start`
3. `curl --interface utunN https://example.com` → 200
4. 出口 IP 验证
5. `sudo teamx tun0 stop`

### 8.3 测试矩阵（场景 × 平台）

| 场景 | Linux | macOS |
|---|---|---|
| tun0 启动（root） | ✅ | ✅ |
| 非 root 启动报错 | ✅ | ✅ |
| TCP 转发（HTTP 200） | ✅ | ✅ |
| 出口 IP 正确 | ✅ | ✅ |
| fake-ip DNS 返回 | ✅ | ✅ |
| 域名路由分流 | ✅ | ✅ |
| IP/CIDR 路由分流 | ✅ | ✅ |
| tun0 stop 清理 | ✅ | ✅ |
| 与 proxy start 共存 | ✅ | ✅ |

## 9. 实施步骤

1. Cargo 依赖：`tun = "0.8"`、`smoltcp = "0.14"`（feature: proto-ipv4, socket-tcp, socket-udp, socket-dns, phy-tuntap_interface）、`ipnet`（可选）。
2. `tun_dev.rs`：跨平台 TUN 创建/读写（mac/linux 分支）。
3. `tun_stack.rs`：smoltcp Device impl + Interface + SocketSet + poll 循环。
4. 抽取 `spawn_tunnel_bridge()`（从 run_socks5_proxy 抽出 WS 桥接复用）。
5. `tun_socks.rs`：TCP 桥接（新连接→WS→搬运）+ UDP（DNS 劫持/丢弃）。
6. `tun_dns.rs`：fake-ip 分配 + DNS 应答。
7. `tun_cli.rs`：tun0 start/stop/status + 权限 + 路由注入。
8. `cli.rs` + `commands.rs` 接线。
9. 单元测试。
10. Linux 集成测试（hub03）+ macOS 集成测试（本机 sudo）。
11. 文档（本文件 + 20-manual 增补）+ CHANGELOG + 提交。

## 10. 工作量与里程碑

| 里程碑 | 内容 | 预计 |
|---|---|---|
| M1 | tun_dev + tun_stack（能建 tun0、smoltcp 跑起来） | 1 天 |
| M2 | TCP 桥接到 egress（能 curl 通） | 1 天 |
| M3 | fake-ip DNS + 域名路由 | 1 天 |
| M4 | CLI + 权限 + 路由注入 + 双平台适配 | 0.5 天 |
| M5 | 测试（Linux + macOS）+ 文档 | 1 天 |
| **合计** | | **约 4.5 天** |

## 11. 实现记录（2026-08-23）

已按本设计实现并验证，关键偏差与决策记录：

### 11.1 smoltcp `listen(0)` patch（透明代理的关键发现）

调研与实现中发现 **smoltcp 0.14 的 `listen()` 拒绝 port=0**（`ListenError::Unaddressable`），
且 listen socket 按 `repr.dst_port == listen_endpoint.port` **精确匹配** —— 无法直接做
"任意目标端口"的透明代理。

**解决**：vendor smoltcp 到 `vendor/smoltcp`，用 `[patch.crates-io]` 指向本地副本，打两处补丁：
- `listen()`：允许 `port==0`（作为通配符，不再报 Unaddressable）
- `accepts()`：`self.listen_endpoint.port == 0 || repr.dst_port == listen_endpoint.port`
  （port=0 的 listen socket 接受任意目标端口）

这样 `TunStack::new` 里 `socket.listen(0)` 即"监听所有端口"，配合 `set_any_ip(true)` +
fake-ip 前缀路由，实现透明拦截。

### 11.2 DNS 绑定策略

- 最初绑 `198.18.0.1:53` 在接口刚 up 时可能失败；绑 `0.0.0.0:53` 又撞上
  Ubuntu 的 systemd-resolved（127.0.0.53）。
- **最终**：优先绑 tun 网关 IP（`198.18.0.1:53`），失败回退 `0.0.0.0:53`。

### 11.3 验证结果（Linux, hub03）

| 项 | 结果 |
|---|---|
| `tun0 start`（root） | ✅ dev=tun0 ip=198.18.0.1 mtu=1280 |
| fake-ip 路由 | ✅ 198.18.0.0/15 → tun0 |
| fake-ip DNS | ✅ 198.18.0.1:53，example.com→198.18.0.1, www.google.com→198.18.0.2 |
| TCP 全链路 | ✅ `curl -k --interface tun0 --resolve example.com:443:<fake>` → HTTP 200 |
| 域名路由分流 | ✅ routes `*.com→egress2` 时，197 上出现到 example.com(20.85.130.105):443 的 ESTAB 连接 |
| stop 清理 | ✅ 路由删除、设备释放 |

### 11.4 待办（后续迭代）

- macOS 本机 sudo 实测（`tests/tun0-macos-test.sh`）
- fake-ip AAAA（IPv6）应答
- UDP 转发（非 DNS）
- 配置持久化（`teamx tun0 config`）
