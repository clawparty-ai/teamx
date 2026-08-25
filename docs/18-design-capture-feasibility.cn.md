# 18 — 可行性分析：本地流量抓包（HTTP 级 / TCP 级）

> 状态：**可行性分析（待确认方向后出实施方案）**
> 关联：`docs/09-design-tun0.*.md`、`docs/06-design-proxy.*.md`、`docs/17-design-tun0-improvements.*.md`
> 日期：2026-08-24

## 0. 问题定义

无论用户采用哪种使用方式（tun0 透明代理 / SOCKS5 proxy / tunnel 转发），
能否在 teamx 内部抓取本地收发的数据？分两级：

- **L1 HTTP 抓包**：类似 Fiddler —— 看到 HTTP 请求行/头/体、响应状态/头/体，
  可按域名/方法过滤。
- **L2 TCP 抓包**：类似 tcpdump —— 看到连接级/包级记录（五元组、方向、
  长度、时序，理想情况下含 TCP 头部字段）。

## 1. 关键代码事实（决定可行性的基础）

| # | 事实 | 出处 |
|---|---|---|
| K1 | tun0 的 smoltcp 栈已把 IP 包**重组为完整 TCP 字节流**；数据泵读到的就是应用层 payload（无 TCP 头） | `tun_socks::pump_active` → `recv_from_socket` |
| K2 | tun fd 上能读到**原始 IP 包**（进），smoltcp 发出的包能写回（出）——即天然存在一个全量 IP 包观测点 | `tun_stack.rs TunPhy::receive/transmit` |
| K3 | SOCKS5 CONNECT 握手解析出**目标域名**（明文可得） | `socks5::parse_connect_request → SocksTarget{host,port}` |
| K4 | tun0 模式下目标域名可通过 ip_map/fake-ip 映射回域名 | `resolve_target` |
| K5 | 项目已依赖 `rcgen` 且 `pki.rs` 已实现 CA 生成 + 按 CN/SAN 签发证书 —— **动态 MITM 证书的技术栈已存在** | `pki.rs cert_params/ensure_pki` |
| K6 | 三条路径的字节流都流经 teamx 进程内的 relay 循环（proxy 的 socks5 relay / tun0 的 pump_active / tunnel 的 forward relay） | 各对应文件 |

结论先行：**两条路径（K2/K6）意味着 teamx 在所有模式下都处于天然的中间人
位置。L2 在 tun0 下近乎免费；L1 明文 HTTP 近乎免费；L1 HTTPS 需要引入
TLS MITM（K5 使其可行但工作量最大）。**

## 2. L2 — TCP 级抓包（tcpdump 能力）

### 2.1 tun0 路径：✅ 完全可行（近零成本）

`tun fd` 是全量 IP 包的必经点：

- **入向**：`TunPhy::receive` 里 `read_packet` 读到的就是完整 IPv4 包。
- **出向**：smoltcp 应答包在 `Tx::consume` 写回 tun 前可截获。

实现形态（建议做成独立子命令 + 可选内嵌）：

```
teamx capture tun0 [--filter <bpf-ish>] [-w <file>] [--ring N]
```

- 记录格式建议直接用 **pcap 格式**（LINKTYPE_RAW / LINKTYPE_IPV4），
  这样 Wireshark/tcpdump 可直接打开 —— 不自造格式，生态免费拿。
- 过滤器 v1 用简单结构化规则（host/port/proto/cidr），不必实现完整 BPF。
- 注意：tun0 只能看到**被路由进 tun 的流量**（拦截网段/CIDR/DNS 命中域名
  的 IP）。它是"我们转发的流量"的 tcpdump，不是整机网卡级 tcpdump。

### 2.2 proxy / tunnel 路径：⚠️ 部分可行（"合成包"级别）

这两条路径只有重组后的字节流，没有 IP/TCP 头。能做到的是：

- **连接级记录**（等价于 `ss`/netstat 事件流）：时间戳、五元组
  （本地端口、目标 host:port——proxy 下是真实域名）、方向、字节数、
  关闭原因。这已经覆盖 tcpdump 最常用的分析场景（谁连了什么、多少流量、
  什么时候断的）。
- **伪包头流**：按 chunk 方向/长度合成"逻辑包"序列。可以画时序图，
  但 TTL/flags/窗口这些真实头部字段不存在。

实现位置：`open_tunnel_bridge` 与 socks5 relay 的收发两端各加一个可选的
tap 回调/通道。

### 2.3 全机抓包说明

若需求升级为"整台机器所有流量"（不限于 teamx 转发的），那需要 BPF 设备
（libpcap / BPFDevice）+ root，与 teamx 的转发功能无关，不建议混进来。

## 3. L1 — HTTP 抓包（Fiddler 能力）

### 3.1 分层现实

| 流量类型 | 可见性（当前架构） |
|---|---|
| 明文 HTTP | 字节流直接可见，解析即可（K1/K6） |
| HTTPS（绝大多数） | 只有 **TLS 密文**；要看内容必须本地终止 TLS（MITM） |

### 3.2 HTTPS MITM 的三个必要件

