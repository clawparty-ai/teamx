# 17 — tun0 改进设计：Watchdog / AsyncFd / Bridge 异步化

> 状态：**设计提案（待确认后实施）**
> 关联：`docs/09-design-tun0.cn.md`、`docs/15-design-transparent-proxy.en.md`、`docs/16-code-review-transparent-proxy.en.md`
> 日期：2026-08-24

本文分析 code review 中标记为「接受/暂缓」的三个结构性问题，给出根因、
影响量化、候选方案对比与推荐，**确认后再实施**。

---

## 问题 1 — Watchdog：tun0 死亡后的 DNS 自愈

### 现状与风险

`tun0 start` 启动时把系统 DNS 设为 `[127.0.0.1, <原DNS>]`：

- tun0 正常运行：本地 DNS 代理应答（拦截域名经出口解析）。
- tun0 **正常退出**（面板停止 / `teamx tun0 stop`）：`restore_system_dns()`
  从 `~/.teamx/dns-backup.json` 还原 → 无问题。
- tun0 **异常死亡**（崩溃、被 `kill -9`、系统强杀）：无人还原 DNS。此后：
  - 每次域名解析先等 `127.0.0.1` 超时（macOS mDNSResponder 对无响应
    DNS 服务器的惩罚窗口约 3~5s），再落到第二 DNS。
  - 结果不是"完全断网"，而是**所有解析慢 3~5 秒**；部分对超时敏感的
    应用（硬编码短超时的 CLI 工具）会直接失败。
  - 更坏的情形：进程"半死"（活着但主循环卡死），本地代理端口仍在但不
    应答，且 watchdog 若只探测"进程存在"会误判正常。

### 目标

tun0 异常死亡后 **≤10s 内**自动恢复系统 DNS；正常运行时不产生误恢复。

### 方案对比

| 方案 | 做法 | 优点 | 缺点 |
|---|---|---|---|
| W-A 内嵌看门狗线程 | tun0 进程内起一个线程：持有 DNS 备份副本；主循环每秒喂狗；线程发现 2s 未喂狗 → 以 root 身份调 restore_system_dns + 删备份 | 自包含，无需额外组件 | 只覆盖进程死亡；无法处理"卡死但活着"（除非喂狗点放在主循环内——正是本方案）；kill -9 时看门狗线程也死，**失效** |
| W-B launchd 独立守护 | 启动 tun0 的同时注册一次性 LaunchAgent（`teamx-dns-watchdog`），它轮询 `pgrep teamx tun0`；进程消失且备份文件存在 → 还原 DNS 后自我卸载 | 与 tun0 进程解耦，kill -9 也生效；macOS 原生机制 | 多一个常驻组件；需要安装/清理逻辑；Linux 需另做 systemd-run 版本 |
| W-C 心跳文件探测 | tun0 每秒 touch `~/.teamx/tun0.alive`；一个轻量常驻 watcher（可复用 GUI app）检测心跳过期 → 提示用户或自动还原 | 实现简单 | 依赖第三方（GUI）存活才有自动恢复；纯 CLI 场景退化为提示 |
| W-D 启动即自愈（推荐组合） | (1) 每次 `tun0 start` / `dns_proxy spawn` 前，检测"备份文件存在但 pgrep 无 tun0 进程"→ 先还原残留备份；(2) W-B 的 launchd 守护做成可选 `--watchdog` | 覆盖最常见场景（上次崩溃的残留）；实现小 | 不覆盖运行中卡死 |

### 推荐

**W-D + W-B 组合**，分两期：
- 一期（小改动，~80 行）：W-D 启动自愈 + 把 `restore_system_dns()` 注册到
  进程信号处理（SIGTERM/SIGINT 时还原再退出），覆盖正常与大部分异常路径；
  另外给 dns_proxy 加"连续 N 次 resolve 失败 → 自动还原 DNS 并退出"的自毁保护。
- 二期（可选）：W-B 独立 LaunchAgent 守护，覆盖 `kill -9`。

---

## 问题 2 — AsyncFd：消除主循环 2ms 忙轮询

### 现状与影响

主循环结构：

```rust
loop {
    stack.poll();                       // 读 tun fd + 驱动 smoltcp + UDP DNS
    /* take_new_connection / pump_active */
    tokio::time::sleep(2ms).await;      // 让出调度器
}
```

- 空闲时 CPU 占用实测约 0.5~2%（笔记本续航/发热有感知）。
- 流量大时反而没问题（poll 本来就要跑）；**空闲时是纯浪费**。
- 不能简单换回同步 `phy::wait(fd, timeout)`：它会阻塞 current_thread
  runtime，饿死同一线程上的 bridge spawn 任务（已踩过坑）。

### 根因

smoltcp 的 `Interface::poll` 需要"有机会就跑"，但我们只想在 **tun fd 可读**
或 **定时器到期**（重传/TIME_WAIT 等 maintenance）时才跑。需要一个能同时等
fd 可读 + 定时的异步原语，并且不阻塞 runtime。

### 方案对比

