# 22 — Enterprise TODO：流量回放（TCP 回放 + HTTP 回放）

> 状态：**TODO（设计定稿，未实施）**
> 关联：`docs/19-todo-capture-system.cn.md`（抓取底座）、
> `docs/23-todo-flow-analysis.cn.md`（流量分析）
> 日期：2026-08-24

## 0. 定位与数据源

回放 = 「把录下来的流量再跑一遍」。与抓取（P1）、分析（P2）共用同一
存储底座 `~/.teamx/capture.db`（flows / stream_chunks / http_exchanges /
packets 四张表，见 19 号文档 §1.1）。

两条回放主线：

| 主线 | 数据源 | 用途 |
|---|---|---|
| **TCP 回放** | `flows` + `stream_chunks`（字节流级）；`packets`（包级，二期） | 故障复现、服务端联调、性能压测素材 |
| **HTTP 回放** | `http_exchanges`（已解密的请求/响应对） | API 回归测试、接口调试、Mock 服务 |

---

## 1. TCP 回放

### 1.1 三种模式

#### 模式 A — 客户端重放（Client Replay，核心模式）

把录制流的 **up 方向字节流** 作为客户端输入，重新向目标发起 TCP 连接并
按时序发送；down 方向（原响应）保留用于对比。

```
teamx replay tcp start <flow-id|--session <id>>
        [--to host:port]            # 目标改写（默认按录制时的 remote_host）
        [--speed 1.0|fast]          # 时序回放速率（1=原速；fast=尽快）
        [--timeout 30s]
        [--diff]                    # 录制响应 vs 回放响应逐块 diff
```

实现要点：
- 复用现有出口通道：`open_tunnel_bridge(server_url, exit, target)` 建立
  到目标的桥，然后把 `stream_chunks(dir=up)` 按 `(ts[i]-ts[0]) / speed`
  调度写入 bridge.tx。
- **分块调度器**：`tokio::select!` 定时器驱动；chunk 间隔 < 1ms 合批发送。
- down 方向收到的字节实时存入 `replay_runs` 表（见 1.3），同时可选做
  与原 down 流的前缀 diff（LCS 或简单 chunk hash 对比，v1 用后者）。
- 结束条件：up 发完 && (对端 FIN || idle 5s)；输出摘要（总字节、耗时、
  响应前缀匹配率）。

#### 模式 B — Mock 服务端回放（Server Replay）

把录制流的 **down 方向** 变成 Mock 响应脚本，供客户端连本地 Mock 口：

```
teamx replay serve <flow-id|--session <id>> [--port 18080]
        [--match strict|prefix|first-bytes]     # 请求匹配策略
```

- 本地起 TCP listener；客户端连入后读取其 up 字节流，与录制的 up 流做
  匹配（strict=全等；prefix=前缀 N 字节；first-bytes=首个请求行/记录头），
  命中则回放对应 down 字节流。
- 用途：客户端开发脱离真实后端；弱网/错误注入（`--inject delay 200ms`
  / `--inject reset-after 100B`）后续版本再加。

#### 模式 C — 包级回放（Packet Replay，二期，低优先级）

`packets` 表原始 IP 包经 tun 注入或 raw socket 发送。受 TCP 状态机约束
（seq/ack 与对端不符），需地址重写与流控抑制（tcpreplay-edit 同款问题），
对本产品核心场景增益有限 → **降级为诊断工具，暂缓**。

### 1.2 会话级编排

一条 flow 是一次连接；一个 **session**（同一次 capture 里的全部 flows）
可以整体回放：

```
teamx replay session <capture-session-id> [--mode client|serve]
        [--concurrency 4] [--preserve-order] [--report report.json]
```

- 并发模型：默认按录制时间窗并行（同 100ms 窗内的 flows 同时开跑）；
  `--preserve-order` 则严格串行。
- 全局限速：`--speed` 作用于会话时间轴而非单流。
- 报告：每个 flow 一行（成功/失败/响应匹配率/耗时），JSON + 表格双输出。

### 1.3 数据模型增量

```sql
-- 回放运行记录
CREATE TABLE replay_runs (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  mode         TEXT NOT NULL,          -- 'client' | 'serve' | 'session'
  source       TEXT NOT NULL,          -- flow-id 或 session-id
  target       TEXT,                   -- client 模式的实际目标
  speed        REAL NOT NULL DEFAULT 1.0,
  started_at   REAL NOT NULL,
  ended_at     REAL,
  result       TEXT                    -- JSON 摘要（成功数/失败数/diff 概览）
);

-- 单条 flow 的回放结果
CREATE TABLE replay_flow_results (
  run_id      INTEGER NOT NULL REFERENCES replay_runs(id) ON DELETE CASCADE,
  flow_id     INTEGER NOT NULL,
  status      TEXT NOT NULL,           -- 'ok' | 'connect-fail' | 'timeout' | 'diff'
  match_ratio REAL,                    -- down 前缀匹配率 0~1
  error       TEXT,
  duration_ms INTEGER
);
```

