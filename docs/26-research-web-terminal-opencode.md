# Web Terminal 查看 Member opencode — 调研报告

## 1. 需求

在 **admin web console**（`teamx ui`）中嵌入 **web terminal**，通过 WebSocket 实时使用/查看 member 的 opencode。

## 2. 关键结论（可行性）

**✅ 可行，而且 opencode 原生就支持所需的一切。**

调研发现两个关键事实，让这个需求从"自研协议"变成"桥接现有能力"：

1. **opencode 有 headless server 模式**（`opencode serve`），提供 HTTP API + WebSocket，原生支持 **PTY（伪终端）**。
2. **teamx 已有 reverse tunnel 机制**（member 机器 ↔ teamx server 的字节桥接），可以复用把 opencode server 的流量引到 web console。

### 2.1 opencode serve 的能力（已验证）

`opencode serve --port <p> --hostname <h>` 启动 headless server（当前机器上实测成功）。它暴露了一整套 API：

**查看 member 会话（实时监控）**
| 端点 | 用途 |
|------|------|
| `GET /api/session` | 列出所有会话（title/agent/model/tokens/时间） |
| `GET /api/session/{id}/event` | 会话事件流（SSE，实时） |
| `GET /api/session/{id}/message` | 会话消息列表 |
| `GET /api/session/{id}/message/{mid}/part/{pid}` | 单条消息的部分（文本/工具调用/代码） |
| `GET /api/session/{id}/history` | 会话完整历史 |
| `GET /api/session/active` | 当前活跃会话 |
| `GET /event` | 全局事件流（SSE） |

**操作会话（控制）**
| 端点 | 用途 |
|------|------|
| `POST /api/session/{id}/prompt` | 向会话发消息 |
| `POST /api/session/{id}/interrupt` | 中断 |
| `POST /api/session/{id}/question/{requestID}/reply` | 回答会话问题 |
| `POST /api/session/{id}/permission/{requestID}/reply` | 授予/拒绝工具权限 |
| `GET /api/session/{id}/todo` | 查看 todo |

**Web Terminal（PTY）**
| 端点 | 用途 |
|------|------|
| `GET /pty/shells` | 列出可用 shell |
| `POST /pty` | 创建 PTY（body: `{command, args, cwd, env, title}`） |
| `GET /api/pty/{ptyID}/connect-token` | 获取连接 token |
| `GET /api/pty/{ptyID}/connect` | **WebSocket 升级**，实时终端 I/O |
| `GET /api/pty` / `PUT|DELETE /api/pty/{ptyID}` | 管理 PTY |

> 注意：opencode 的 PTY 是"新建一个 shell 进程"，不是 attach 到已运行的 TUI。查看 member 正在运行的 opencode TUI 屏幕流不在 API 范围内，但**查看/操作会话、以及新建终端**完全支持。

### 2.2 teamx 现有 reverse tunnel（已验证）

- member 机器跑 `teamx tunnel expose --name <n> --port <p>` → 通过 mTLS WS 注册到 server，保持长连接。
- admin 侧 `teamx tunnel forward <n>` → server 桥接字节流，本地得到一个 TCP 端口访问 member 的服务。
- 这是**通用的 TCP 字节桥接**（多路复用 WS），可直接用于转发 member 的 `opencode serve` 端口。

## 3. 架构方案

### 3.1 方案 A（推荐）：Member 跑 `opencode serve` + Tunnel + Web console 代理

```
┌─────────────────────────────────────────────────────────────────┐
│  Admin web console (teamx ui, owner 机器)                       │
│                                                                 │
│  ┌─────────────┐   ┌──────────────────────────────────┐        │
│  │ xterm.js    │──▶│ teamx ui /pty proxy (Rust axum)  │        │
│  │ (web终端UI) │   │  - 认证(token)                    │        │
│  └─────────────┘   │  - 打开到 member 的 tunnel 连接   │        │
│                    │  - 桥接 WS↔WS                     │        │
│                    └──────────────────────────────────┘        │
└───────────────────────────────┬─────────────────────────────────┘
                                │  mTLS /tunnel/forward (WS)
                                │
┌───────────────────────────────▼─────────────────────────────────┐
│  teamx server (中央 server, owner 机器)                        │
│  - tunnel registry: 记录 member 暴露的 opencode port            │
│  - 字节中继: provider WS ↔ consumer WS                          │
└───────────────────────────────┬─────────────────────────────────┘
                                │  mTLS /tunnel (WS, provider)
┌───────────────────────────────▼─────────────────────────────────┐
│  Member 机器                                                     │
│  ┌──────────────────────┐    ┌─────────────────────────────┐   │
│  │ teamx tunnel expose  │    │ opencode serve --port 4096  │   │
│  │ (注册到 teamx server) │◀──▶│ (headless, HTTP+WS+PTY)     │   │
│  └──────────────────────┘    └─────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
```

