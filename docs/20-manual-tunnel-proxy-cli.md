# teamx 隧道与代理手动测试指南（纯 CLI，不依赖 opencode/dsh）

> 本文指导你**只用 `teamx` 命令行**手工验证网络功能的三种隧道模式（frp / local / proxy）与 SOCKS5 出站代理的完整生命周期。
>
> 每个场景都与自动化综合测试 `tests/tunnel-proxy-comprehensive.ts`（43 项断言）一一对应；自动化跑通后，本文让你能在真实终端里逐步复现同样的行为。
>
> 插件版（opencode 内 `/team tunnel …`）的手册见 `docs/17-manual-tunnel.md`；三人协作的网络模式入门见 `docs/16-manual-network.md`。

## 0. 前置条件

1. 已构建二进制：

   ```bash
   cargo build          # 产物: target/debug/teamx（下文简写为 $TEAMX）
   export TEAMX="$PWD/target/debug/teamx"
   ```

2. 工具：`curl`（HTTP 验证 + SOCKS5 验证）、`python3`（起示例服务）。隧道支持任意 TCP 协议，这里统一用 HTTP 服务便于观察。

3. 操作系统允许绑定 127.0.0.1 的端口（示例用到 8080 / 1080 / 9100-9999 / 18080）。

## 1. 三种模式一张图

```
frp（服务器暴露公网端口）        local（默认，服务器零暴露）       proxy（SOCKS5 出站代理）

member-b :8080                  member-b :8080                   目标服务(任意 host:port)
   │ expose --mode frp              │ expose（默认 local）            ▲ member-b dial 动态目标
   ▼ WS(mTLS)                       ▼ WS(mTLS)                       │ WS(mTLS)
┌──────────────┐                 ┌──────────────┐                ┌──────────────┐
│ teamx serve  │                 │ teamx serve  │                │ teamx serve  │
│ 公网端口 9100+│                 │ 只桥接,不开端口│                │ 只桥接,不开端口│
└──────▲───────┘                 └──────▲───────┘                └──────▲───────┘
       │ TCP 直接连                    │ forward 本地映射                │ 本地 SOCKS5 :1080
   member-a curl                  member-a :18080                 member-a curl --socks5-hostname
```

| 模式 | 注册后 server 端口 | 消费方访问方式 | 典型用途 |
|---|---|---|---|
| `frp` | 分配 9100-9999 | 直连 `tcp://<server>:<port>` | 临时给全队一个可直达入口 |
| `local`（默认） | **不占端口**（注册返回 port=0） | `tunnel forward` 映射到本机端口 | SSH `-L` 体验，最安全 |
| `proxy` | 不占端口（port=0），无固定目标 | 本地 SOCKS5 端口，目标由 CONNECT 动态指定 | 让队友的出口 IP 变成自己的 |

## 2. 环境准备：单机模拟「服务器 + 成员 A + 成员 B」

用三个终端 + 三份 `TEAMX_HOME` 模拟三方。成员 B 通过 invitation letter 获得 mTLS 材料，之后所有隧道命令**自动发现**服务器地址与证书（来自 `$TEAMX_HOME/letters/<id>/`），无需再传。

### 2.1 目录布局

```bash
LAB=/tmp/teamx-lab && rm -rf "$LAB" && mkdir -p "$LAB/server" "$LAB/a" "$LAB/b"
```

### 2.2 终端 ①——服务器 + owner

```bash
export LAB=/tmp/teamx-lab
export TEAMX=/path/to/target/debug/teamx
export TEAMX_HOME=$LAB/server TEAMX_DB=$LAB/server/teamx.db

$TEAMX init                                   # 生成实例 CA（$TEAMX_HOME/ca/）
$TEAMX serve --addr 127.0.0.1 --port 5781     # 前台运行，保持不动
```

### 2.3 终端 ②——owner 运维命令（建队 / 发信 / 批准）