### 1.4 验收标准

- [ ] 对一次真实 HTTPS 会话（经 P2 解密的 google 搜索）做 client 回放，
      up 字节流完整送达目标（以 TLS alert/close 验证对端处理）
- [ ] mock 模式：浏览器代理指向 mock 口，命中录制的响应
- [ ] session 回放 20 条 flows 并发 ≤4 无死锁；报告字段齐全
- [ ] 回放期间主抓包功能不受影响（tap 隔离）

---

## 2. HTTP 回放

前置：flows 已被解密并产出 `http_exchanges`（依赖 23 号文档的解密管线，
或手动导入 HAR）。

### 2.1 核心抽象：HttpExchange ↔ HAR 互转

采用 **HAR 1.2**（HTTP Archive）为交换格式，与 Chrome DevTools /
Charles / mitmproxy 生态互通：

```
teamx replay export har <session-id> -o session.har
teamx replay import har session.har      # 写入 http_exchanges（新建虚拟 session）
teamx replay to-curl <exchange-id>       # 打印等价 curl 命令（调试利器）
```

内部模型（复用 23 号文档的 http_exchanges 表 + 新增 session 概念）：

```sql
ALTER TABLE ... ADD COLUMN session_id INTEGER;  -- 属于哪个可回放集合
CREATE TABLE replay_sessions (
  id INTEGER PRIMARY KEY, name TEXT, created_at REAL,
  variables TEXT                          -- JSON: {{token}} 等变量定义
);
```

### 2.2 单条重放（交互式调试）

```
teamx replay http send <exchange-id>
        [--var token=xxx ...]           # 变量替换
        [--map from=to ...]             # 目标改写 api.prod.com=api.staging
        [-o out.json]
```

- 请求重建：method/url/headers/body 原样（变量先替换）；自动补
  `Host`/`Content-Length`；`Accept-Encoding` 默认置 identity 以便 diff。
- 出口选择：走 routes.resolve(url.host)，与正常代理一致。
- 响应与原响应 diff：状态码 / headers 键集 / body（JSON 做 semantic diff，
  其他做字节 diff）→ 存 `replay_flow_results`。

### 2.3 批量回归（Session Replay as Test）

```
teamx replay http run <session-name> [--config replay.toml]
```

`replay.toml` 示例：

```toml
[target]
map = { "api.prod.com" = "http://127.0.0.1:8080" }
[vars]
token = "{{env TEAMX_TEST_TOKEN}}"
[assert]
default_status_ok = true         # 2xx 为过
max_latency_ms = 3000
[[assert.custom]]
exchange_sni = "api.prod.com"
body_contains = '"code":0'
[report]
out = "replay-report.html"
```

- 断言引擎：内置 status/latency/body-contains/header-exists；表达式 v2 再扩。
- 报告：HTML（每 exchange 的 diff 详情）+ JSON；非零失败退出码 → 可接 CI。
- 敏感头默认脱敏展示（Authorization/Cookie 打码），`--show-secret` 关闭。

### 2.4 面板集成（Swift）

「回放」卡片（第 10 个 tab）：
- 左列：session 列表 + exchange 树（方法/URL/状态徽章）
- 右侧：单条详情 + 「编辑参数」「Send」；底部结果 diff 双栏视图
- 顶部工具条：导入 HAR / 导出 HAR / 运行整组（进度条 + 结果汇总）

### 2.5 验收标准

- [ ] Chrome 导出的 HAR 导入后可逐条 Send，目标映射生效
- [ ] 录制的 google 搜索会话（经解密）能整组重放且报告正确
- [ ] `to-curl` 输出可直接在终端执行并得到一致响应
- [ ] 变量替换 + 断言 + HTML 报告全链路可用
- [ ] 回放流量本身可再次被抓包（自举验证）

---

## 3. 实施顺序与工作量

| 步骤 | 内容 | 估计 |
|---|---|---|
| R1 | replay_runs/results 表 + TCP client replay（单 flow） | 1d |
| R2 | TCP mock serve 模式 + session 编排 | 1d |
| R3 | HttpExchange/HAR 互转 + 单条 HTTP send + to-curl | 1d |
| R4 | session 回归（toml 配置 + 断言 + HTML 报告） | 1.5d |
| R5 | 面板「回放」卡片 | 1.5d |

前置依赖：P1（抓取底座）必须先落地；HTTP 回放另依赖 P2 的解密产出
（或 HAR 导入绕过）。
