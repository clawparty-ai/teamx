# 测试计划 — 流量产品线（三项改进 + 抓包/回放/分析验收）

> 本计划分两部分：
> - **Part A**：tun0 三项改进端到端验证 —— **当前即可执行**
>   （待测提交 main `15d5f01`，已打包 dist/Teamx.app）
> - **Part B/C/D**：抓包 / 回放 / 分析的验收测试 —— 对应功能在 enterprise
>   分支实施完成后执行（19/22/23 号 TODO 文档的配套验收标准已并入本计划，
>   实施时按此逐项打勾）
>
> 环境：macOS 本机；hub03(server:8888) + egress2(出口) 在线
> 初始系统 DNS 应为：`114.114.114.114`（你手动设置的原值）

## 图例
- 🤖 = 我来自动执行验证
- 👤 = 需要你在终端/面板操作

---

## T0 · 环境准备 👤🤖

```bash
# 你执行（清理 + 起始态确认）:
sudo pkill -9 -f "teamx tun0 start"     # 杀掉所有 tun0（含 root）
pkill -f TeamxApp                        # 停掉面板 app（避免它拉起 tun0）
rm -f ~/.teamx/dns-backup.json           # 清掉可能的残留备份
networksetup -getdnsservers Wi-Fi        # 我来核对：应为 114.114.114.114
```

✅ 通过标准：无 teamx 进程；无备份文件；DNS=114.114.114.114

---

## T1 · 基础回归（透明代理主链路）👤→🤖

👤 面板点「启动 tun0」（或命令行 `sudo ... teamx tun0 start &`）

🤖 自动验证：
| # | 命令 | 预期 |
|---|---|---|
| 1 | `networksetup -getdnsservers Wi-Fi` | `127.0.0.1` + `114.114.114.114` |
| 2 | `dig +short www.google.com A` | 142.251.x（真实 IP，非污染） |
| 3 | `curl -skI https://www.google.com/generate_204` | HTTP/2 204 |
| 4 | `curl -sI https://www.baidu.com` | 200（直连不受影响） |
| 5 | `/tmp` 下无新日志；`~/.teamx/tun0.log` 存在且含 `ok dns-proxy` | 日志新路径生效 |

---

## T2 · AsyncFd — 空闲 CPU 占用 🤖

前提：T1 通过后静置 30 秒（无任何浏览/下载）。

```bash
# 采样 3 次，每次间隔 2 秒
for i in 1 2 3; do ps -o %cpu= -p $(pgrep -f "teamx tun0 start" | head -1); sleep 2; done
```

✅ 通过标准：三次采样均 **< 1.0%**（改造前 busy-poll 典型 0.5~2%）
补充对照：跑一次 curl 下载期间采样，CPU 可以上升（说明事件驱动没有误伤吞吐）。

---

## T3 · Bridge 异步化 — 多连接不卡顿 🤖

前提：T1 状态不变。

```bash
# 同时发起 8 个新 TLS 连接（不同域名，逼出多个并发建桥）
for d in www.google.com mail.google.com drive.google.com \
         news.google.com translate.google.com photos.google.com \
         docs.google.com scholar.google.com; do
  curl -sk --max-time 20 -o /dev/null -w "%{http_code} $d\n" "https://$d" &
done; wait
```

🤖 观察项：
- 8 个请求全部返回（200/301/302 均算成功），总耗时应明显小于串行累加
- `~/.teamx/tun0.log` 中 `bridge up` 行交错出现（而非阻塞式依次完成）

✅ 通过标准：全部成功；期间用另一终端跑 `curl baidu.com` 不被卡顿（数据泵持续工作）

---

## T4 · Watchdog — kill -9 自愈链路 👤→🤖

这是三项改进里最关键的故障路径验证：

```bash
# 你执行（模拟异常死亡）:
sudo kill -9 $(pgrep -f "teamx tun0 start" | head -1)
```

🤖 立即验证（阶段 A — 死亡现场）：
| 检查 | 预期 |
|---|---|
| 进程 | 无 tun0 进程 |
| 系统 DNS | **仍是** `127.0.0.1` + `114...`（kill -9 来不及还原，符合预期） |
| 备份文件 | `~/.teamx/dns-backup.json` 存在 |

👤 再次面板「启动 tun0」

🤖 验证（阶段 B — 启动自愈）：
| # | 检查 | 预期 |
|---|---|---|
| 1 | 日志首行区域含 `ok watchdog: restoring leftover DNS backup` | 自愈触发 |
| 2 | 设置后 DNS = `127.0.0.1` + `114...`（干净叠加，无重复条目） | ✅ |
| 3 | `curl google 204` 复测通过 | 功能完好 |

---

## T5 · Watchdog — 优雅退出立即还原 👤→🤖

👤 面板点「停止 tun0」（内部走 `teamx tun0 stop` + SIGTERM）

