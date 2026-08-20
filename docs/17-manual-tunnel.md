# teamx 反向隧道手动测试：member-a 经 server 访问 member-b 的本地服务

> 场景：**member-b（开发者）**在本机跑了一个服务（例如 HTTP 服务），通过 `teamx` 反向隧道把它暴露给团队；**member-a（测试员）**在另一台机器（或同一台机器另一个会话）通过网络访问这个服务——即使两边网络无法直达，也能经 `teamx serve` 中继；同网段时优先直连。
>
> **两种模式（expose 时选择）**：
> - **local（默认）**：server 不暴露任何端口；member-a 用 `/team tunnel forward` 在本地映射端口访问（更安全、SSH `-L` 体验）。
> - **frp**：server 暴露公开端口（tcp://server:9100），member-a 直接连接。
>
> 前置：网络模式可用（mTLS + `/team tunnel` 命令已安装），owner 已建队并 `serve start`，两个成员已通过 invitation letter 加入并被 approve。

## 0. 前置条件

1. `./install.sh` 已执行并重启 opencode（`/team tunnel` 子命令可用）。
2. 已有一个网络模式的团队（owner 已 `serve start`，成员已 import letter + approve，见 `docs/16-manual-network.md`）。
3. member-b 机器上有一个正在运行的本地服务（本示例用 HTTP 服务，隧道本身支持任意 TCP 协议：SSH / 数据库 / 自定义协议）。

## 1. 数据流

**frp 模式（`expose --mode frp`）**

```
┌─ member-b（开发者）───────┐       ┌──────────────┐       ┌─ member-a（测试员）─────┐
│ 本地服务 :8080             │       │ teamx serve  │       │ curl / 浏览器            │
│ /team tunnel expose        │──────▶│ 中继 :9100+   │◀──────│ tcp://<server>:9100      │
└────────────────────────────┘  WS   └──────────────┘  TCP  └──────────────────────────┘
```

**local 模式（默认，`expose` 不带 --mode）**

```
┌─ member-b（开发者）───────┐       ┌──────────────┐       ┌─ member-a（测试员）─────────┐
│ 本地服务 :8080             │       │ teamx serve  │       │ /team tunnel forward demo   │
│ /team tunnel expose        │──────▶│ 桥接（不暴露）│◀──────│ 本地监听 127.0.0.1:8080      │
└────────────────────────────┘  WS   └──────────────┘  WS   └───── curl http://127.0.0.1:8080/
```

- **member-b**：`/team tunnel expose` 打开一条持久 mTLS WebSocket 连到 serve 的 `/tunnel`，注册本地 `:8080`。
- **serve**：
  - frp 模式：分配一个公开端口（`9100-9999`），收到 member-a 的 TCP 连接后，通过该 WS 把字节中继给 member-b。
  - local 模式：不暴露任何端口；member-a 的 `forward` WS（`/tunnel/forward`）经 server 桥接到 member-b 的隧道 WS。
- **member-a**：frp 连公开端口；local 用 `/team tunnel forward` 在本地映射端口，访问 `http://127.0.0.1:<local>/` 即达 member-b 服务。
- **同网段直连**：`/team tunnel status <name>` 返回 `same_subnet`；若为 true，member-a 可直接访问 `direct_addr`（member-b 的 `lan_ip:target_port`）。

## 2. 手动测试步骤

### 2.1 member-b —— 准备本地服务

先在本机起一个简单的 HTTP 服务（示例用 Python）：

```bash
cd /tmp && mkdir -p svc && cd svc
echo "hello from member-b's service" > index.html
python3 -m http.server 8080 --bind 127.0.0.1
# 验证：curl http://127.0.0.1:8080/index.html → hello from member-b's service
```

### 2.2 member-b —— 暴露服务

在 member-b 的 opencode 窗口（**local 模式，默认**）：

```
/team tunnel expose --name demo --port 8080
```

预期：返回 `mode: local`（server 不暴露端口），提示队友用 `forward` 访问，隧道已持久化。

**frp 模式（可选）**：

```
/team tunnel expose --name demo --port 8080 --mode frp
```

预期：返回 `public_port`（例如 9100），server 暴露 `tcp://<server>:9100`。

