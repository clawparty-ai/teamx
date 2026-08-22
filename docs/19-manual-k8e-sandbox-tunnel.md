# 方案：dsh 主机经 teamx tunnel 访问 k8e 沙箱内服务

> 适用场景：用户在本机（dsh 主机）运行 DeepSeek Harness（dsh），通过
> `dsh-k8e-sandbox` 插件（KIP-20）把 agent 的 fs / subprocess / terminal 路由进
> 云端的 k8e 沙箱（K8E Sandbox Matrix）。Agent 在沙箱内启动的服务（如
> `python -m http.server 8080`、FastAPI、vLLM 等）监听在沙箱 pod 内网，本机无法
> 直接访问。本方案用 teamx 反向隧道，让 dsh 主机（本地）经公网访问沙箱 pod 内
> 的服务。

## 1. 目标

- 让 **dsh 主机（本地）** 通过 HTTP/TCP 访问 **云上沙箱 pod 内** 的服务。
- 使用 teamx 反向隧道承载跨公网传输，`local` 模式（默认，仅团队成员可访问）。
- 明确 k8e 沙箱的入站网络策略约束，并给出完整可复现的操作步骤。

## 2. 拓扑

```
┌─ 云端 ──────────────────────────────────────────────────────────────┐
│                                                                     │
│  [沙箱 pod (gVisor) : 服务 :8080]                                   │
│        │  (pod 网络; CNP 需放行 8080 入站)                           │
│  [k8e server/worker 节点]                                           │
│      ├─ kubectl port-forward pod/<pod> 18080:8080 → 127.0.0.1:18080 │
│      └─ teamx tunnel expose svc --port 18080 (local 模式)           │
│           │ 持久 mTLS WebSocket（节点→云）                           │
│  [teamx serve (云主机, 公网 IP, :5781)] ── 隧道注册表 + 中继        │
└────────────┬────────────────────────────────────────────────────────┘
             │ 公网 HTTPS/WSS 中继
┌────────────▼────────────────────────────────────────────────────────┐
│  [dsh 主机 (本地)]                                                  │
│      └─ teamx tunnel forward svc --local-port 18080                 │
│           └─ curl http://127.0.0.1:18080/  → 即达沙箱服务           │
└─────────────────────────────────────────────────────────────────────┘
```

数据流（local 模式，全程经 serve 中继）：

```
dsh 主机 curl 127.0.0.1:18080
  → teamx forward 本地监听
  → 公网 HTTPS → teamx serve（中继桥）
  → 节点 teamx expose 的持久 WebSocket
  → 节点 127.0.0.1:18080（kubectl port-forward）
  → 沙箱 pod :8080
```

## 3. 前置条件

| 项 | 要求 | 说明 |
|---|---|---|
| teamx serve | 云主机运行，公网可达 | `~/.teamx/serve.json` 记录 URL，如 `https://<公网IP>:5781` |
| teamx CLI | 云节点 + dsh 主机都安装 | `~/ .local/bin/teamx`（`install.sh` 安装） |
| 团队 | 一个网络模式团队 | owner 建队 + `serve start` |
| 成员身份 | 云节点与 dsh 主机都是**同一团队**成员 | mTLS 证书（invitation letter）即身份 |
| k8e 集群 | 云端运行，节点可登录 | 有 `kubectl` + kubeconfig |
| 沙箱会话 | dsh 会话已路由进沙箱 | `k8e-sandbox-cli sessions` 可见 Active 会话 |

## 4. 核心原理（为什么必须这样做）

1. **沙箱 pod 默认拒绝一切非 sandboxd 入站**。每个会话生成
   `CiliumNetworkPolicy sandbox-session-<sid>`
   （`k8e/pkg/sandboxmatrix/grpc/orchestrator.go` 的 `buildSessionCNP`），
   ingress 仅允许：
   - `fromEntities: [host]` → 端口 `2024`
   - `fromEndpoints: [app=sandbox-grpc-gateway]` → 端口 `2024`
   - `fromEndpoints: [app=e2b-server]` → 端口 `2024`

   CNP 一旦选中 endpoint，未匹配的入站流量一律丢弃。因此 agent 在 pod 里起的
   8080 服务**对任何人（含节点、kubectl port-forward）都不可达**——必须先显式
   追加一条 ingress 规则放行服务端口。

2. **沙箱网关没有 port-forward / TCP 代理 RPC**。`SandboxService`
   （`k8e/proto/sandbox/v1/sandbox.proto`）只有 fs / exec / pty 类接口，无法透传
   任意 TCP 端口。所以必须借道 `kubectl port-forward` 把 pod 端口桥到节点本地。

3. **出站相对宽松**：pod 默认允许 DNS 53 + TCP 443 到 world。这意味着"在 pod
   内跑隧道 provider"（方案 B，见 §10）可行但受 443 限制；本方案走节点
   provider，规避该限制。