| 方案 | 做法 | 优点 | 缺点 |
|---|---|---|---|
| A1 tokio AsyncFd（推荐） | `AsyncFd::new(std::fs::File)` 包住 tun fd；主循环 `tokio::select! { _ = asyncfd.readable(), _ = sleep(next_timer) }` 再 poll | 事件驱动，空闲 0% CPU；保留单线程模型；改动集中在 run_tun_proxy 主循环（~40 行） | fd 需要非阻塞（已是）；readable 是水平触发需配合 `clear_ready()`；需要小心 poll 后立即再读空的情况 |
| A2 分离读线程 + channel | std::thread 阻塞 read tun fd，包通过 unbounded channel 发给主循环 | 简单直观 | 每包多一次跨线程拷贝；TunPhy 结构大改（phy 直接读 fd 的假设被破坏）；高吞吐下 channel 反压难做 |
| A3 降频轮询 | sleep 从 2ms 动态调整（空闲 50ms，有流量回 2ms） | 最小改动（~10 行） | 仍是轮询；空闲 CPU 降低但延迟抖动引入（首包最多等 50ms）；治标 |
| A4 waker 注入 phy | 给 TunPhy 实现 `phy::Device` 的同时注册 waker，fd 可读时 wake | 最优雅 | smoltcp 0.14 的 phy trait 无 waker 钩子，要 vendor 补丁，复杂度最高 |

### 推荐

**A1**。要点：
- `let mut async_fd = tokio::io::unix::AsyncFd::new(tun_fd)?;`
- select 分支：`(async_fd.readable().await)?.clear_ready();` → `stack.poll()`
- 定时分支：`sleep_until(next_maintenance)`（smoltcp 无显式接口时用固定
  100ms 兜底即可满足重传精度）
- 回归测试：bridge 数据双向吞吐不回归（现有 curl google 204 用例）+ 空闲
  CPU 对比（activity monitor 采样）。

---

## 问题 3 — Bridge 异步化：建立连接不再阻塞 tun0 主循环

### 现状与影响

```rust
while let Some((handle, remote)) = stack.take_new_connection() {
    let bridge = open_tunnel_bridge(...).await;   // ← 主循环内联等待
```

`open_tunnel_bridge` = 新建 mTLS WS 连接 + connect 帧 + 等 stream_open，
实测正常 ~300ms，server 慢/网络差时最长 15s（HTTPS 超时上限）。

**在此 await 期间，主循环完全不跑**：已有连接的数据泵停转（正在下载的
页面卡住）、新 SYN 无法处理、UDP DNS（fake-dns 模式）停答。打开一个含
几十个连接的页面时，这些 bridge 串行建立，总阻塞可达数秒 —— 这是可感知
的卡顿来源。

另一个隐患：bridge 建立失败只 reset 该 socket，但成功路径里 slot 状态
机（Connecting→Active）与 pump 循环耦合在同一循环，任何一环慢都会放大。

### 目标

bridge 建立（以及失败重试）不阻塞数据泵；并发建立多个 bridge。

### 方案对比

| 方案 | 做法 | 优点 | 缺点 |
|---|---|---|---|
| B1 spawn + oneshot 回填（推荐） | 主循环发现新连接 → 标记 Connecting → `tokio::spawn` 一个任务执行 `open_tunnel_bridge`，完成后经 `oneshot`/mpsc 把 bridge 发回主循环，由主循环在下一轮 poll 里接入 slot | 主循环永不因建桥阻塞；并发建桥自然形成；改动集中在 tun_socks.rs（~60 行） | bridge 任务持有 server_url/exit 等克隆；需要处理"结果回来时 slot 已被 reset"（以 generation 计数或校验 state==Connecting 且 handle 匹配） |
| B2 建桥队列 + 单 worker | 一个专职 worker 任务按队列串行建桥，主循环只入队 | 主循环不被阻塞；worker 可做退避重试 | 仍串行建桥（页面上线慢）；多一层队列状态管理 |
| B3 连接池预热 | 预先与 server 保持 N 条 WS 连接，建桥只是发帧 | 建桥延迟降到 ~RTT | 大改 tunnel 协议与 server；超出本次范围 |

### 推荐

**B1**。要点：
- slot 增加 `generation: u64`（每次 reset_socket 递增）；spawn 任务捕获
  `(handle, generation)`，回发结果时主循环校验 generation 一致才接入。
- 结果通道：`mpsc::unbounded_channel<(SocketHandle, u64, Result<TunnelBridge, String>)>`。
- 失败路径：spawn 任务直接 `reset_socket`（经主循环代执行，避免跨线程碰
  非 Send 的 stack——注意！stack 不可 Send，reset 也必须由主循环做，因此
  失败信息同样走结果通道）。
- 并发上限：同时 Connecting 数 ≥8 时暂缓 spawn（防 server 过载），排队下轮。

---

## 实施顺序建议

三者相互独立，可分开落地；按收益/风险排序：

1. **问题 1 一期（watchdog 自愈 + 信号处理 + 自毁保护）** — 小改动、消除最疼的用户可见故障。
2. **问题 3（B1 bridge spawn 化）** — 消除可感知卡顿；中等改动，需重点测 generation 竞态。
3. **问题 2（A1 AsyncFd）** — 省电优化；改动集中在主循环，建议在 1/3 稳定后做（同一处代码区域，避免合并冲突）。

预计工作量：一期 ≈ 半天；B1 ≈ 1 天（含回归）；A1 ≈ 半天（含 CPU 对比验证）。

## 回归验证清单（实施后统一跑）

- [ ] curl google 204（透明代理主链路）
- [ ] 打开多连接页面（如 news.ycombinator.com）无卡顿；bridge 建立期间已有下载持续
- [ ] `kill -9` tun0 → ≤10s 内 DNS 自动还原（watchdog 生效）
- [ ] 正常停止 tun0 → DNS 立即还原
- [ ] 空闲 1 分钟 CPU 占用 ≈0%（AsyncFd 生效）
- [ ] 73 个 cargo 测试 + Swift build 零警告
