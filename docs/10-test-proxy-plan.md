# Proxy（SOCKS5 出站代理）测试计划

## 1. 范围

覆盖 `teamx proxy` 功能：

- `teamx proxy exit`（member-b 出站代理出口，provider 侧）
- `teamx proxy start`（member-a 本地 SOCKS5 代理，consumer 侧）
- SOCKS5 协议解析（握手 + CONNECT 请求）
- 端到端数据面（curl 经 SOCKS5 代理访问 member-b 网络出口）

## 2. 测试分层

| 层 | 工具 | 内容 |
|----|------|------|
| 单元测试 | `cargo test` | socks5.rs 解析、tunnel.rs Proxy 模式、open_stream target 透传 |
| 集成测试 | `bun tests/proxy-test.ts` | server + exit 成员 + proxy 成员 + curl --socks5 端到端 |
| 全量回归 | `./tests/run-all.sh` | 新增 proxy 步骤，确认 tunnel/ws/mtls 等不回归 |

## 3. 测试环境

- 单机闭环：`TEAMX_SERVER_URL=https://127.0.0.1:PORT`（serve 自带 mTLS）。
- 两个成员：owner（proxy 消费者）+ 成员 B（exit 提供者）。
- 目标服务：member-b 本地启动一个 HTTP 服务（如 `127.0.0.1:19099`），
  模拟"member-b 网络出口可达的服务"；member-a 通过 SOCKS5 访问它。
- 使用 `curl --socks5-hostname 127.0.0.1:1080 http://127.0.0.1:19099/`。
  - 域名解析发生在 member-b 侧（SOCKS5 把目标地址原样传给出口）。

## 4. 隔离与清理

- 每次测试用独立临时 `TEAMX_HOME` / `TEAMX_DB` / 端口（server、socks5、
  目标服务、exit）。
- `trap cleanup` 清理临时目录与进程。

## 5. 关键验证点（对照测试案例）

- SOCKS5 握手（NO AUTH）回复 `05 00`。
- CONNECT 请求：IPv4 / 域名 / IPv6 三类 ATYP 解析正确；非 CONNECT 命令拒绝。
- exit 注册 mode=proxy 成功，port=0，不绑定服务器端口。
- consumer 带 target 连接 → provider 收到 open_stream 带 target → 拨号目标。
- 数据面双向字节一致（member-a 应用收到 member-b 出口服务的响应）。
- `tunnel.list` 能看到 mode=proxy 的 exit。
- 成员 B 不在团队时连接被拒（mTLS 授权边界）。

## 6. 执行顺序

1. `cargo test`（P1/P2 单元）
2. `cargo clippy --all-targets -- -D warnings`
3. `bun tests/proxy-test.ts`（P3-P5 集成）
4. `./tests/run-all.sh`（全量回归）