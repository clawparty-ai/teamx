# Proxy（SOCKS5 出站代理）设计文档

## 1. 背景与目标

tunnel 解决的是"入站暴露"：member-b 开放一个本地端口，member-a 通过 tunnel
把它映射到本地访问，目标是**固定端口**。

proxy 解决的是"出站代理"：member-a 本地启动一个 **SOCKS5 代理端口**，
member-a 上的应用（curl、firefox 等）配置该代理后，流量经过
`member-a --ws--> team server --ws--> member-b`，由 **member-b 动态拨号**
到 SOCKS5 请求指定的目标地址，实现"借 member-b 的网络出口访问任意目标"。

核心差异：

| 维度 | tunnel (local forward) | proxy |
|------|------------------------|-------|
| 入口 | member-a 本地 TCP 端口（透传字节） | member-a 本地 **SOCKS5** 端口（解析目标地址） |
| 目标 | 固定（provider 注册时指定 target_port） | **动态**（来自 SOCKS5 CONNECT 请求的 host:port） |
| 出口 | member-b 拨号固定 target_port | member-b 拨号 SOCKS5 请求中的目标地址 |
| 用途 | 访问 member-b 的本地服务 | 借 member-b 的网络出口访问任意目标 |

## 2. 总体架构

```
+--------+   SOCKS5   +----------+   mTLS WS    +-----------+   mTLS WS    +----------+
|  curl/ | --1080-->  | member-a | --connect--> | teamx     | --open_    | member-b |
| firefox|            | (proxy   |  target=H:P | server    |   stream   | (exit)   |
|        |            |  start)  | <----------- | (relay)   | <----------|          |
+--------+            +----------+   [4B sid]   +-----------+   [4B sid]  +----------+
                                                               |
                                                               | dial H:P
                                                               v
                                                        +---------------+
                                                        | 目标服务 H:P  |
                                                        +---------------+
```

数据面完全复用 tunnel 的 stream 中继机制（`[4-byte BE stream_id][payload]`
二进制帧 + 每 WS 一条 stream 的双向桥接），**控制面增加目标地址传递**：

- consumer（member-a）→ server：`{"type":"connect","name":"<exit>","target":"host:port"}`
- server → provider（member-b）：`{"type":"open_stream","stream_id":N,"target":"host:port"}`

provider 收到带 `target` 的 open_stream 时，**拨号 target**（而非注册时固定端口）。

## 3. 协议设计

### 3.1 扩展 TunnelMode

`tunnel.rs` 的 `TunnelMode` 增加变体：

```rust
pub enum TunnelMode {
    Local,   // 默认：服务器不绑定端口，consumer 用 forward 访问
    Frp,     // 服务器绑定公共端口
    Proxy,   // 新增：出站代理出口，provider 动态拨号 SOCKS5 目标
}
```

- `--mode proxy`（或 `proxy` 子命令）注册代理出口。
- Proxy 模式：`target_port = 0`（无固定目标），不绑定服务器端口（同 Local）。

### 3.2 注册（provider → server）

```json
{"type":"register","name":"egress","port":0,"mode":"proxy"}
```

- 服务器校验：proxy 模式允许 `port == 0`（Local/Frp 仍要求 port != 0）。
- ack 与现有一致：`{"type":"registered","name":"egress","port":0,"mode":"proxy"}`。

### 3.3 连接（consumer → server）

```json
{"type":"connect","name":"egress","target":"example.com:80"}
```

- `target` 为 SOCKS5 CONNECT 请求解析出的 `host:port`。
- 兼容：无 `target` 时按现有行为（拨号注册时固定端口）。

### 3.4 打开流（server → provider）

```json
{"type":"open_stream","stream_id":12,"target":"example.com:80"}
```

- 带 `target`：provider 拨号该地址。
- 不带 `target`：provider 拨号注册时固定端口（兼容 tunnel forward）。

### 3.5 数据面

不变：`[4B BE stream_id][payload]` 双向二进制帧，服务器按 stream_id 路由。

## 4. 成员侧实现

### 4.1 member-b：代理出口（provider / exit）

CLI：`teamx proxy exit --name egress`

复用 `tunnel_client::run_expose` 的 WS 循环，修改 open_stream 处理：

```text
收到 open_stream（带 target）→ TcpStream::connect(target)
                     （不带 target）→ TcpStream::connect(127.0.0.1:固定端口)
```

### 4.2 member-a：本地 SOCKS5 代理（consumer）

CLI：`teamx proxy start --port 1080 --exit egress`

