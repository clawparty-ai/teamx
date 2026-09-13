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

## 后续（未实现）
- P1：provider `direct listen`（mTLS 直连 listener）+ consumer 直连握手 + `teamx direct ping`。
- P2：tunnel 数据面**优先直连 + 自动回退**；台账 `direct.established` / `direct.failed`。
- P3：QUIC（多路复用/0-RTT）、UPnP、质量上报。

## 安全
- 候选仅同 team 成员可查（`member_in_team` 校验）。
- 直连通道（P1）必须 mTLS（复用成员证书）+ 校验对端 member 身份。
- 控制面始终经 server；数据面直连不绕过鉴权。

## 开放问题（讨论中）
Q1 TCP vs QUIC；Q2 STUN/UPnP；Q3 与 tunnel 的关系；Q4 安全模型；Q5 端口/listener 复用。
