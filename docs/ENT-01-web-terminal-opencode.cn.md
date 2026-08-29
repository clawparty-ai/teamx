# ENT-01 — Admin Web Console 实时查看 Member opencode（Web Terminal + 会话监控）

> 状态：**DESIGN（设计定稿，待实施 MVP）**
> 分支约束：本文档为 **enterprise 专属**（`ENT-` 前缀），**不得合并到 main 分支**。
> 关联：`docs/26-research-web-terminal-opencode.md`（可行性调研）、
> `crates/teamx/src/ui.rs`（`teamx ui` dashboard）、`crates/teamx/src/serve.rs`（tunnel/WS）
> 日期：2026-08-30

## 1. 背景与目标

Admin（team lead）需要在 `teamx ui`（admin web console）中**实时查看**每个 member 正在运行的
opencode：在 Members 页面点击一个 member，即可看到该 member 的会话列表、实时活动
（工具调用/消息/文件编辑），并可（P2）通过 web terminal 实际操作 member 机器上的 shell。

**核心目标（MVP）**：Members 页面内嵌"成员会话监控"——只读实时查看 member 的 opencode 活动。

**P2 目标**：Web Terminal（xterm.js + PTY），通过 mTLS 隧道实时操作 member 的 shell。

## 2. 非目标

- ❌ 不做独立一级菜单（Sessions 取消，内嵌在 Members 页面）
- ❌ P1 不提供 PTY 写入；只读会话/活动流
- ❌ 不做成员间的实时协作（那是团队核心功能，不在此文档）

## 3. 架构总览

```
┌────────────────────────────────────────────────────────────┐
│  Admin web console (teamx ui，owner 机器)                  │
│                                                            │
│  Members 页面                                               │
│  ┌───────────────┐   ┌──────────────────────────────────┐  │
│  │ 左：成员列表   │   │ 右：会话详情（实时活动/消息/终端）│  │
│  │ 点击 member   │──▶│  - 实时活动流（SSE→WS 桥接）      │  │
│  └───────────────┘   │  - 会话列表 / 消息 / 终端         │  │
│                      └──────────────────────────────────┘  │
│  teamx ui 新增代理端点：/api/members/{id}/sessions 等       │
└──────────────────────────────┬─────────────────────────────┘
                               │ HTTP (同 ui 进程，token 认证)
┌──────────────────────────────▼─────────────────────────────┐
│  teamx serve（中央 server，owner 机器）                    │
│  - tunnel registry：member 暴露的 opencode 隧道            │
│  - 代理端点拉取 member 的 opencode HTTP/WS                 │
└──────────────────────────────┬─────────────────────────────┘
                               │ mTLS /tunnel (provider WS，字节中继)
┌──────────────────────────────▼─────────────────────────────┐
│  Member 机器                                                │
│  ┌─────────────────────┐   ┌────────────────────────────┐  │
│  │ teamx tunnel expose │   │ opencode serve --port N    │  │
│  │ name=opencode-<id>  │──▶│ (headless，HTTP+WS+PTY)    │  │
│  └─────────────────────┘   └────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
```

## 4. 关键前提（调研结论）

### 4.1 opencode serve 提供完整 API（已验证）

member 机器上运行 `opencode serve --hostname 127.0.0.1 --port <N>`（无 UI、无 headless TUI），
暴露：

| 类别 | 端点 | 用途 |
|------|------|------|
| 会话列表 | `GET /api/session` | 所有会话（title/agent/model/tokens/时间） |
| 会话详情 | `GET /api/session/{id}` | 单会话元数据 |
| 会话事件流 | `GET /api/session/{id}/event` | **SSE 实时事件流** |
| 消息 | `GET /api/session/{id}/message` | 消息列表 |
| 消息 part | `GET /api/session/{id}/message/{mid}/part/{pid}` | 单条消息的部分（文本/工具） |
| 历史 | `GET /api/session/{id}/history` | 完整历史 |
| 活跃会话 | `GET /api/session/active` | 当前活跃会话 |
| 全局事件 | `GET /event` | 全局事件流（SSE） |
| PTY 管理 | `POST /pty`, `GET /api/pty/{id}/connect-token` | 创建伪终端（P2） |
| PTY 连接 | `GET /api/pty/{id}/connect` | **WebSocket 终端 I/O（P2）** |
| shell 列表 | `GET /pty/shells` | 可用 shell（P2） |

