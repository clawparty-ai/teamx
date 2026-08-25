# 19 — Enterprise TODO：本地流量抓包体系（L2 抓包 / 后解密 / TLS MITM）

> 状态：**TODO（设计定稿，未实施）**
> 关联：`docs/18-design-capture-feasibility.cn.md`（可行性分析）、
> `docs/09-design-tun0.cn.md`（tun0）、`docs/06-design-proxy.cn.md`（proxy）
> 日期：2026-08-24

三个方案构成一个完整的「本地流量可见性」产品线，共享同一套存储与展示
底座，按 P1 → P2 → P3 分期落地。本文是实施级细节（数据模型、模块边界、
关键算法、验收标准），确认后可直接开工。

---

## 总架构

```
                    ┌────────────────────────────────────────────┐
                    │              Capture Hub (新模块)            │
                    │                                            │
  tun fd 包 tap ────▶│ FlowTracker                                │
  (TunPhy, 仅tun0)   │  - 按五元组聚合连接                          │
                    │  - 提取 ClientHello(sni/client_random)      │
                    │  - 分方向分块写字节流                         │
                    │        ▼                                   │
                    │  SQLite: flows / stream_chunks / tls_keys    │
                    │        ▼                                   │
                    │  DecryptEngine(可选) ──▶ HTTP Parser         │
                    │        ▼                                   │
                    │  展示: CLI(tail/list) / 面板「抓包」卡片       │
                    └────────────────────────────────────────────┘
                       ▲             ▲              ▲
                  tun0 数据泵    proxy socks5     tunnel forward
                  (K1 字节流)    relay(K3 域名)    relay
```

核心原则：**转发路径零阻塞**。所有 tap 通过有界 channel 交给独立的
capture 写线程；channel 满时丢包并计数（抓包永远不能拖垮代理本身）。

---

## 方案一（P1/L2）：原始包 + 连接流抓包入库

### 1.1 数据模型（SQLite，库文件 `~/.teamx/capture.db`）

独立于主库（teamx.db）：抓包数据量大、生命周期短，分库存便于整库删除/
过期清理，也避免撑大主库 WAL。

```sql
-- 连接元数据（列表页/详情页主表）
CREATE TABLE flows (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  started_at    REAL NOT NULL,          -- unix epoch 秒（含小数）
  ended_at      REAL,
  source        TEXT NOT NULL,          -- 'tun0' | 'socks5' | 'forward'
  proto         TEXT NOT NULL DEFAULT 'tcp',
  local_addr    TEXT NOT NULL,
  remote_addr   TEXT NOT NULL,
  remote_host   TEXT,                   -- proxy=CONNECT域名; tun0=SNI反查
  sni           TEXT,                   -- 从 ClientHello 提取
  client_random BLOB,                   -- 32B，关联 tls_keys 的锚点
  bytes_up      INTEGER NOT NULL DEFAULT 0,
  bytes_down    INTEGER NOT NULL DEFAULT 0,
  pkts_up       INTEGER NOT NULL DEFAULT 0,
  pkts_down     INTEGER NOT NULL DEFAULT 0,
  close_reason  TEXT,                   -- 'eof'|'reset'|'timeout'|'active'
  decrypted     INTEGER NOT NULL DEFAULT 0  -- 解密成功后置 1
);
CREATE INDEX idx_flows_started ON flows(started_at DESC);
CREATE INDEX idx_flows_sni     ON flows(sni);

-- 字节流分块（每方向独立序号；解密的最小数据单元）
CREATE TABLE stream_chunks (
  flow_id  INTEGER NOT NULL REFERENCES flows(id) ON DELETE CASCADE,
  dir      INTEGER NOT NULL,            -- 0=up(c→r) 1=down(r→c)
  seq      INTEGER NOT NULL,            -- 流内递增
  ts       REAL NOT NULL,
  data     BLOB NOT NULL,               -- 8~16KB 分块
  PRIMARY KEY (flow_id, dir, seq)
);

-- 原始 IP 包（仅 tun0 模式；供 Wireshark 深挖真实头字段）
CREATE TABLE packets (
  id        INTEGER PRIMARY KEY AUTOINCREMENT,
  ts        REAL NOT NULL,
  iface     TEXT NOT NULL,
  data      BLOB NOT NULL               -- 完整 IPv4 包（LINKTYPE_RAW）
);
CREATE INDEX idx_packets_ts ON packets(ts);

-- TLS 会话密钥（tail SSLKEYLOGFILE 实时导入）
CREATE TABLE tls_keys (
  client_random BLOB PRIMARY KEY,       -- 32B
  label         TEXT NOT NULL,          -- CLIENT_HANDSHAKE_TRAFFIC_SECRET 等
  secret        BLOB NOT NULL,
  cipher        TEXT,                   -- 从 ServerHello 提取（辅助）
  added_at      REAL NOT NULL
);
```