4. **teamx tunnel 是反向隧道**：provider 暴露"本机本地端口"，经 serve 中继；
   consumer 用 `forward` 在本地映射端口。公网拓扑下 `same_subnet` 直连不适用，
   全程走 relay。

## 5. 分步操作

### 5.1 团队与成员身份

**owner（建队 + serve，若尚未做）**：

```bash
teamx team create sandbox-access
teamx serve start                       # 云主机上，绑定公网可访问的地址/端口
# 记录 serve URL：https://<公网IP>:5781
```

**云 k8e 节点加入团队**（任选其一）：

```bash
# 方式 1：CLI 直接 join（owner 审批）
teamx team join <invite_token> k8e-node --server https://<公网IP>:5781

# 方式 2：放置 letter 证书（等 owner 签发 teamx_team_invite 后）
mkdir -p ~/.teamx/letters/<letter-id>
cp client.crt client.key ca.crt ~/.teamx/letters/<letter-id>/
```

owner `teamx approve <member_id>` 通过后，节点即拥有 mTLS 身份。

**dsh 主机**：已是团队成员（此前流程），证书位于 `~/.teamx/letters/<id>/`。

### 5.2 确认沙箱会话与 pod

```bash
# dsh 主机或任意有权限的机器
k8e-sandbox-cli sessions                # 取 SID

# 云端节点
kubectl -n sandbox-matrix get pods -l sandbox.k8e.io/session-id=<SID> -o wide
kubectl -n sandbox-matrix get cnp sandbox-session-<SID>   # 确认 CNP
```

沙箱内服务必须 bind `0.0.0.0`（如在终端里
`python3 -m http.server 8080 --bind 0.0.0.0`），否则谁都进不去。

### 5.3 放行服务端口入站（关键）

```bash
SID=<沙箱会话ID>
SVC_PORT=8080

kubectl -n sandbox-matrix patch cnp sandbox-session-$SID --type=json -p="[
  {\"op\":\"add\",\"path\":\"/spec/ingress/-\",
   \"value\":{\"fromEntities\":[\"host\"],
             \"toPorts\":[{\"ports\":[{\"port\":\"$SVC_PORT\",\"protocol\":\"TCP\"}]}]}}]"
```

验证：

```bash
kubectl -n sandbox-matrix get cnp sandbox-session-$SID -o yaml | grep -A6 "fromEntities"
```

### 5.4 节点上桥接 pod 端口

```bash
kubectl -n sandbox-matrix port-forward pod/<pod> 18080:8080
# 保持前台运行；或后台：nohup ... &
```

验证（另开终端）：

```bash
curl -s http://127.0.0.1:18080/    # 应返回沙箱服务内容
```

### 5.5 节点上暴露隧道（provider）

```bash
export TEAMX_SERVER_URL=https://<公网IP>:5781
export TEAMX_MTLS_CERT=~/.teamx/letters/<id>/client.crt
export TEAMX_MTLS_KEY=~/.teamx/letters/<id>/client.key
export TEAMX_MTLS_CA=~/.teamx/letters/<id>/ca.crt

teamx tunnel expose svc --port 18080          # local 模式（默认）
# 若 CLI 强制要求 --session：传该成员的 session key（teamx member list 可查）
```

预期：注册成功，`mode: local`（serve 不暴露端口）。

> 可选 frp 模式（公开端口，给非团队成员直接访问）：
>
> ```bash
> teamx tunnel expose svc --port 18080 --mode frp
> # 返回 public_port，如 9100 → 任何人可访问 tcp://<公网IP>:9100
> ```

### 5.6 dsh 主机消费（consumer）

```bash
# dsh 主机（dsh-plugin V1 无 tunnel 工具，用 CLI）
export TEAMX_SERVER_URL=https://<公网IP>:5781
export TEAMX_MTLS_CERT=~/.teamx/letters/<id>/client.crt
export TEAMX_MTLS_KEY=~/.teamx/letters/<id>/client.key
export TEAMX_MTLS_CA=~/.teamx/letters/<id>/ca.crt

teamx tunnel list                      # 应列出 svc
teamx tunnel forward svc --local-port 18080
```

若本机装有 opencode-plugin，也可直接：

```
/team tunnel forward svc --local-port 18080
```

### 5.7 验证

```bash
curl -s http://127.0.0.1:18080/               # 沙箱服务响应
teamx tunnel status svc                       # 看 mode/relay_addr
```

预期 status：

```json
{
  "name": "svc",
  "port": 18080,
  "target_port": 8080,
  "lan_ip": "<节点内网IP>",
  "same_subnet": false,
  "relay_addr": "https://<公网IP>:5781"
}
```

## 6. 收尾与清理

```bash
teamx tunnel close svc        # 释放隧道
# 结束 port-forward（Ctrl-C / kill）
kubectl -n sandbox-matrix delete cnp sandbox-session-$SID   # 可选，会话销毁时 CNP 自动清理
```

