# 23 — Enterprise TODO：流量分析（TLS/HTTPS 解密 · 协议识别 · 统计 · 搜索）

> 状态：**TODO（设计定稿，未实施）**
> 关联：`docs/19-todo-capture-system.cn.md`（抓取底座）、
> `docs/22-todo-flow-replay.cn.md`（回放）
> 日期：2026-08-24

## 0. 定位

流量分析 = 把 capture.db 里的原始记录变成**结构化、可搜索、可统计**的
情报层。TLS/HTTPS 解密是本块的第一能力（从 19 号文档 §方案二升级为分析
管线的一个阶段），之上再叠加协议识别、指标聚合与全文检索。

分析管线总览：

```
flows/stream_chunks（密文/明文字节流）
   │
   ▼
[1] Protocol Detector ──▶ 标注 protocol: http/1.1 | h2 | ws | tls | grpc | dns | other
   │
   ▼
[2] Decrypt Engine（TLS over TCP）──▶ 明文子流（依赖 keylog，见 §2）
   │
   ▼
[3] Sub-protocol Parsers ──▶ http_exchanges / ws_frames / grpc_msgs
   │
   ▼
[4] Stats & Search ──▶ 聚合视图 + FTS5 全文索引 + 面板图表
```

---

## 1. 协议识别器（Protocol Detector）

对每条 flow 的 up 方向前 4KB 做指纹判定（在 ClientHello 解析器基础上扩展）：

| 指纹 | 判定 |
|---|---|
| 首 byte 0x16 且 handshake type 0x01 | TLS（继续提取 SNI/random/ciphers） |
| `GET /`、`POST /`、`HTTP/1.` 等请求行 | HTTP/1.1 明文 |
| 首帧 magic `PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n` | h2（gRPC 通常跑在其上） |
| `GET … Upgrade: websocket` | WebSocket（明文 upgrade；wss 走 TLS 后） |
| 目标端口 53 且报文格式匹配 | DNS |

产出写入 `flows.protocol`；识别不出 → `other`（仍可 hex 查看）。
实现为纯函数 `detect_protocol(first_bytes) -> Protocol`，~150 行。

## 2. TLS/HTTPS 解密管线（核心）

### 2.1 与 19 号文档方案二的关系

19 号文档的 DecryptEngine 是"单条 flow 的解密器"；本节把它纳入分析管线并
补齐批量/增量维度：

- **批量模式**：`teamx analyze decrypt [--session id] [--sni glob]` 对选中
  flows 批量跑引擎，结果落 `http_exchanges` + `plaintext_fts`。
- **增量模式**：capture 进行中，连接 close 即自动尝试解密（keylog 已有
  匹配时），面板实时出明文——用户无感。

### 2.2 引擎要点（沿用 19 号文档 §方案二 §2.2 的算法）

- TLS 1.3：keylog 四类 secret + HKDF-Expand-Label + AEAD
  （nonce = static_iv XOR seq）；handshake→application 密钥切换点处理
- TLS 1.2：CLIENT_RANDOM 行 → master_secret → RFC5246 key_block
- 输出按方向重组为**应用层消息流**，交给子协议解析器
- 失败分类落库（`decrypt_error`）：no-keys / bad-record / unsupported-cipher，
  UI 据此展示原因而非笼统"失败"

### 2.3 产出表

```sql
CREATE TABLE http_exchanges (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  flow_id INTEGER NOT NULL REFERENCES flows(id) ON DELETE CASCADE,
  seq INTEGER NOT NULL,             -- 连接内第几个 exchange（h2 用 stream_id）
  method TEXT, url TEXT, status INTEGER,
  req_headers TEXT,                 -- JSON
  res_headers TEXT,
  req_body_ref TEXT,                -- 指向 body_chunks 或内联小 blob
  res_body_ref TEXT,
  content_type TEXT,
  req_start REAL, res_end REAL,     -- 计时（延迟/耗时计算）
  UNIQUE (flow_id, seq)
);
CREATE INDEX idx_ex_flow ON http_exchanges(flow_id);
CREATE INDEX idx_ex_url ON http_exchanges(url);

-- WebSocket 帧 / gRPC 消息（二期）
CREATE TABLE ws_frames ( flow_id INTEGER, dir INTEGER, ts REAL,
                         opcode INTEGER, data BLOB );
CREATE TABLE grpc_msgs ( flow_id INTEGER, dir INTEGER, ts REAL,
                         path TEXT, data BLOB );

-- 解密后明文全文索引（SQLite FTS5 内置）
CREATE VIRTUAL TABLE plaintext_fts USING fts5(
  flow_id UNINDEXED, dir UNINDEXED, content
);
```