容量治理：
- `PRAGMA journal_mode=WAL`；单库软上限（默认 512MB，`--max-size` 可调），
  触顶后按 flows.started_at 从旧到新删除（含 chunks/packets 级联）。
- `packets` 表默认采样保留（如只存每个流的前 N 个包 + SYN/FIN/close），
  全量包模式需显式 `--full-packets`。
- blob 分块 16KB：SQLite 单行过大会有性能悬崖，16KB 在写入放大和行数间平衡。

### 1.2 Tap 点与接线

| Tap | 位置 | 抓到什么 | 开关 |
|---|---|---|---|
| T1 包 tap | `TunPhy::receive`（入）/ `Tx::consume`（出） | 原始 IPv4 包 → `packets` | `tun0 start --capture-packets` |
| T2 流 tap | `pump_active` 的 Direction A/B | 每连接双向字节流 → `stream_chunks` | `tun0 start --capture` |
| T3 流 tap | proxy 的 socks5 relay 循环收发处 | 同上（域名来自 CONNECT，最可靠） | `proxy start --capture` |
| T4 流 tap | tunnel forward relay 收发处 | 同上 | `tunnel forward --capture` |
| K tail | keylog 文件尾随线程（inotify/FSEvents 或 500ms 轮询） | 密钥行 → `tls_keys` | capture 开启即启用 |

实现要点：
- **T2/T3/T4 共用同一个 `FlowTracker`**：以 `(source, local, remote)` 为
  连接键；首次见到连接时解析前 ~4KB 找 ClientHello（TLS record 0x16，
  handshake type 0x01），提取 SNI（extension 0x0000）+ client_random(32B)
  + 支持的 cipher 列表。
- **ClientHello 解析器**（新 `tls_parse.rs`）：record header(5B) →
  handshake header(4B) → random(32B) → session_id → cipher_suites →
  extensions 遍历找 SNI。纯解析无加密，~200 行。
- **T1 与 T2 的关联**（仅 tun0）：包 tap 按 SYN 五元组开 flow_id；
  smoltcp 侧的 slot 与五元组可映射（`resolve_target` 已有 endpoint）。
  v1 允许两者弱关联（各自记录，UI 并排展示），v2 再严格 join。
- **性能红线**：tap 发送用 `try_send`（bounded channel 8192 条），
  满则 `dropped += 1` 并每 5s 打一条摘要日志；写库在独立线程
  （批量事务：攒 64 chunk 或 50ms flush 一次）。

### 1.3 CLI / API

```
teamx capture start  [--sources tun0,socks5] [--max-size 512MB]
                     [--full-packets] [--out capture.db]
teamx capture stop
teamx capture status
teamx capture list   [--sni <glob>] [--since <ts>] [--limit N] [--json]
                     # 输出 flows 列表，含 🔑 标记（client_random 在
                     # tls_keys 中有匹配 = 可解密）
teamx capture show <flow-id> [--hex]
teamx capture export <flow-id|--all-matching> --format pcap [--with-keylog]
                     # 导出合成 pcap（LINKTYPE_RAW）+ 配套 keylog 文件
teamx capture prune  [--older-than 7d | --max-size 512MB]
```

`export pcap` 的合成规则：从 stream_chunks 反向构造 TCP 段（seq 用虚拟
单调序号，flags 只设 PSH+ACK，窗口填 65535）；若存在 packets 表数据且
flow 能匹配则优先用真实包。keylog 导出为标准格式文本。

### 1.4 面板集成（Swift）

新「抓包」卡片（第 9 个 tab）：
- 顶部：capture 开关（start/stop）+ 状态（已抓 N 连接 / M MB / dropped）
- 中部：flows 实时列表（SNI、目标、↑↓速率、🔑 图标），点击进详情
- 详情页：字节流 hex 视图；若可解密 → 「解密」按钮走方案二引擎
- 设置项：自动清理策略（保留天数）、大小上限

---

## 方案二（P2）：事后选择性解密（SSLKEYLOGFILE 路线）

### 2.1 使用流程（用户视角）