### 4.2 teamx 已有 reverse tunnel（已验证）

member 机器 `teamx tunnel expose --name opencode-<member> --port N` 注册到 teamx server；
admin 侧通过 tunnel registry 打开到 member 的字节流连接。已支持本地/FRP/代理模式。

### 4.3 隧道是通用字节桥接

`/tunnel/forward` 消费者发送 `{"type":"connect","name":"<tunnel>"}`，server 在 registry
中找到 provider 并桥接字节。**HTTP 与 WS 都可透传**（均为 TCP 字节流）。

## 5. MVP 详细设计（P1：只读会话监控）

### 5.1 UI（Members 页面内嵌）

**菜单**：无新增一级菜单；`Members` 页面即"成员 + 实时会话"。

```
Members 页面布局：
┌─ 左栏（300px）────────────────┐ ┌─ 右栏（详情面板）──────────────────────┐
│ Members                       │ │                                       │
│ "点击成员查看其实时会话"       │ │  👈 未选择：空状态提示                  │
│                               │ │                                       │
│ [A] Alice  ● LIVE  (contributor)│ │  ┌ 成员chip + 会话标题 + 操作 ──────┐ │
│ [B] Bob    ● LIVE  (reviewer)  │ │  │ 实时活动 | 消息 | 终端            │ │
│ [C] Carol  ○      (observer)   │ │  │ 实时活动流（工具/文件高亮）        │ │
└───────────────────────────────┘ │  └────────────────────────────────────┘ │
                                   └────────────────────────────────────────┘
```

**交互**：
1. 点击成员 → 右侧显示该成员会话详情（实时活动流）
2. 在线成员卡片带 `LIVE` 呼吸标；离线成员点击后显示空状态
3. 右侧详情 tab：`实时活动 / 消息 / 终端`
4. `实时活动` tab：时间线流（时间 + 图标 + 内容；工具名主色、文件路径橙色等宽）

### 5.2 teamx ui 新增代理端点（owner 机器）

`teamx ui` 增加以下 API（在 ui.rs 中，auth 沿用 token/cookie）：

```
GET /api/members/{memberId}/sessions         → opencode /api/session 的代理（拉取会话列表）
GET /api/members/{memberId}/session/{sid}/event → 代理 SSE → WS/SSE 实时流
GET /api/members/{memberId}/session/{sid}/message → 代理消息列表
```

实现方式：ui.rs 的 handler 通过 `tunnel.open_stream(team, "opencode-<member>", ...)` 建立到
member 机器的字节流，然后作为 HTTP client 转发 opencode 的请求/响应。SSE 需逐块转发。

> **简化备选（MVP 首选）**：MVP 阶段先在 ui.rs 里维护一个"member→隧道名"映射表，
> 用现有 tunnel 客户端代码（`tunnel_client`）打开到 member 的 forward，在 ui 进程内
> 起一个本地转发端口（127.0.0.1:随机端口），ui 的 API 直接 HTTP 访问该本地端口。
> 该方案复用 `teamx tunnel forward` 的全部逻辑，MVP 改动最小。

### 5.3 member 侧配置（opencode serve 自动拉起）

member 机器需：
1. 运行 `opencode serve`（headless）——由 teamx 插件/命令自动拉起（新增 `teamx git`-like 命令，
   或 opencode-plugin 在成员会话启动时 spawn）。
2. `teamx tunnel expose --name opencode-<memberId> --port <opencode-port>` 注册隧道。

**隧道命名**：`opencode-<member_id>`（固定前缀，teamx ui 按 member 查询）。

### 5.4 数据流（实时活动）