body 存储策略：<64KB 内联 BLOB；≥64KB 写 `body_chunks(flow_id, seq, data)`
分块；>2MB 截断并标记 `truncated`。

## 3. 统计引擎（Stats）

全部由 SQL 聚合产出（免自研），新增几个物化视图表供面板快速读取：

```sql
-- 按 host 维度（SNI 优先，缺则 IP）
CREATE TABLE stats_by_host (
  day TEXT, host TEXT, conns INTEGER, bytes_up INTEGER, bytes_down INTEGER,
  requests INTEGER, errors INTEGER, p50_ms REAL, p95_ms REAL, p99_ms REAL,
  PRIMARY KEY (day, host)
);
-- 按 exit 维度（哪个出口扛了多少流量）
CREATE TABLE stats_by_exit ( day TEXT, exit TEXT, 同上字段 );
-- 时序（每分钟桶）用于折线图
CREATE TABLE stats_timeline (
  bucket INTEGER,            -- epoch/60
  conns INTEGER, bytes_up INTEGER, bytes_down INTEGER,
  requests INTEGER, errors INTEGER
);
```

- **延迟分布**：来自 `http_exchanges.req_start/res_end`；TCP 层延迟用
  metrics.rs 的 RTT 近似（标注来源不同）。
- CLI：

```
teamx analyze stats --by host|exit|proto [--since 24h] [--top 20]
teamx analyze timeline [--bucket 1m|1h]
```

- 面板：「分析」卡片 —— Top 表格 + 折线图（Swift Charts 或轻量自绘）。

## 4. 搜索与过滤（Search & Filter）

- **全文**：`SELECT flow_id FROM plaintext_fts WHERE plaintext_fts MATCH
  '"Bearer token"'` —— FTS5 开箱即用；高亮用 snippet()。
- **过滤语法**（CLI 与面板共用解析器）：

```
sni:*.google.com  status:>=500  bytes:>10KB  proto:h2
method:POST  since:-1h  has:keys      # 有密钥可解密
free-text                             # 走 FTS5
```

- 实现为 `analyze/filter.rs` 的小型 parser（~200 行），输出 SQL WHERE。

## 5. 安全观察项（轻量，二期）

- 新出现 SNI 提醒（基线 = 首日集合）
- 证书观察：ServerHello 后 Certificate message 可从明文握手段提取
  （TLS1.3 证书已加密，需先解密 handshake 才可见——依赖解密管线）
- 大上传检测（单 flow bytes_up > 阈值提醒）

## 6. 面板「分析」卡片（Swift）

三个子页：
1. **总览**：今日流量卡片（conns/bytes/requests/errors）+ 时序折线 +
   Top10 host 表
2. **连接明细**：可过滤的 flows/exchanges 双栏（左列表右详情）+ 搜索框
3. **解密视图**：选中有 🔑 的 exchange → 请求/响应树形展示（复用回放
   卡片的 diff 组件风格）

CLI 一等公民同步提供（headless 场景可用）。

## 7. 实施顺序与工作量

| 步骤 | 内容 | 估计 |
|---|---|---|
| A1 | Protocol Detector + flows.protocol 回填 | 0.5d |
| A2 | DecryptEngine 批量模式（复用 19 号文档设计）+ 失败分类 | 2d |
| A3 | HTTP/1.1 解析器 + http_exchanges 落库 + body 分块 | 1d |
| A4 | FTS5 接入 + 过滤语法 parser + `teamx analyze search/list` | 1d |
| A5 | 统计视图表 + CLI stats/timeline | 1d |
| A6 | 面板「分析」卡片三子页 | 2d |
| A7 | HTTP/2 + gRPC 解析（可选增强） | 2d |
| A8 | WS 帧解析（可选增强） | 0.5d |

前置：P1 抓取底座（19 号文档）必须先行；A2 依赖 keylog 引导流程。

## 8. 验收标准

- [ ] Chrome 会话抓取后一键批理解密，http_exchanges 覆盖 ≥95% 的 TLS 流
      （其余正确标注 no-keys/bad-record 原因）
- [ ] FTS5 搜 `"authorization"` 在 100MB capture.db 中 <200ms 返回
- [ ] 过滤语法组合查询（sni+status+bytes）结果正确
- [ ] stats 三张视图与手工 SQL 抽查一致；p95 计算合理
- [ ] 面板三子页在 10 万 chunk 规模下滚动流畅（虚拟列表/分页）
