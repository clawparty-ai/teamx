# member 间直连数据通道（direct）设计（P0）

> 状态：P0 已实现（候选登记 + 探测）。讨论线程：`dsc_18d4ed1b2b1d95c81c0923`。

## 背景
两个 teamx member 现通过 `teamx tunnel` 经 **server 桥接**互通（数据全走 server）。
`tunnel.status` 已有同网段直连提示（`same_subnet(peer_ip, lan_ip)` → `direct_addr`），
但只是只读提示：无探测、无 NAT 穿透、无独立数据通道抽象。

## 定位
- **控制面（server）**：候选登记、鉴权（mTLS）、台账、rendezvous。
- **数据面（可直连）**：大流量走 member↔member 直连；不可达则回退现有 relay。

## P0（本 PR）
- **候选登记**：`direct.advertise { lan_ips, listener_port, session, team }` → server 存
  `direct_endpoints(member_id, team_id, lan_ips, reflexive_ip, reflexive_port, listener_port, updated_at)`；
  `reflexive_ip:port` 由 server 从连接的对端地址（STUN-like）记录。
- **候选查询**：`direct.candidates { member_id }` → 仅同 team 可见；返回候选地址列表
  （LAN 优先，其后 reflexive）。
- **探测**：`direct.probe { member_id, timeout_ms }` → 对候选依次短超时 TCP 连接，返回
  `reachable` 与每条候选的 `rtt_ms`/`error`。
- **台账**：`direct.advertised`。
- **CLI**：`teamx direct advertise --lan-ip <ip> --port <p>` / `candidates <member>` / `probe <member>`。

## P0.5 + P1（已实现：配对票据 + 限速 + listener/ping）

讨论线程（Q1-Q5 收敛）定下的安全与端口口径落地：

- **配对票据（P0.5）**：`direct.probe` 探测可达即签发**一次性配对票据**（两个 uuid v4
  拼接，TTL 60s，单次消费）；provider 在直连 TLS 握手时经 `direct.ticket.check` RPC
  校验（绑定 team + provider=caller，过期/已消费/队不匹配全部拒绝并落台账）。
  **知道 IP 不再能完成直连会话**。
- **probe 限速（P0.5）**：每 (member, peer) 对 60s 滑动窗口最多 10 次探测；超限拒绝并落
  `direct.rejected`（对齐 `im.rejected` 风格），收敛端口扫描面。
- **`teamx direct listen`（P1，provider）**：rustls mTLS listener（成员证书为身份、
  团队 CA 为客户端信任锚），随机端口 + 自动探测主 LAN IP（UDP-connect 路由法）并
  `direct.advertise` 登记；入站连接先 TLS 握手（证明同 team）→ 读票据行 → 服务端校验 →
  ping/pong 循环。Ctrl-C 停止。
- **`teamx direct ping`（P1，consumer）**：candidates → probe（拿票据）→ 连最优候选 →
  TLS 握手 → 呈递票据（`ok <requester>` 确认）→ N 次 ping/pong 测 RTT。
- **TLS 名字策略**：成员证书无 SAN，成员间端点没有主机名——consumer 侧用
  `CaChainVerifier`（链验证到团队 CA、**名字无关**，EKU=clientAuth）；身份绑定由
  一次性票据 + 服务端审计承担，禁 TOFU/自签。
- **db**：`direct_tickets` / `direct_probe_attempts` 收敛式建表（不占 user_version，
  与并行迁移轨零冲突）。
- **测试**：单测 4 条（票据发放/单次消费/未知票据与错误 caller/限速与配对隔离）+
  `tests/direct-listen-ping-test.sh`（真实 serve + 双成员 mTLS 闭环 + 限速负例 +
  `direct.rejected` 台账断言），注册 run-all 15i。

## 后续（未实现）
- P2：tunnel 数据面**优先直连 + 自动回退**（transport 抽象 + 迟滞切换；红线：relay 永远可用）；台账 `direct.established` / `direct.failed`。
- P3：QUIC（多路复用/0-RTT）、STUN/UPnP（默认 off）、质量上报。

## 安全
- 候选仅同 team 成员可查（`member_in_team` 校验）。
- 直连通道（P1）必须 mTLS（复用成员证书）+ 校验对端 member 身份。
- 控制面始终经 server；数据面直连不绕过鉴权。

## 开放问题（讨论中）
Q1 TCP vs QUIC；Q2 STUN/UPnP；Q3 与 tunnel 的关系；Q4 安全模型；Q5 端口/listener 复用。