```bash
export LAB=/tmp/teamx-lab
export TEAMX=/path/to/target/debug/teamx
export TEAMX_HOME=$LAB/server TEAMX_DB=$LAB/server/teamx.db

# 建队，拿到 owner 成员 id 并签发客户端证书
$TEAMX team create Lab --session s:a --json | tee create.json
OWNER_ID=$(python3 -c 'import json;print(json.load(open("create.json"))["owner_member_id"])')
$TEAMX cert issue "$OWNER_ID" owner --out "$LAB/a"

# owner 自己也要有 mTLS 材料才能调 tunnel/proxy 子命令（它们走网络而非本地 DB）
export TEAMX_MTLS_CERT=$LAB/a/member.crt \
       TEAMX_MTLS_KEY=$LAB/a/member.key \
       TEAMX_MTLS_CA=$LAB/server/ca/ca.crt

# 给成员 B 发邀请信
$TEAMX team invite "contributor: 提供本地服务" --session s:a --json | tee invite.json
MEMBER_B=$(python3 -c 'import json;print(json.load(open("invite.json"))["member_id"])')
```

### 2.4 终端 ③——成员 B 导入邀请信

```bash
export LAB=/tmp/teamx-lab
export TEAMX=/path/to/target/debug/teamx
export TEAMX_HOME=$LAB/b

cp "$LAB/server/invite.json" .   # 或从消息渠道拿到 letter 字符串存入 letter.json
LETTER=$(python3 -c 'import json;print(json.load(open("invite.json"))["letter"])')

# 关键一步：import 是“落库”操作，单机演练时临时借用 server 的 DB 完成占座；
# mTLS 材料会写入 $TEAMX_HOME/letters/<invitation-id>/（属于 B 自己的 HOME）。
export TEAMX_DB=$LAB/server/teamx.db
$TEAMX team import "$LETTER" --name DevB --session s:b
unset TEAMX_DB

# 之后 B 的所有网络命令都不需要任何环境变量以外的配置：
# 服务器 URL ← 自动读 letter 里嵌入的 https://127.0.0.1:5781
# 客户端证书 ← 自动读 letters/<id>/client.crt|key、ca.crt
```

### 2.5 回到终端 ②：批准成员 B

```bash
$TEAMX team approve "$MEMBER_B" --session s:a
$TEAMX team status --session s:a --json | python3 -m json.tool   # members 应含 owner + DevB(active)
```

> **两台真实机器时**：invite 加 `--server-url https://<LAN_IP>:5781`；serve 改 `--addr 0.0.0.0 --san <LAN_IP>`；成员侧无法共享 DB，改用带客户端证书的 RPC 导入：
> ```bash
> LETTER='teamx-inv:v1:…'
> python3 - "$LETTER" <<'PY'      # 解出证书三件套到 ./mtls/
> import sys,json,base64,pathlib
> d=json.loads(base64.b64decode(sys.argv[1][len('teamx-inv:v1:'):]))
> c=d['certificates']; p=pathlib.Path('mtls'); p.mkdir(exist_ok=True)
> (p/'client.crt').write_text(c['client_cert']); (p/'client.key').write_text(c['client_key'])
> (p/'ca.crt').write_text(c['ca_cert'])
> PY
> curl --cacert mtls/ca.crt --cert mtls/client.crt --key mtls/client.key \
>      -H 'Content-Type: application/json' \
>      -d "{\"method\":\"team.import\",\"args\":{\"letter\":\"$LETTER\",\"name\":\"DevB\"}}" \
>      https://<LAN_IP>:5781/rpc
> # 之后给该终端 export TEAMX_MTLS_CERT/KEY/CA 指向 ./mtls/ 即可
> ```

## 3. 测试一：FRP 隧道（公网端口中继）

**对应自动化断言：Section 2（9 项）**

### 3.1 终端 ③（B）：起本地服务并暴露

```bash
mkdir -p /tmp/www && echo "hello from member-b" > /tmp/www/index.html
python3 -m http.server 8080 --directory /tmp/www &

$TEAMX tunnel expose web --port 8080 --mode frp --session cli
# 期望输出:
# ok tunnel registered: name=web mode=frp port=9100
```

进程保持前台运行（这就是 provider 连接；Ctrl-C 会触发断连清理，见 §7）。

### 3.2 终端 ②（A）：经公网端口访问 + 查询状态

```bash
curl http://127.0.0.1:9100/
# → hello from member-b          （字节经 serve 中继到达 B 的 8080）

$TEAMX tunnel list --session cli
# → tunnels 数组含 {"name":"web","mode":"frp","port":9100,"target_port":8080,...}

$TEAMX tunnel status web --session cli
# 单机演练时 A、B 同在 loopback：
#   same_subnet=true, direct_addr="127.0.0.1:8080"   ← 同 /24 网段提示直连
# 跨网段时 same_subnet=false，只有中继地址可用

$TEAMX tunnel close web --session cli
curl --max-time 2 http://127.0.0.1:9100/ || echo "✓ 端口已释放，连接被拒"
```