**Member 侧**（每个 member 机器）：
```bash
opencode serve --hostname 127.0.0.1 --port 4096 &   # headless opencode
teamx tunnel expose --name opencode-<member> --port 4096   # 注册隧道
```

**Admin web console 侧**：
1. `teamx ui` 增加 `/terminal` 页面（xterm.js）。
2. 页面选择 member → 通过 `teamx ui` 的 Rust 代理 `GET /proxy/pty/{member}`。
3. Rust 代理：认证 → `tunnel.open_stream` 连到 member 的 tunnel → 与浏览器 WS 双向桥接字节。
4. 浏览器 xterm.js 把字节渲染为终端。

**两种 PTY 使用模式：**
- **查看 opencode 会话**：代理把 opencode 的 `/api/session/{id}/event`（SSE）转为 WS 推给页面，页面渲染聊天流（不需要 PTY，是结构化数据）。
- **终端操作**：代理调用 opencode `/pty` 创建终端 + `/api/pty/{id}/connect` WS，转发到 member 的 shell（等同于在 member 机器开终端）。

### 3.2 方案 B：tunnel forward（本地端口）+ 独立 web

不修改 `teamx ui`，admin 手动 `teamx tunnel forward opencode-<member>` 得到本地端口，再浏览器开 `http://127.0.0.1:<port>`（opencode web UI）。缺点：需要手动端口、无统一管理界面。

### 3.3 方案 C（轻量只读监控）：不走 PTY，直接事件流

只做"查看 member 正在做什么"：代理拉取 member opencode 的 `/event` SSE → 推给 web console 实时渲染（member 的当前 session、正在调用的工具、消息流）。实现最简单，不需要 PTY。可作为 MVP。

## 4. 安全考量

| 风险 | 缓解 |
|------|------|
| 任意 member 的 opencode 被看 | 只有 owner（team lead）能进 web console；tunnel 授权沿用 teamx 权限模型 |
| member 机器上的 shell 被远程操作 | PTY 是双向的，等于远程 shell——**默认只读/仅 owner 可启 PTY**；或先做只读会话查看（方案 C） |
| opencode server 无密码 | `opencode serve` 支持 `OPENCODE_SERVER_PASSWORD`，member 侧应设置；tunnel 本身已 mTLS |
| 隧道暴露面 | tunnel 只允许 team 成员连接，owner 通过 web console 认证后才能发起 |

## 5. 实现工作量估算

| 阶段 | 内容 | 估算 |
|------|------|------|
| MVP（方案 C） | web console 加"member 会话监控"页：代理 SSE→WS + 渲染 | 2-4 人天 |
| P1（方案 A 只读） | member `opencode serve` + tunnel 注册 + 会话事件流桥接 | 3-5 人天 |
| P2（方案 A 全功能） | PTY 桥接 + xterm.js + 终端交互 | 4-7 人天 |
| P3 | 权限细化（只读/可写）、审计日志、多 member 同时在线 | 3-5 人天 |

## 6. 建议

1. **先做 MVP（方案 C）**：在 `teamx ui` 里加"成员会话"面板，实时显示每个 member 正在跑的 opencode 会话/工具调用/消息。验证整个隧道链路（member opencode serve → tunnel → web console）可行。
2. **再做 P2 终端**：确认需要交互后，加 xterm.js + PTY 桥接，但**默认只读，显式开启写**。
3. 安全上，PTY 远程 shell 能力要谨慎，建议 owner-only 且留审计。

## 7. 需要验证的遗留问题

- opencode PTY `/connect` 的 WS 协议帧格式（client→server 的输入编码）。
- opencode `/event` SSE 的事件 payload 结构（已在 JS bundle 中看到 `session.updated` / `message.updated` 等类型）。
- teamx tunnel 的 WS 桥接是否能透传 opcode/binary 帧（PTY 需要 binary 帧）。
- opencode serve 跨机器访问时的 CORS/来源校验（`--cors` 参数）。
