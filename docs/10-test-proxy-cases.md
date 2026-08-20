# Proxy（SOCKS5 出站代理）测试案例

编号规则：`PR-<模块>-<序号>`。PR=SOCKS5 协议，PE=exit 出口，PC=consumer 代理，PI=集成。

## 单元测试（cargo test）

### PR-001 SOCKS5 握手：NO AUTH 协商
- 输入：`05 01 00`（VER=5, 1 方法, 无认证）
- 期望：返回 `0x00`（NO AUTH）

### PR-002 SOCKS5 握手：多个方法选 NO AUTH
- 输入：`05 03 00 01 02`
- 期望：返回 `0x00`

### PR-003 SOCKS5 握手：客户端要求认证方法
- 输入：`05 01 02`
- 期望：错误（v1 不支持认证）

### PR-004 SOCKS5 CONNECT：IPv4 目标
- 输入：`05 01 00 01 7F 00 00 01 1F 90`（CONNECT, IPv4 127.0.0.1:8080）
- 期望：`host=127.0.0.1, port=8080`，consumed=10

### PR-005 SOCKS5 CONNECT：域名目标
- 输入：`05 01 00 03 09 65 78 61 6d 70 6c 65 2e 63 6f 6d 00 50`（example.com:80）
- 期望：`host=example.com, port=80`，consumed=6+11+2=19

### PR-006 SOCKS5 CONNECT：IPv6 目标
- 输入：`05 01 00 04 <16B addr> 00 50`
- 期望：host 为 IPv6 字符串，port=80

### PR-007 SOCKS5 CONNECT：非 CONNECT 命令拒绝
- 输入：`05 02 00 01 7F 00 00 01 00 50`（BIND）
- 期望：错误

### PR-008 SOCKS5 CONNECT：残缺输入
- 输入：`05 01 00 01 7F`（不足）
- 期望：错误（或返回需要更多字节）

### PE-001 TunnelMode::Proxy 解析
- `TunnelMode::parse("proxy")` == Proxy；`as_str()` == "proxy"

### PE-002 Proxy 模式注册允许 port=0
- `register(team, member, "egress", 0, None, tx, Proxy)` 成功，port 返回 0

### PE-003 Proxy 模式不绑定服务器端口
- Local/Proxy 注册后 `port == 0`；list 报告 mode=proxy

### PE-004 open_stream 透传 target
- `open_stream(team, name, tx, Some("example.com:80"))` → provider 收到
  `{"type":"open_stream","stream_id":N,"target":"example.com:80"}`

### PE-005 open_stream 无 target 兼容
- `open_stream(team, name, tx, None)` → provider 收到 open_stream（无 target 字段）

### PE-006 重复注册同名 proxy exit 被拒
- 同 team 同名两次注册 → 第二次 Err

## 集成测试（bun tests/proxy-test.ts）

### PI-001 exit 注册（mode=proxy）
- 成员 B 连接 `/tunnel`，register `{"name":"egress","port":0,"mode":"proxy"}`
- 期望：收到 `registered`，port=0，mode=proxy

### PI-002 SOCKS5 端到端：curl 经代理访问 member-b 出口服务
- member-a 启动 `teamx proxy start --port 1080 --exit egress`
- member-b 本地 HTTP 服务 `127.0.0.1:19099`（返回固定 body）
- `curl --socks5-hostname 127.0.0.1:1080 http://127.0.0.1:19099/`
- 期望：返回 member-b 服务的 body（字节经 a→server→b 往返一致）

### PI-003 多路并发 SOCKS5 连接
- 同时发起 3 个 curl（不同 stream）
- 期望：3 个都成功，stream_id 互不冲突

### PI-004 tunnel.list 报告 proxy exit
- `tunnel.list` 返回 mode=proxy 的 egress

### PI-005 非团队成员无法使用 exit
- 无证书/无成员身份连接 `/tunnel/forward` 请求 egress → 被拒

### PI-006 断开清理
- exit WS 断开 → tunnel.list 不再包含 egress

## 手工验收（可选）

### PI-H1 Firefox/Chrome SOCKS5 配置
- 浏览器配置 SOCKS5 127.0.0.1:1080 → 能访问 member-b 侧可达的目标

### PI-H2 域名解析在 member-b 侧
- curl --socks5-hostname 访问仅 member-b 能解析的域名 → 成功