## 4. 测试二：Local 隧道 + forward（服务器零暴露）

**对应自动化断言：Section 3（5 项）**

### 4.1 终端 ③（B）：以默认 local 模式暴露

```bash
$TEAMX tunnel expose web2 --port 8080 --session cli
# 期望输出:
# ok tunnel registered: name=web2 mode=local port=0     ← 0 = server 未开任何端口
```

### 4.2 终端 ②（A）：确认零暴露，然后本地映射访问

```bash
$TEAMX tunnel status web2 --session cli | grep -E '"mode"|"port"'
# → "mode":"local", "port":0
ss -ltn | awk '$4 ~ /:91[0-9][0-9]$/'   # 无 9100+ 监听（Linux）；macOS 用 lsof -iTCP -sTCP:LISTEN

$TEAMX tunnel forward web2 --local-port 18080 --session cli
# 期望输出:
# ok forward: name=web2 listening on 127.0.0.1:18080 (access like a local service)

curl http://127.0.0.1:18080/
# → hello from member-b      （A:18080 →WS→ serve →WS→ B:8080）
```

验证完 Ctrl-C 结束 forward。local 模式下 server 全程只转发加密 WS 流量，不暴露 TCP 端口。

## 5. 测试三：SOCKS5 出站代理（proxy exit / proxy start）

**对应自动化断言：Section 4（4 项）**

角色互换：这次 **B 是出口**（egress），A 把自己的流量借道 B 发出去。

### 5.1 终端 ③（B）：启动代理出口

```bash
$TEAMX proxy exit egress
# 进程保持运行。注册 mode=proxy、port=0 的特殊隧道；每个流的目标由消费方动态指定
```

### 5.2 终端 ②（A）：启动本地 SOCKS5 端口

```bash
$TEAMX proxy start --port 1080 --exit egress
# 期望输出:
# ok proxy: exit=egress SOCKS5 listening on 127.0.0.1:1080
#   (set curl --socks5-hostname or browser proxy)
```

### 5.3 验证：A 经 B 的出口访问 B 所在网络的服务

```bash
# 目标本机服务（B 的 8080，仍在跑）
curl --socks5-hostname 127.0.0.1:1080 http://127.0.0.1:8080/
# → hello from member-b

# 也可解析域名型目标（CONNECT ATYP=domain 由 B 侧代为拨号）
curl --socks5-hostname 127.0.0.1:1080 https://example.com -o /dev/null -w '%{http_code}\n'

$TEAMX tunnel list --session cli | grep egress
# → {"name":"egress","mode":"proxy","port":0,...}
```

### 5.4 断开出口 → 注册表自动清理

```bash
# 终端 ③ Ctrl-C 掉 proxy exit，稍候 1 秒：
$TEAMX tunnel list --session cli   # egress 已消失
curl --max-time 2 --socks5-hostname 127.0.0.1:1080 http://127.0.0.1:8080/ \
  || echo "✓ 出口下线后代理不可用"
```

## 6. 测试四：多隧道共存与选择性关闭

**对应自动化断言：Section 5（8 项）**

```bash
# 终端 ③（B）：再开第二个 HTTP 服务并暴露两条 frp 隧道
echo svc-a > /tmp/www/a.html && echo svc-b > /tmp/www/b.html
python3 -m http.server 8081 --directory /tmp/www &
$TEAMX tunnel expose svc-a --port 8080 --mode frp --session cli   # → port=9100
$TEAMX tunnel expose svc-b --port 8081 --mode frp --session cli   # → port=9101（另一条独立 WS）

# 终端 ②（A）：两条都可达，互不影响
curl http://127.0.0.1:9100/a.html   # → svc-a
curl http://127.0.0.1:9101/b.html   # → svc-b
$TEAMX tunnel list --session cli      # 同时列出 svc-a / svc-b

# 只关 svc-a：
$TEAMX tunnel close svc-a --session cli
curl --max-time 2 http://127.0.0.1:9100/ || echo "✓ svc-a 已关"
curl http://127.0.0.1:9101/b.html    # → svc-b 仍然正常
$TEAMX tunnel list --session cli      # 只剩 svc-b
```

重名保护：让 B 再执行一次 `$TEAMX tunnel expose svc-b --port 8081 --mode frp --session cli`，应得到错误
`tunnel 'svc-b' already exists in this team`，且原隧道不受影响。