🤖 立即验证：
| # | 检查 | 预期 |
|---|---|---|
| 1 | `networksetup -getdnsservers Wi-Fi` | **仅 `114.114.114.114`**（127.0.0.1 已移除） |
| 2 | `dig +short www.baidu.com`（用系统 DNS） | 正常解析（网络可用） |
| 3 | 备份文件已删除 | `ls ~/.teamx/dns-backup.json` → No such file |

---

## T6 · Watchdog — 自毁保护（可选，耗时 ~2 分钟）⏱️

模拟「server 不可达」：对 hub03 加 pf 防火墙规则阻断出站，然后连续查 5 个
不同拦截域名，观察 dns-proxy 计数递增并在第 5 次后自毁还原。

👤 执行阻断：`echo "block drop out quick on en0 to 81.70.41.108" | sudo pfctl -f -`
🤖 循环 dig 5 个域名 → 观察 `/~/.teamx/tun0.log` 出现
   `dns-proxy: resolve ... failed (n/5)` 且最终 `restoring system DNS and aborting`
👤 解除阻断：`sudo pfctl -F rules`
🤖 确认 DNS 已还原为 114...

⚠️ 此项每轮 RPC 超时最长 15s，全程约 2~3 分钟；可跳过（逻辑已被
code review 覆盖），建议至少做一次。

---

## T7 · 循环稳定性（快速启停 3 轮）👤🤖

👤×3：面板「启动」→ 等 5 秒 → 「停止」
🤖 每轮停止后核对：DNS=114...、无残留备份、无多余 utun 接口
✅ 三轮后状态与初始一致。

---

## 通过标准汇总

| 级别 | 标准 |
|---|---|
| 必须全过 | T1、T4、T5（核心功能 + 故障自愈主路径） |
| 必须全过 | T2（CPU 数字达标）、T3（无卡顿） |
| 建议 | T7；T6 可选 |

## 已知不在本轮范围

- kill -9 场景的独立 launchd 守护（watchdog 二期，见 docs/17 §W-B）
- Linux/Windows 平台行为（见 19 号文档跨平台 TODO）
- QUIC/HTTP3 流量（三条路径均不转发 UDP）


---

# Part B · 抓包系统验收（enterprise P1 实施后执行）

> 对应设计：docs/19-todo-capture-system.cn.md
> 前置：capture.db 初始化正常；`teamx capture start/status/stop` 可用

## B1 · Tap 点与数据完整性 🤖

| # | 步骤 | 预期 |
|---|---|---|
| B1.1 | `tun0 start --capture` 后 `curl https://www.google.com` | `flows` 新增一条：source=tun0、sni=www.google.com、client_random=32B 非零 |
| B1.2 | `proxy start --capture` 后 `curl --socks5-hostname 127.0.0.1:1080 https://www.google.com` | source=socks5、remote_host=www.google.com（CONNECT 域名最可靠）|
| B1.3 | 字节数核对：curl 下载固定大小文件（如 1MB），flows.bytes_up/down 与实际相符（±TLS overhead 容差） | 误差 <15% |
| B1.4 | stream_chunks 双方向均有分块且 seq 连续；单块 ≤16KB | ✅ |

## B2 · pcap 导出与生态兼容 🤖

| # | 步骤 | 预期 |
|---|---|---|
| B2.1 | `teamx capture export <flow-id> --format pcap -o /tmp/t.pcap` | 文件生成，magic 为 pcap（d4 c3 b2 a1 或反序）|
| B2.2 | `tshark -r /tmp/t.pcap` | 能解析出 TCP 会话与 TLS ClientHello（含 SNI）|
| B2.3 | `--with-keylog` 同时导出 keylog；`tshark -r t.pcap -o tls.keylog_file:key.log` | TLS 应用数据帧解密可见 HTTP 明文 |

## B3 · Keylog 关联与「可解密」标记 🤖

```bash
SSLKEYLOGFILE=~/.teamx/sslkeys.log open -a "Google Chrome"
# 浏览器访问若干站点后：
teamx capture list
```

✅ 通过标准：Chrome 发起的 flows 显示 🔑（client_random 在 tls_keys 有匹配）；
Safari/原生 App 流量无 🔑（keylog 不支持属预期，不是缺陷）。

## B4 · 容量治理与丢弃保护 🤖

| # | 步骤 | 预期 |
|---|---|---|
| B4.1 | 大文件下载至 capture.db 触及 --max-size | 触顶后自动从最老 flow 清理；新写入继续成功 |
| B4.2 | 高并发（ab/curl x32 并行）压测期间观察 status | dropped 计数有界（<总量 1%）；代理转发不受影响 |
| B4.3 | 重启 tun0 再查 | capture 会话恢复配置；旧数据保留 |

## B5 · 性能隔离（tap 不得拖垮转发）🤖

- 对照测速（fast.com 或 curl 10MB 文件）：关闭抓包 vs 开启抓包各 3 次
✅ 通过标准：吞吐损耗 <10%；p95 延迟增加 <20ms

## B6 · 隐私默认值 🤖

- 列表视图仅元数据（SNI/IP/字节数），任何明文不出现
- 「解密」按钮需显式点击（Part D 验收联动）
- capture.db 权限 0600