1. **动态证书签发**：对每个 SNI/域名现场签一张"CA 签发"的证书。
   → `rcgen` 已在依赖里，`pki.rs` 的 `cert_params(cn, sans, is_ca)`
   改造即可复用（生成一个专用 capture CA，签发叶子证书含该域名的 SAN）。
2. **客户端信任 CA**：把 capture CA 安装进系统钥匙串并信任
   （macOS `security add-trusted-cert`，需一次管理员授权；GUI 引导一次性完成）。
   这是 Fiddler/Charles/Proxyman 同款流程，无法绕过（协议使然）。
3. **TLS 终止 + 上游重建**：本地以假证书与客户端握手 → 解密得到明文 HTTP →
   记录/展示 → 以真实 SNI 向出口重新发起 TLS 转发。

### 3.3 各路径落地点

| 路径 | MITM 注入点 | 目标域名来源 | 工作量评估 |
|---|---|---|---|
| proxy (SOCKS5) | relay 收发循环处替换为"本地 TLS 服务端 ↔ 出口 TLS 客户端" | CONNECT 域名（K3，最可靠） | 中 |
| tun0 | pump_active 收发循环同上 | ip_map 反查域名（K4）；无映射时只能按 SNI 透传不可解密 | 中偏大（依赖 DNS 命中） |
| tunnel | consumer relay 同上 | forward 配置里的目标 | 中 |

**推荐先做 proxy 路径**：域名来源最可靠（CONNECT 明文）、注入点单一、
且与"浏览器手动配代理"的 Fiddler 使用习惯一致。

### 3.4 MITM 的边界（必须写清楚）

- **Certificate Pinning 应用会失败**（银行/部分 App/CLI 用 SPKI pin）。
  Fiddler 同样处理不了，属于协议层限制。策略：对 pinning 连接自动降级为
  "透传不解密"（检测到握手失败即切 passthrough），保证不断网。
- **非 HTTP over TLS**（如 gRPC 二进制、WebSocket 帧）：TLS 能解，
  但"HTTP 语义视图"需要按协议识别；v1 可以只给字节流 hex 视图。
- **QUIC/HTTP3（UDP）**：当前三条路径都不转发 UDP（tun0 也只处理 TCP），
  天然不在范围内；浏览器会回落 HTTP/2 over TCP。需要在文档注明
  "禁用 QUIC 或忽略其流量"。

## 4. 总体推荐架构

```
                ┌──────────────────────────────┐
                │        Capture Hub（新增）     │
                │  - tap 注册表（每连接一个 sink） │
                │  - L2 writer: pcap 文件/环形缓冲 │
                │  - L1 parser: HTTP 事件流       │
                │  - 展示: CLI(tail) / 面板卡片    │
                └──────────────────────────────┘
                   ▲            ▲            ▲
             tun_fd 原始包   relay 字节流   socks5 relay
             (仅 tun0 模式)  (三路通用)     (域名最可靠)
```

- **L2 tap**：挂在 `TunPhy::receive/consume`（tun0）+ 三个 relay 的连接
  open/close 事件（全模式）→ pcap writer。
- **L1 plain**：relay 字节流旁路一份给 HTTP 增量解析器（明文才有效）。
- **L1 TLS-MITM**：作为 proxy 路径的可选模式（`teamx proxy start --capture-tls`）
  + capture CA 生成/信任引导。

分期建议：

| 期 | 内容 | 交付效果 |
|---|---|---|
| P1 | L2：tun0 原始包 → pcap 文件（Wireshark 可开）+ 连接级事件日志（全模式） | "teamx 版 tcpdump"（限转发流量） |
| P2 | L1a：明文 HTTP 解析 + 面板/CLI 实时视图 | 弱加密场景立即可看 |
| P3 | L1b：proxy 路径 TLS MITM（capture CA + 动态签发 + 透传降级） | 完整 Fiddler 体验（HTTPS） |

## 5. 风险与注意

- **隐私/合规**：MITM 解密的是用户自己的流量，但产品文案必须明确提示；
  capture CA 私钥保存在 `~/.teamx/capture-ca/`（0600）。
- **性能**：tap 默认关闭；开启时 ring buffer 有界，防止内存膨胀；
  pcap 写盘用异步线程，避免拖慢数据泵。
- **pinning 降级**必须自动化，否则一次握手失败 = 用户断网感知。
- **与三个改进项的关系**：L2 tap 点在 `TunPhy`，与 AsyncFd 改造（同一文件）
  建议排序上错开；bridge 异步化（B1）会让 relay 并发增加，tap 需按连接
  （而非全局）缓冲，避免交叉。

## 6. 结论

- **L2（TCP/包级）**：tun0 路径 ✅ 直接可行且成本低（输出标准 pcap）；
  proxy/tunnel 路径给出连接级 + 合成包记录。
- **L1（HTTP）**：明文 ✅ 低成本；HTTPS 需要 TLS MITM —— 技术栈已具备
  （rcgen/pki.rs），**推荐从 proxy 路径切入**，配合一次性 CA 信任引导 +
  pinning 自动降级。