## 7. 测试五：Provider 断连自动清理 + 端口池回收

**对应自动化断言：Section 7-8（13 项）**

```bash
# 终端 ③（B）：新起一条 frp 隧道
$TEAMX tunnel expose ghost --port 8080 --mode frp --session cli    # 记下分配的端口，如 9102
```

```bash
# 终端 ②（A）：确认可达后，让 B Ctrl-C
$TEAMX tunnel list --session cli | grep ghost                      # 存在
sleep 1 && $TEAMX tunnel list --session cli                       # ghost 已消失（WS 断开即清理）
curl --max-time 2 http://127.0.0.1:9102/ || echo "✓ 公网端口随断连关闭"

# 端口池：连续 expose 多条 frp 隧道，分配值单调递增且都在 9100-9999 内；
# close 一条后重新 expose，可复用刚释放的端口号（自动化测试观察到 9101 复用）。
```

边界行为速查：

| 操作 | 期望结果 |
|---|---|
| `expose` 重名 | 报错 `already exists in this team`，原隧道不受影响 |
| `close` 不存在的名字 | 返回 `{"closed":false,"freed_port":null}`（幂等，不算错） |
| `status` 不存在的名字 | 报错（ok=false） |
| 空团队 `list` | 空 `tunnels` 数组 |
| provider WS 断开 | ≤1 秒内从注册表移除、释放端口、关闭中继 |

## 8. 验收清单

- [ ] serve 健康：owner 带 mTLS `curl --cacert $TEAMX_HOME/ca/ca.crt --cert … --key … https://127.0.0.1:5781/health` → `"ok":true`
- [ ] frp：`curl http://<server>:<port>/` 拿到 B 服务内容；`close` 后同端口拒绝连接
- [ ] local：`status` 显示 `port=0`；A `forward` 后 `curl 127.0.0.1:<本地端口>` 可用；server 无新增监听
- [ ] proxy：A 上 `curl --socks5-hostname` 经 B 出口取回内容；并发多条连接互不串流
- [ ] 多隧道：互不影响；选择性关闭正确
- [ ] 断连：provider Ctrl-C 后 ≤1s 注册表清理、端口回收
- [ ] 重名 / 不存在名字的 close/status 表现符合 §7 表格

## 9. 故障排查

| 症状 | 原因 | 处理 |
|---|---|---|
| `no mTLS material: import an invitation letter or set TEAMX_MTLS_CERT/KEY/CA` | 该 HOME 下没有证书材料 | 完成 §2.4 导入；或显式 export `TEAMX_MTLS_CERT/KEY/CA` |
| `connect …: invalid peer certificate` / TLS 握手失败 | 服务器证书 SAN 不含所连主机名 | 跨机器访问时 serve 加 `--san <LAN_IP>`，并用 letter 里嵌的同一地址连接 |
| expose 报 `already exists in this team` | 同名隧道在线 | 先 `close` 或换名 |
| frp 端口连不上但 list 里有 | provider 进程已死或被防火墙拦截 | 确认 expose 进程存活；检查 server 主机防火墙放行 9100-9999 |
| forward 卡住无响应 | provider 侧未桥接 / 本地目标端口不对 | 确认 B 的 expose 进程在跑且 `--port` 指向真实监听端口 |
| SOCKS5 收到 `general SOCKS server failure`(05 05) | 出口不在线或目标不可达 | `tunnel list` 确认 egress 存在；在 B 上手动 `curl <目标>` 验证可达性 |
| SOCKS5 收到 `Connection refused`(05 05) / `Can't complete SOCKS5 connection`(97) | **隧道 WS 空转被 NAT/中间设备静默掐断**，注册表里隧道已消失，或 provider 的 WS 已半开 | 确认 proxy exit / proxy start 已升级到含**心跳+自动重连**的版本（`>= 0.2.1`，见 §11）；两个进程会自动重连并重新注册，**无需手动重启**；若仍在旧版，重启两端即可临时恢复 |
| `tunnel port pool exhausted (9000-9999)` | 900 条 frp 隧道占满 | 清理不再使用的隧道 |
| 成员报 `not a member of team …` | 用了错误的 team 标识 / 未 approve | RPC 按**证书 CN** 定身份，确认该成员已被 approve |

## 10. 与自动化测试的对应关系

