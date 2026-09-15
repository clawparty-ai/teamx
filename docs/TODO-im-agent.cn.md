# TODO — IM agent 接入（飞书 ↔ 云端 member 对话）

> 状态：方案 1 已实施（单人 sidecar）；方案 2 待设计（多成员）。

## 背景

目标：飞书 chat app 里与 bot 的**任意对话**，都由用户的云端 teamx member（opencode agent）回复，而不只是 teamx 的固定命令。

## 方案 1：单人 sidecar（已实施）

```
新主机（AWS 东京 35.77.5.99，用户 caishu）
  ┌──────────────────────────────────────────┐
  │  opencode serve :42630                   │  caishu 的 member agent
  │      ▲ HTTP 127.0.0.1（同机）             │
  │  teamx im-gateway run --agent <url>      │  飞书长连接 + 路由
  └──────┬───────────────────────────────────┘
         │ mTLS RPC（审计 + 命令快路径）
         ▼
  主云主机（香港 120.27.222.33）teamx serve :5783
```

- gateway 与 agent **同机直连**（`127.0.0.1`），零网络开销、无 NAT 问题
- 固定命令（状态/任务/目标/讨论/问题/绑定）走 serve 快路径
- 其他任意文本 → 转发 opencode → AI 回复 → 推送飞书
- 双向落台账审计（`im.chat.in` / `im.chat.out`）
- 会话：`POST /session` 建一次，`POST /session/{id}/message` 复用（上下文连续）

**约束**：飞书 app 的长连接是单逻辑连接，多客户端会随机分发事件。所以一个飞书 app 只能有一个 gateway。方案 1 因此只服务一个成员。

## 方案 2：中央路由 + 成员 sidecar（待设计）

当需要**多个成员**各自的 agent 都能通过 IM 对话时，方案 1 的单连接约束会成为瓶颈。

```
[成员 A sidecar] ──出站连接──┐
[成员 B sidecar] ──出站连接──┼─► [中央 gateway] ◄──► 飞书 app（唯一连接）
[成员 C sidecar] ──出站连接──┘         │
                                        └─ 按 im_bindings 路由到对应 sidecar
```

要点：
- **中央 gateway** 收敛为路由器（一个飞书 app 一个 gateway）
- **成员 sidecar** = opencode + outbound adapter，主动连出（NAT 友好）
- gateway 按 `im_bindings` 把消息路由到对应成员的 sidecar
- 可复用 teamx 现有 **tunnel** 机制做 outbound 注册/转发

待设计问题：
1. sidecar 注册协议（身份、心跳、重连）
2. 路由表（`(platform, im_user_id, team) → sidecar`）
3. sidecar 不在线时的降级（排队 / 提示 / 转台账唤醒）
4. 多 sidecar 与飞书 app 的配额/限速
5. 与 teamx tunnel 的关系（复用还是独立通道）

## 相关代码

| 文件 | 说明 |
|---|---|
| `crates/teamx/src/im/ws_gateway.rs` | 持久连接网关；`--agent` 分流（方案 1） |
| `crates/teamx/src/im/agent.rs` | opencode HTTP 客户端 + 会话管理（方案 1） |
| `crates/teamx/src/serve.rs` | `im.chat.log` 审计 RPC（服务白名单） |
| `crates/teamx/src/im/mod.rs` | `im.*` RPC 路由 |

## 部署记录

| 主机 | 角色 | 用户 |
|---|---|---|
| 120.27.222.33（香港） | teamx serve（治理） | root |
| 35.77.5.99（AWS 东京） | opencode agent + im-gateway（sidecar） | caishu |