## 7. 自动化脚本（推荐）

沙箱会话临时性决定了 §5.3–5.5 需随会话重建，封装脚本：

```bash
#!/usr/bin/env bash
# expose-sandbox <sid> <svc_port> <tunnel_name> [local_port]
set -euo pipefail
SID=$1; SVC_PORT=$2; NAME=$3; LOCAL_PORT=${4:-$((RANDOM%1000+18000))}
POD=$(kubectl -n sandbox-matrix get pods -l sandbox.k8e.io/session-id=$SID -o jsonpath='{.items[0].metadata.name}')

# 1. 放行 CNP 入站
kubectl -n sandbox-matrix patch cnp sandbox-session-$SID --type=json -p="[{\"op\":\"add\",\"path\":\"/spec/ingress/-\",\"value\":{\"fromEntities\":[\"host\"],\"toPorts\":[{\"ports\":[{\"port\":\"$SVC_PORT\",\"protocol\":\"TCP\"}]}]}}]"

# 2. port-forward
kubectl -n sandbox-matrix port-forward pod/$POD $LOCAL_PORT:$SVC_PORT &
PF_PID=$!
sleep 2

# 3. 暴露隧道
TEAMX_SERVER_URL=https://<公网IP>:5781 \
TEAMX_MTLS_CERT=~/.teamx/letters/<id>/client.crt \
TEAMX_MTLS_KEY=~/.teamx/letters/<id>/client.key \
TEAMX_MTLS_CA=~/.teamx/letters/<id>/ca.crt \
  teamx tunnel expose $NAME --port $LOCAL_PORT

echo "consumer: teamx tunnel forward $NAME --local-port $LOCAL_PORT"
wait $PF_PID
```

## 8. 故障排查

| 现象 | 原因 | 处理 |
|---|---|---|
| 访问超时/拒绝 | CNP 未放行 | §5.3 patch；确认 `ingress` 已含服务端口 |
| 服务连不上但 CNP 已放行 | 服务 bind 127.0.0.1 | 改 bind 0.0.0.0 |
| `expose` 报 requires network mode | 未设 `TEAMX_SERVER_URL` | 导出后重试 |
| `expose` 报 already exists | 同名隧道残留 | `tunnel close` 或改名 |
| `forward` 连不上 | mTLS 证书错 / 成员未审批 | 检查 `~/.teamx/letters/<id>/` 与 `teamx status` |
| 公网不通 | 云安全组未放行 5781 | 云控制台放行 |
| pod 被回收后失联 | 会话销毁 | 重建会话重跑脚本 |

## 9. 风险与限制

- **CNP 放行是硬性前置**，这是沙箱隔离设计，不可绕过（除非走方案 B）。
- 隧道生命周期 ≤ 沙箱会话生命周期；pod 暂停/回收即断。
- `local` 模式仅同团队成员可访问；`frp` 模式放开给公网，默认不推荐。
- 隧道为 TCP 级，支持任意协议（HTTP/SSH/DB/自定义）。

## 10. 备选方案 B：隧道 provider 跑在沙箱 pod 内

> 不推荐，仅作对比记录。核心思路：利用 pod 默认允许的出站 TCP 443，让 pod
> 内进程直接向 teamx serve 发起隧道，无需改 CNP 入站、无需节点当成员。

约束：

- teamx serve 必须在 pod 可达的 **TCP 443** 上（默认 CNP 只放行 53/443）；
  `TEAMX_SERVER_URL` 用 `https://<公网IP>`（443）。
- 需把 teamx 二进制 + mTLS 证书写进沙箱
  （`k8e-sandbox-cli write <sid> /workspace/teamx ...` + `chmod +x`）。
- 沙箱终端里起 provider：`... /workspace/teamx tunnel expose <name> --port <svc_port>`。
- pod 临时，隧道生命周期 = 会话生命周期。

优点：不碰 CNP、不需要节点当成员。缺点：server 必须 443 可达（否则要改会话
egress 策略或启用 Cilium DNS proxy + allowedHosts）；teamx 是 Rust 二进制，需能
跑在沙箱镜像里；操作更重。

## 11. 相关源码位置（备忘）

- 沙箱 CNP 生成：`k8e/pkg/sandboxmatrix/grpc/orchestrator.go` `buildSessionCNP`
- 命名空间 / 标签：`sandbox-matrix`，`sandbox.k8e.io/session-id`
- 沙箱网关 RPC 面：`k8e/proto/sandbox/v1/sandbox.proto`（无 port-forward）
- teamx 隧道机制：`teamx/docs/17-manual-tunnel.md`、`teamx/crates/teamx/src/tunnel_client.rs`
- dsh 插件说明：`teamx/dsh-plugin/README.md`（V1 无 tunnel 工具）