```
一次性引导:
  teamx capture sslkeylog chrome   # 输出启动命令或直接拉起:
  SSLKEYLOGFILE=~/.teamx/sslkeys.log open -a "Google Chrome"
日常:
  teamx capture start --sources socks5
  …正常浏览…
  teamx capture list                # 看到 🔑 可解密标记的连接
  teamx capture decrypt <flow-id>   # → HTTP 明文视图
面板: 抓包卡片 → 勾选连接 → 「解密」→ 请求/响应树形视图
```

### 2.2 DecryptEngine 设计（新模块 `decrypt.rs`）

输入：`stream_chunks`（dir 有序）+ `tls_keys`（client_random 匹配）。
输出：解密后的明文字节流（再交给 HTTP parser）。

**TLS 1.3 解密算法**（主力，现代流量 >95%）：

```
1. 从 keylog 取 CLIENT_HANDSHAKE_TRAFFIC_SECRET / SERVER_HANDSHAKE_TRAFFIC_SECRET
   与 CLIENT_TRAFFIC_SECRET_0 / SERVER_TRAFFIC_SECRET_0
2. 密钥派生（HKDF-SHA256，RFC 8446 §7.1）:
   key = HKDF-Expand-Label(secret, "key", "", key_len)
   iv  = HKDF-Expand-Label(secret, "iv", "", 12)
3. record 解密（AEAD）:
   nonce = static_iv XOR (left_pad_64(record_number))
   aad   = TLSCiphertext.opaque_header (5B)
   成功明文 = payload[:-1]（尾部 content_type 一字节）
4. 密钥切换点：ChangeCipherSpec 兼容包 / handshake 消息结束后
   application secret 替换 handshake secret
5. 重放保护：record_number 单调；解密失败即停止该方向
```

依赖：`ring` 已在依赖里（HKDF/AES-GCM/ChaCha20 全覆盖），无需新增。
预计代码量：`decrypt.rs` ~600 行 + `tls_parse.rs` 扩展 ~200 行
（ServerHello cipher 提取、CCS 识别、alert 处理）。

**TLS 1.2（ECDHE+AES-GCM）**：master_secret 由 keylog 的
`CLIENT_RANDOM` 行提供，key_block 派生按 RFC 5246 §6.1（PRF）。~300 行。
CBC 套件不支持（现网占比 <1%，明确报错「unsupported legacy cipher」）。

**限制（写死在文档与 UI）**：
- 仅覆盖写 keylog 的客户端（Chrome/Edge/Firefox/curl-openssl3/
  Python urllib3≥2.0/Node≥12）；Safari/原生 App/Go ❌
- 会话票据恢复（PSK）连接：keylog 同样有对应行 ✅（Chrome 每连接都导出）
- QUIC/HTTP3 ❌（不经 teamx）

### 2.3 HTTP 解析与展示（`http_view.rs`）

- 解密明文 → HTTP/1.1 增量解析（请求行/状态行/headers/chunked/content-length
  分帧）；body 按 Content-Type 决定展示（text/json/图片预览/hex）
- gzip/br/deflate 解压（`flate2` 新增依赖，或用系统 `gzip` 子进程起步）
- 双向时间轴视图：请求 → 响应配对（按连接内顺序）
- CLI：`teamx capture decrypt <flow-id> [--body-only] [-o file]`
- 面板：树形（请求展开响应），JSON 自动格式化

### 2.4 隐私与合规设计（必须项）

- 解密是**显式动作**：列表默认只显示元数据；解密按钮二次确认
- 解密结果不落盘（内存中渲染）；导出明文需 `--allow-plaintext`
- capture.db 建议整体加密（SQLCipher 或对 blob 做 AES-GCM，
  密钥挂 macOS Keychain）——二期增强
- 产品文案明确：「仅解密本机自己产生的、应用自愿导出密钥的会话」

---

## 方案三（P3）：TLS MITM（全量覆盖，enterprise 专属）

### 3.1 定位

覆盖 keylog 路线无法触及的客户端（Safari、原生 App、任意 CLI）。
代价是需要信任自签 CA + 处理 pinning。**仅 enterprise 提供**
（main 保持无 MITM，规避合规面）。

### 3.2 架构

```
应用 ──TLS(MITM假证书)──▶ CaptureMITM(本地127.0.0.1:P)
                             │ 1. 终止TLS → 明文 → Capture Hub(L1)
                             │ 2. 以真实SNI向出口新建TLS ──▶ exit ──▶ 目标
                             └─ pinning检测: 上游握手失败 → 当前连接切透传
```