| 手动章节 | 自动化 Section | 断言数 |
|---|---|---|
| §3 frp | 2. FRP Tunnel | 9 |
| §4 local + forward | 3. Local Tunnel | 5 |
| §5 SOCKS5 代理 | 4. Proxy Tunnel | 4 |
| §6 多隧道 | 5. Multi-Tunnel | 8 |
| §7 边界 + 断连 + 端口池 | 6/7/8 | 17 |
| **合计** | | **43** |

一键回归：

```bash
bun tests/tunnel-proxy-comprehensive.ts    # 或 ./tests/run-all.sh 的第 13 步
```

## 11. 连接保活与自愈（生产环境必读）

### 背景

长时间空转后，隧道 WebSocket（`/tunnel` provider 通道与 `/tunnel/forward` consumer 通道）可能被 NAT、云厂商防火墙或中间代理**静默掐断**——两端都感知不到（没有 FIN，只有丢弃），于是：

- 注册表里仍显示隧道存在，但真实连接已死（stale / half-open）
- 消费方的每个新请求在链路上丢失，表现为 `curl: (97) Can't complete SOCKS5 connection` 或 SOCKS5 `Connection refused`(05 05)
- 旧版本需要**手动重启两端进程**才能恢复

### 修复内容（代码层）

从 `0.2.1` 起（commit `4167ab5`）：

| 层 | 机制 | 效果 |
|---|---|---|
| server 侧 `serve.rs` | 所有隧道 WS 通道（provider `/tunnel` + consumer `/tunnel/forward`）每 **30s 发送应用层 `{"type":"ping"}`** 心跳 | 主动探测链路，让中间设备持续看到活跃流量，避免被当作空闲连接回收 |
| client 侧 `tunnel_client.rs` | 客户端收到 `ping` 回 `pong`；`run_expose`（`proxy exit` / `tunnel expose`）在 WS 断开后**指数退避自动重连**（1s → 2s → 4s … 上限 30s）并**自动重新注册隧道** | provider 端断线自愈，新注册的隧道让消费方恢复可用 |

> 版本检查：`teamx --version`。若 server 或 client 任一低于 0.2.1，请升级二进制（重新 `cargo build --release` 部署）。

### 运维兜底（进程级）

即使心跳 + 重连已覆盖 WS 层，仍建议对长期运行的进程做**进程级守护**，应对进程崩溃、机器重启等场景：

**云主机（systemd）** —— `teamx serve` 与 `proxy exit` 各建一个 unit：

```ini
# /etc/systemd/system/teamx-serve.service
[Unit]
Description=teamx network-mode server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=ubuntu
Environment=TEAMX_HOME=/home/ubuntu/.teamx
Environment=TEAMX_DB=/home/ubuntu/.teamx/teamx.db
ExecStart=/usr/local/bin/teamx serve --addr 0.0.0.0 --port 8888 --san hub03.flomesh.io
Restart=always
RestartSec=3
NoNewPrivileges=true
ProtectSystem=full

[Install]
WantedBy=multi-user.target
```

```ini
# /etc/systemd/system/teamx-proxy-exit.service
[Unit]
Description=teamx proxy exit egress
After=teamx-serve.service
Wants=teamx-serve.service

[Service]
Type=simple
User=ubuntu
ExecStart=/home/ubuntu/start-exit.sh
Restart=always
RestartSec=5
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
```

> `start-exit.sh` 里 export `TEAMX_HOME` / `TEAMX_DB` / `TEAMX_SERVER_URL` / `TEAMX_MTLS_CERT|KEY|CA` 后 `exec teamx proxy exit <name>`。

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now teamx-serve teamx-proxy-exit
```

**本地（macOS/Linux 客户端）** —— 用 while 循环守护 `proxy start`：

```bash
while true; do
  teamx proxy start --port 1080 --exit egress
  sleep 5   # 进程异常退出后自动重启
done
```

### 验证清单

- [ ] `teamx --version` >= 0.2.1（server 与 client 两侧）
- [ ] 长空闲测试：`proxy start` + `proxy exit` 空闲 > 心跳周期（如 100s）后，`curl --socks5-hostname 127.0.0.1:1080 https://example.com` 仍可用
- [ ] 自愈测试：`sudo systemctl kill -s SIGKILL teamx-serve` 后，serve 由 systemd 拉起，proxy exit 自动重连并重新注册 egress（观察 `journalctl -u teamx-proxy-exit` 出现 `ok tunnel registered`），代理全程可用