---

# Part C · 回放功能验收（enterprise R1-R5 实施后执行）

> 对应设计：docs/22-todo-flow-replay.cn.md

## C1 · TCP Client Replay 🤖

| # | 步骤 | 预期 |
|---|---|---|
| C1.1 | 录制一次 `curl https://httpbin.org/get`（经抓包），然后 `teamx replay tcp start <flow-id>` | 目标侧收到相同 up 字节流（httpbin 返回同样参数的 JSON）|
| C1.2 | `--speed 0.5` 回放录制时长 4s 的会话 | 总耗时 ≈ 8s（±30%）|
| C1.3 | `--to` 目标改写到本地 nc 监听 | nc 收到的字节流与 chunks(dir=up) 一致 |

## C2 · Mock Serve 模式 👤🤖

| # | 步骤 | 预期 |
|---|---|---|
| C2.1 | `teamx replay serve <flow-id> --port 18080` 后浏览器/curl 连 127.0.0.1:18080 | 收到录制的 down 响应字节流 |
| C2.2 | 三种匹配策略分别验证（strict 全等 / prefix / first-bytes） | 不匹配请求按策略拒绝或超时 |

## C3 · Session 编排 🤖

- 录制含 20 条 flows 的会话 → `replay session --concurrency 4`
✅ 全部完成、无死锁；`--preserve-order` 时开始时间严格递增；
报告 JSON 含每条 result

## C4 · HTTP 回放 🤖

| # | 步骤 | 预期 |
|---|---|---|
| C4.1 | Chrome 导出 HAR → `replay import har` | exchanges 数量与 HAR entries 一致 |
| C4.2 | `replay to-curl <id>` 输出的命令直接终端执行 | 响应与会话录制一致（status/body）|
| C4.3 | 单条 send + `--var token=xxx` + `--map api.prod.com=api.staging.local` | 请求打到 staging 且 header 已替换 |
| C4.4 | session run + replay.toml 断言（status/latency/body-contains） | HTML 报告生成；失败项非零退出码 |
| C4.5 | Authorization/Cookie 在报告/视图中脱敏显示 | 默认打码，`--show-secret` 才显示 |

## C5 · 自举闭环 🤖

回放产生的流量本身开启抓包 → 新 capture 会话能看到这批 flows。
✅ 抓包↔回放互不干扰且可组合。

---

# Part D · 分析功能验收（enterprise A1-A6 实施后执行）

> 对应设计：docs/23-todo-flow-analysis.cn.md

## D1 · 协议识别 🤖

抽样核对 flows.protocol 标注：

| 流量 | 期望 protocol |
|---|---|
| curl http://（明文） | http/1.1 |
| curl https:// | tls（解密后升级 http/1.1 或 h2）|
| Chrome 现代站点 | h2（ALPN 协商）|
| wss:// echo 服务 | ws |
| dig 出站（若被 capture）| dns |

## D2 · 解密管线 🤖

| # | 步骤 | 预期 |
|---|---|---|
| D2.1 | Chrome 会话（keylog 开启）→ `teamx analyze decrypt --session` | http_exchanges 覆盖 ≥95% TLS 流 |
| D2.2 | 未开 keylog 的流量 | decrypt_error=no-keys（原因分类落库，UI 显示原因）|
| D2.3 | pinning 站点（如某银行 API）| bad-record 分类；不影响其他连接 |
| D2.4 | TLS1.2 与 TLS1.3 站点各一 | 两者均解密成功（两套密钥路径）|

## D3 · HTTP 解析正确性 🤖

- http_exchanges 的 method/url/status/headers 与浏览器 DevTools Network
  面板逐项比对（≥3 个不同站点）
- gzip/br body 正确解压展示；JSON 自动格式化
- 计时字段合理（req_start < res_end；多次请求延迟与体感一致）

## D4 · FTS5 全文搜索 🤖

```sql
SELECT flow_id FROM plaintext_fts WHERE plaintext_fts MATCH 'authorization';
```
✅ 命中包含该词的 flows；100MB 级库查询 <200ms；snippet() 高亮正确

## D5 · 过滤语法 🤖

组合用例（结果人工抽查 3 条核对）：
```
sni:*.google.com status:>=500 bytes:>10KB
method:POST since:-1h has:keys
```
✅ 解析器正确转 SQL；非法语法给出友好报错

## D6 · 统计引擎 🤖

- stats_by_host 与手工 SQL 聚合抽查一致
- p50/p95/p99 与原始 exchange 延迟分布吻合
- timeline 分钟桶求和 = 当日总量

## D7 · 面板大数据量 🤖

灌入 ~10 万 chunks 的库后：连接明细滚动流畅（虚拟化/分页）、搜索交互
不卡主线程。

---

# 执行顺序建议

```
现在        : Part A（三项改进）→ 通过后 push main
P1 实施后    : Part B（抓包）
R 步骤后     : Part C（回放）
A 步骤后     : Part D（分析）
每个 Part 通过: 更新本文档勾选状态 + CHANGELOG 记录
```