两种接入方式：
- **proxy 模式（推荐首做）**：SOCKS5 CONNECT 已带域名（K3 最可靠）。
  relay 收到 CONNECT 后不再直通，而是起本地 TLS 服务端（动态证书）。
- **tun0 模式**：ip_map 反查域名生成证书；DNS 未命中（无 SNI）的连接
  只能透传不解密。

### 3.3 动态证书签发（复用 pki.rs）

```
~/.teamx/capture-ca/ca.crt + ca.key (0600, RSA4096 或 ECDSA P-256)
叶子证书: CN=<domain>, SAN=[<domain>], EKU=serverAuth, 有效期 48h(缓存复用)
签发缓存: LRU 512 张（内存 + 磁盘序列化），避免每次握手都签
```

改动点：`pki.rs` 增加
`ensure_capture_ca(home)`（生成/加载专用 CA）与
`issue_leaf(ca, domain)`（现成 `cert_params` 改造，加缓存层）。
rustls 服务端 accept 需要 `Acceptor`（tokio-rustls 自带）。

CA 信任引导（一次性，GUI 引导）：
```bash
sudo security add-trusted-cert -d -r trustRoot \
  -k /Library/Keychains/System.keychain ~/.teamx/capture-ca/ca.crt
```
面板按钮触发 osascript 授权 + 校验（`security verify-cert`）。
卸载命令同步提供。

### 3.4 pinning 自动降级（关键健壮性设计）

状态机（per connection）：

```
CONNECT/新流 → [mitm 尝试]
   ├─ 客户端 TLS 握手 OK 且上游握手 OK → 解密模式（正常路径）
   ├─ 客户端握手 OK 但上游验证失败/被拒（pinning 特征:
   │   上游 alert=bad_certificate/unknown_ca，或上游 RST in handshake）
   │   → 丢弃已解密内容 → 向客户端重放「透传通道」
   │     （技术上：对该连接放弃 mitm；因客户端握手已消耗，
   │      需要向客户端回 RST 让其重连一次，重连的连接进入透传白名单
   │      —— 以 (domain) 维度记住 N 小时，避免反复断）
   └─ 客户端直接拒绝假证书（也是 pinning 特征）→ 同上白名单透传
```

注意：第一次 pinning 失败会造成该连接一次重连延迟（业界通行做法，
Charles 同款行为）。白名单持久化到 capture 设置，可在 UI 清除。

### 3.5 与出口链路的组合

MITM 后 teamx 变成真正的 HTTP(S) 代理语义：
- 出口请求按明文 HTTP 目标重建（Host/SNI=原域名）
- 路由表照常生效（routes.resolve(domain) 选 exit）
- HTTP 层还能做改写（enterprise 后续能力：header 注入/mocK 响应——另立 TODO）

### 3.6 工作量预估

| 部分 | 估计 |
|---|---|
| ensure_capture_ca + issue_leaf + 缓存 | 0.5d |
| SOCKS5 MITM 模式（TLS server accept + 上游 client + 双泵） | 1.5d |
| pinning 状态机 + 白名单 | 0.5d |
| CA 信任引导（CLI + 面板按钮） | 0.5d |
| HTTP 解析联动 + 面板视图 | 1d |
| 测试（curl/browser/safari/pinning app 矩阵） | 1d |
| 合计 | ≈5d |

---

## 三方案关系与实施顺序

```
P1 (方案一) 存储底座 + pcap/连接级     ← 无侵入，先行
P2 (方案二) keylog 事后解密           ← 依赖 P1 的 flows/chunks/tls_keys
P3 (方案三) TLS MITM                 ← 依赖 P1 存储 + P2 的 http_view
```

- P1 ≈ 3d；P2 ≈ 3d（含 HTTP 解析）；P3 ≈ 5d。
- P1/P2 对 main 也无害（可回迁 main）；P3 仅 enterprise。

## 验收清单（每期完成后跑）

- [ ] P1: Wireshark 打开导出的 pcap 正常解析；高并发下载期间 dropped 计数
      为 0（或有界可控）；capture.db 触顶自动清理生效
- [ ] P2: Chrome 访问 google → 面板勾选 → 看到 HTTP 请求/响应明文；
      Safari 流量正确标注为不可解密；pinning 场景不受影响（本就不解密）
- [ ] P3: Safari HTTPS 流量在面板可见明文；某 pinning App 自动降级透传
      且重连后正常使用；卸载 CA 后系统恢复正常信任链