```
浏览器 xterm.js? No — MVP 是活动流：
浏览器 JS  fetch GET /api/members/{id}/session/{sid}/event   (SSE)
    → ui.rs handler → tunnel open_stream("opencode-<id>")
    → member opencode /api/session/{sid}/event (SSE)
    → ui.rs 逐 chunk 转发回浏览器 → 渲染为活动行
```

### 5.5 安全

| 风险 | 缓解 |
|------|------|
| 非 owner 查看 | ui 进程 owner-only（现状已有）；新端点同样要求 token |
| 任意隧道被读 | ui 只解析 `opencode-<memberId>` 前缀且校验 member 属于当前 team |
| 隧道劫持 | tunnel 数据面沿用 mTLS + team 成员校验 |
| 密码 | member 侧 `opencode serve` 建议设 `OPENCODE_SERVER_PASSWORD`（P1 可选） |

## 6. P2 详细设计（Web Terminal）

### 6.1 UI

`终端` tab 内嵌 xterm.js（本地 npm 静态资源或 CDN）。终端窗口：
- 标题栏：`<member>@<node>: <cwd>`（macOS 风格红黄绿点）
- 输入 → WS → ui 代理 → tunnel → opencode `/api/pty/{id}/connect` WS → member shell
- 底部提示"该终端运行在 <member> 的机器，通过 mTLS 隧道实时连接"

### 6.2 流程

1. 浏览器请求 `POST /api/members/{id}/pty` → ui.rs 调 member opencode `POST /pty`
   （body: `{command, cwd, env}`）→ 得到 `ptyID`
2. 浏览器建立 WS → `ws://ui/ws/members/{id}/pty/{ptyID}`（ui 内置 WS 代理）
3. ui.rs 将 WS 字节桥到 member opencode `/api/pty/{ptyID}/connect`（WS）
4. member shell 输出经同一链路返回浏览器 → xterm.js 渲染

### 6.3 注意事项

- **双向**：PTY 写入即远程 shell 操作，**仅 owner 可开**，且需二次确认
- 审计：记录谁在何时打开了谁的终端（写入 activity/events）
- 帧协议：需确认 opencode PTY WS 的输入编码（UTF-8 文本 / base64 / binary）

## 7. 模块改动清单

| 文件 | 改动 |
|------|------|
| `crates/teamx/src/ui.rs` | 新增代理 API：`/api/members/{id}/sessions`、`session/{sid}/event`、`message`、`pty`、WS 代理 |
| `crates/teamx/src/ui.rs` | Members 页面 JS：成员列表 + 会话详情（实时活动流渲染） |
| `crates/teamx/src/ui.rs` | 增加 SSE→WS 转发 helper、tunnel→member 字节桥 helper |
| `crates/teamx/src/cli.rs` | （可选）`teamx member-serve` 命令：拉起 opencode serve + 注册隧道 |
| `opencode-plugin` | （可选）成员会话启动时自动拉起 opencode serve + tunnel expose |
| `docs/26-research-web-terminal-opencode.md` | 调研记录（enterprise 专属，不合并 main） |

## 8. 实施里程碑

| 阶段 | 内容 | 验收 |
|------|------|------|
| M1（MVP） | ui.rs Members 页面内嵌会话监控；`/api/members/{id}/sessions` + `session/{sid}/event` 代理（本地 forward 方案） | 点击 member 能看到其实时会话/活动流 |
| M2 | member 侧自动拉起 `opencode serve` + `tunnel expose`（命令/插件） | 一键部署 member 监控 |
| M3（P2） | Web Terminal：xterm.js + PTY WS 代理（owner-only + 审计） | 在浏览器操作 member shell |

## 9. 遗留问题（需在 M1 验证）

- [ ] opencode `/api/session/{id}/event` SSE 事件 payload 结构（session.updated / message.updated 等）
- [ ] tunnel WS 能否透传 SSE 逐块（HTTP chunked 是否被字节桥无损转发）
- [ ] ui.rs 本地 forward 方案与直接 tunnel.open_stream 方案的选择
- [ ] opencode serve 跨机器访问的 CORS/来源校验（`--cors`）