新模块 `socks5.rs`，职责：

1. 监听 `127.0.0.1:PORT`，接受应用连接。
2. SOCKS5 握手（NO AUTH）：
   - 读 `VER NMETHODS METHODS...`（VER=0x05）
   - 回复 `05 00`（选择无认证）
3. 解析 CONNECT 请求：`VER CMD RSV ATYP ADDR PORT`
   - `ATYP=0x01` IPv4（4 字节）
   - `ATYP=0x03` 域名（1 字节长度 + 名称）
   - `ATYP=0x04` IPv6（16 字节）
   - `CMD=0x01` CONNECT；其他 CMD 回复不支持
4. 连接 `wss://server/tunnel/forward`，发送 `{"type":"connect","name":"egress","target":"host:port"}`
5. 收到 `stream_open` 后回复 SOCKS5 成功 `05 00 00 01 00 00 00 00 00 00`
6. 之后按 tunnel forward 方式桥接字节（consumer 侧逻辑完全复用）

### 4.3 SOCKS5 协议解析（纯函数，可单测）

`socks5.rs` 提供无副作用解析函数：

```rust
/// 解析 SOCKS5 握手: 输入前 2 字节 + methods，返回选中的认证方法
pub fn parse_greeting(buf: &[u8]) -> Result<u8, String>;   // 返回 0x00 = NO AUTH

/// 解析 CONNECT 请求，返回 (atyp, host, port)
pub struct SocksTarget { pub host: String, pub port: u16 }
pub fn parse_connect_request(buf: &[u8]) -> Result<(usize, SocksTarget), String>;
//                                        ^ consumed 字节数
```

## 5. 服务器改动（serve.rs / tunnel.rs）

| 位置 | 改动 |
|------|------|
| `tunnel.rs` `TunnelMode` | 增加 `Proxy` 变体，`parse` 支持 `"proxy"`，`as_str` 返回 `"proxy"` |
| `tunnel.rs` `register` | Proxy 模式允许 `target_port == 0`（不校验非零） |
| `tunnel.rs` `open_stream` | 增加可选 `target: Option<String>` 参数，透传到 provider 的 `open_stream` 消息 |
| `serve.rs` `handle_tunnel_ws` | register 分支：Proxy 模式不要求 port；透传 target |
| `serve.rs` `handle_tunnel_forward` | connect 分支：解析可选 `target` 字段，传给 `open_stream` |

## 6. CLI 设计

```
teamx proxy exit    --name egress [--server URL]     # member-b: 出站代理出口（长驻）
teamx proxy start   --port 1080 --exit egress [--server URL]  # member-a: 本地 SOCKS5（长驻）
```

- `exit`：provider 侧，长驻 WS 循环（复用 tunnel_client::run_expose，mode=proxy）。
- `start`：consumer 侧，长驻 SOCKS5 监听（新 run_socks5_proxy）。
- 无 `--server` 时按现有规则解析（flag > env > letter > localhost）。

## 7. 安全边界

- 复用现有 mTLS：成员必须有本团队有效证书才能注册/连接。
- proxy exit 只能被**本团队**成员使用（服务器按 team_id 校验，同 tunnel）。
- SOCKS5 只监听 `127.0.0.1`，不暴露到局域网。
- 仅支持 CONNECT（HTTP/HTTPS/任意 TCP），不支持 UDP ASSOCIATE（v1 范围外）。
- 目标地址由 member-b 解析拨号；member-a 可请求任意 host:port（与 tunnel
  相同信任模型：团队成员互相开放）。

## 8. 里程碑

| 步骤 | 内容 | 验证 |
|------|------|------|
| P1 | socks5.rs：SOCKS5 握手 + CONNECT 解析（纯函数） | 单元测试 |
| P2 | tunnel.rs：TunnelMode::Proxy + register 放宽 + open_stream 透传 target | 单元测试 |
| P3 | serve.rs：register/connect 支持 proxy + target | 构建 + 集成测试 |
| P4 | tunnel_client.rs：run_expose 支持 target 拨号 + run_socks5_proxy | 集成测试 |
| P5 | cli.rs + commands.rs + main.rs：proxy exit/start 命令 | 端到端 curl --socks5 |

## 9. 非目标（v1 范围外）

- UDP ASSOCIATE（SOCKS5 UDP 中继）
- 认证（用户名/密码）——只做 NO AUTH
- 域名解析本地化（member-a 侧 DNS）——目标域名由 member-b 侧解析
- proxy 出口的访问控制/白名单