> 手动等价（CLI）：
> ```bash
> TEAMX_SERVER_URL=https://<server>:5781 \
> TEAMX_MTLS_CERT=~/.teamx/letters/<id>/client.crt \
> TEAMX_MTLS_KEY=~/.teamx/letters/<id>/client.key \
> TEAMX_MTLS_CA=~/.teamx/letters/<id>/ca.crt \
> teamx tunnel expose demo --port 8080            # local（默认）
> teamx tunnel expose demo --port 8080 --mode frp # frp
> ```

### 2.3 任意成员（member-a）—— 查看/访问

在 member-a 的 opencode 窗口：

```
/team tunnel list
```

预期：列出 `demo`，包含 mode（local/frp）、公开端口（frp 才有）和 provider 的 LAN IP。

**local 模式 —— 本地转发**：

```
/team tunnel forward --name demo
```

预期：本地监听 `127.0.0.1:8080`（默认 = provider target 端口；冲突则返回随机候选端口需确认），提示"访问如本地服务"。

```bash
curl http://127.0.0.1:8080/index.html
# → hello from member-b's service
```

**frp 模式 —— 直接连 server 端口**：

```bash
curl http://<server>:9100/index.html
# → hello from member-b's service
```

```
/team tunnel status demo
```

预期：
```json
{
  "name": "demo",
  "port": 9100,
  "target_port": 8080,
  "lan_ip": "192.168.1.5",
  "same_subnet": true|false,
  "direct_addr": "192.168.1.5:8080",
  "relay_addr": "tcp://<server>:9100"
}
```

**经中继访问（总能通，即使网络无法直达）**：

```bash
curl http://<server>:9100/index.html
# → hello from member-b's service
```

**同网段直连（same_subnet=true 时）**：

```bash
curl http://<direct_addr>/index.html     # 例 http://192.168.1.5:8080/index.html
```

或让插件帮你选最优地址：

```
/team tunnel direct demo
# → same_subnet=true → direct_addr（直连）；否则 relay_addr（中继）
```

### 2.4 关闭隧道

member-b（或任意成员）：

```
/team tunnel close demo
```

预期：返回 `closed: true`，公开端口释放（`curl` 该端口立即失败）；持久化记录被清除（重启不再自动重建）。

## 3. 验收清单

- [ ] member-b `expose` 后，member-a 经 `http://<server>:9100/...` 能访问到 member-b 的本地服务
- [ ] `tunnel list` 显示该隧道（名称/公开端口/LAN IP）
- [ ] `tunnel status` 返回 `same_subnet` / `direct_addr` / `relay_addr` 三个字段
- [ ] 同网段时 `tunnel direct` 返回 `direct_addr`，直连访问成功
- [ ] 关闭后公开端口不可访问，持久化记录清除
- [ ] 重启 member-b 的 opencode 后，持久化隧道自动重建（`serve` 日志显示 `restored reverse tunnel`）

## 4. 故障排查

| 现象 | 原因 | 处理 |
|---|---|---|
| `expose` 报 "requires network mode" | 未设 `TEAMX_SERVER_URL` | 设置 `TEAMX_SERVER_URL=<server url>` 后重试 |
| `expose` 报 "tunnel `x` already exists" | 同名隧道已存在（或上次未关） | 先 `tunnel close x` 或换名字 |
| 访问公开端口 `connection refused` | 隧道未注册/已关闭 | `tunnel list` 确认；重试 `expose` |
| 访问超时 | serve 未启动/端口被墙 | 确认 `serve status`；跨机检查防火墙放行端口 |
| 直连失败但 `same_subnet=true` | member-b 本机防火墙拦了 target 端口 | 放行 member-b 的 target 端口；或改用中继地址 |
| 重启后隧道未自动重建 | 持久化文件被清（close 过）或 server_url 不匹配 | 重新 `expose`；确认 `TEAMX_SERVER_URL` 与 expose 时一致 |

## 5. 说明

- 隧道是 **TCP 级**（frp 风格），支持任意协议（HTTP/HTTPS/SSH/数据库/自定义），不限于 HTTP。
- 每个服务一条 WS 连接；provider 断开（网络抖动）会自动重连并重新注册。
- 公开端口范围 `9100-9999`（`teamx serve` 启动时分配）。
- 安全：与团队其他功能一致——mTLS 身份 + 同团队授权；`tunnel list/status/close` 仅本团队成员可用。
- UDP 隧道为未来计划（当前仅 TCP）。
