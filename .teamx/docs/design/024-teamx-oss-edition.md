# 024 · teamx CLI 命令树 & teamx-oss（开源版）裁剪方案

> 用途：给出**完整命令树**（当前 HEAD：55 个顶层命令 / 311 个命令节点），并设计**裁剪约五分之一**
> 形成 `teamx-oss`（开源版）的方案。维护：owner 台账仓 + 本仓镜像（diff 0）。

## §1 完整命令树（55 顶层 / 311 节点）

> 统计口径：`teamx <path> --help` 递归解析（脚本 dump）。节点数含顶层自身。

### A. 团队协作内核（自托管 OSS 核心）
```
init                                                  1
version                                               1
team      create|join|approve|deny|promote-lead|demote-lead|list|status|     17
          leave|archive|destroy|invite|invite-list|invite-revoke|import|clone
goal      set|share|close                             4
member    set-state|rename|set-user|discussion-quota  5
role      list|set|propose|approve|deny|update        7
discussion create|list|open|say|reply|close|handoff|subscribe|unsubscribe 10
task      create|ack|claim|update|done|verify|reject|retract|re-bid|list|log 12
events|log|sync|publish|ask|respond|ask-cancel        各 1
nudge     set|get|reset                               4
user      list                                        2
teamfile  validate|sync                               3
session   list                                        2
loopx     report                                      2
serve                                                 1
cert      init|issue|issue-service|ca                 5
plugin    install|uninstall                           3
```

### B. 节点与客户端连接
```
worker                                                1
agents    spawn|list|stop                             4
local-team up|ps|stop|logs                            5
fs                                                    1
console                                               1
local     member-list|member-add|member-update|member-remove|get|set 7
git       setup|clone|pull|push|commit|commit-push|list|create|delete|grant|permissions 12
tunnel    expose|list|status|close|forward            6
direct    advertise|candidates|probe|listen|ping      6
proxy     exit|start|routes(list|add|remove|set-default|import|clear) 10
tun0      start|stop|status                           4
dns       list|resolve                                3
gui|gui-panel|gui-member                              各 1
```

### C. 企业云平台（多租户 SaaS）
```
platform  bootstrap-token|redeem|revoke|tokens|admins|admin-revoke|audit|   15
          user(create|set-password|disable|list|set-quota)|console
tenant    create|destroy|reissue-members|ca-audit|owner-letter|set-endpoint|list|show 9
cloud     login|logout|whoami|status|user(6)|team(6)|tenant(list|show|create| 63
          start|stop|status|destroy|endpoint|ca(3)|member(7))|join(4)|node(13)|bootstrap(4)
site                                                  1
issued    list|revoke                                 3
discovery directory|set|apply|mine|withdraw|pending|reconcile|decide 9
```

### D. 经济 / IM / 管理看板
```
tcoin     grant|allocate|settle|reverse|show          6
credit    refresh|show                                3
activity  push|query(summary|rows)                    5
model     set|ls                                      3
im        bind-code|bind|unbind|bindings|resolve      6
im-gateway check|run                                  3
ui                                                    1
```

### E. 企业流量分析 / 云主机编排
```
capture   start|stop|status|list|prune|import-keylog|export 8
analyze   stats|list|http|decrypt                     5
replay    serve|tcp(start)|http(extract|export-har|to-curl|send) 9
node      provision-cloud|provision|list|status|ssh|stop|start|destroy|  13
          auto-stop|run-jobs|snapshot|release-eip
```

## §2 分版判据

| 判据 | 归属 |
|---|---|
| 单/多团队**自托管协作**（team/goal/member/task/discussion/git/serve/证书/客户端连接） | **OSS 保留** |
| **多租户云平台面**（platform/tenant/cloud/discovery/site/issued） | 企业裁剪 |
| **经济与信用**（tcoin/credit/activity/model） | 企业裁剪 |
| **IM 集成**（im/im-gateway，飞书 long-connection + 平台身份） | 企业裁剪 |
| **管理看板**（ui = admin-console） | 企业裁剪 |
| **流量抓包/分析/回放**（capture/analyze/replay，ENT-04/05/06） | 企业裁剪（可选加深） |
| **云主机编排**（node = AWS 生命周期） | 企业裁剪（可选加深） |
| 成员 web console（`console`） | **OSS 保留**（成员自助视图） |

## §3 裁剪方案（审计 / 财务 / 看板）

> owner 定调：OSS 版裁掉**审计、财务、看板**三类相关功能/命令；其余保留。

### 3.1 域 → 命令映射

| 域 | 命令 | 说明 | 处理 |
|---|---|---|---|
| **审计** | `platform`（含 `audit` / `tokens` / `admins` / `admin-revoke` / `console`） | 平台审计日志 + 引导令牌/管理员/平台控制台 | 裁 |
| **审计** | `issued list\|revoke` | 证书签发台账（O-2） | 裁 |
| **审计** | `tenant ca-audit`、`cloud tenant ca audit` | 租户 CA 审计 | 裁（子命令级） |
| **审计** | `log`（团队事件时间线回放） | "audit replay"；但也是核心台账透明性 | **判断点**：默认保留，如按字面也可裁 |
| **财务** | `tcoin`（grant/allocate/settle/reverse/show） | 代币账本 | 裁 |
| **财务** | `credit`（refresh/show） | 信用评分 | 裁 |
| **财务** | `model`（set/ls） | 模型计价 | 裁 |
| **财务** | `activity`（push/query） | 用量分析（喂 TCoin/账单） | 裁 |
| **看板** | `ui` | admin-console 分析看板 | 裁 |
| **看板** | `site`（云官网 + 平台控制台 `/console`） | 平台控制台本身含 tenants/tokens/admins/audit | 裁 |
| **看板** | `console`（成员 web 控制台 = user-console） | 成员自助视图 | **保留** |

### 3.2 裁剪集

**基础集（严格按三域）：8 个顶层**
```
platform  issued  tcoin  credit  model  activity  ui  site
```
+ 子命令级 gate：`tenant ca-audit`、`cloud tenant ca audit`
→ 顶层 8/55 = **14.5%**；节点 37/311 ≈ **12%**。

**推荐集（≈五分之一，把承载审计/看板的"多租户管理面"一并去掉）：11 个顶层**
```
platform  tenant  cloud  discovery  issued  site
tcoin  credit  model  activity  ui
```
→ 顶层 11/55 = **20%**；节点 ≈ 109/311 ≈ **35%**。
理由：`tenant`/`cloud`/`discovery` 是**多租户云管理面**，审计（CA/签发）、看板（平台控制台）、财务（配额/记账）都挂在这套面上；只删子命令会留下"半套控制面"，不干净。

**（可选）再加深**：`capture` `analyze` `replay` `node`（企业流量分析 + AWS 主机编排）→ 累计 15 顶层（27%）。

### 3.3 依赖提示
- `activity` 被 `tcoin`/`credit` 消费：三个一起裁，互不残留。
- `platform console` 与 `site` 的 `/console` 是同一平台控制面：一起裁。
- `discovery`（公开目录/申请）与"邀请函加入"（`team invite/import`）互不依赖：裁目录，保留邀请。
- `issued`（证书台账）与 `cert`（PKI 本体）分开：裁 = 去掉台账/审计，保留签发能力。


## §4 实现机制（建议）

- **Cargo feature `enterprise`**（本仓默认 `default=["enterprise"]`；`teamx-oss` 用 `--no-default-features`）：
  - `#[cfg(feature="enterprise")]` 标在 `cli.rs` 的 `Command` 变体 + 子命令 enum，以及 `commands.rs` 的分发臂；
  - 模块声明 `#[cfg(feature="enterprise")] pub mod platform/tenant/cloud/discovery/tcoin/credit/activity/im/ui/capture/analyze/replay;`
  - `serve.rs` 里平台面 RPC（`PLATFORM_CONSOLE_METHODS` 与相关 handler）按 feature gate；
  - 依赖：`ureq`（cloud login）等按 feature `optional = true`；AWS 用系统 `aws` CLI，无编译依赖。
- **CLI 帮助**：OSS 构建的 `--help` 自然只列保留命令（cfg 掉的变体不出现），无需运行时过滤。
- **发布**：两个 crate 变体同源——`teamx`（OSS，`--no-default-features`）与 `teamx-enterprise`（默认 enterprise）；`crates/teamx/Cargo.toml` 加 `[features] default=["enterprise"] enterprise=[]` 以及 optional deps。
- **测试**：企业相关单测/集成测试同样 `#[cfg(feature="enterprise")]`；CI 跑两套（`cargo test` 与 `cargo test --no-default-features`）。

## §5 依赖与风险

| 点 | 说明 |
|---|---|
| `serve` 耦合 | 核心 `serve` 内含平台面 RPC；需把 method 表与 handler 分文件/分 feature，避免 OSS 版残留入口 |
| `discovery` vs 邀请 | OSS 保留"邀请函加入"（`team invite/import`）；移除的是"公开目录/申请" |
| `activity`/`model` | 纯经济用；`activity push` 若被 `tcoin` 依赖则一起裁 |
| `capture`/`tun0` | 数据面 tap 在 tun0；裁 capture 时保留 tun0 转发、去掉落库 |
| 文档 | 两版 README/命令索引；企业文档（ENT-*）不进 OSS |
| 许可 | OSS 采用开源许可；企业能力隔离在同一 crate 的 feature 后（或拆 crate） |

## §6 实施步骤

1. `Cargo.toml` 加 `[features] default=["enterprise"]`。
2. `cli.rs`：给 Tier-1/2 的 `Command` 变体与对应子命令 enum 加 `#[cfg(feature="enterprise")]`。
3. `commands.rs`：分发臂加 cfg；`lib.rs` 模块声明加 cfg。
4. `serve.rs` / `platform_console.rs`：平台面 RPC、控制台路由按 feature gate。
5. CI：`cargo build/test`（enterprise）与 `cargo build/test --no-default-features`（oss）双跑 + `-D warnings`。
6. 生成 OSS `--help` 快照对比（Tier 集差异）作为回归。
7. 发布 `teamx-oss`（README/许可/命令树文档）。

## §7 实施进度（2026-09-26）

- **Cargo feature**：`crates/teamx/Cargo.toml` → `[features] default = ["enterprise"]`，`enterprise = []`。
- **CLI 面门控**（`#[cfg(feature = "enterprise")]`）：
  - `cli.rs`：11 个 `Command` 变体 + 对应子命令 enum/struct（`PlatformCmd/TenantCmd/CloudCmd/DiscoveryCmd/IssuedCmd/SiteCmd/TcoinCmd/CreditCmd/ActivityCmd/ModelCmd/UiCmd`）。
  - `commands.rs`：11 个分发臂 + 企业导入；`hostname`/`split_model_ref` 亦 gate。
  - `serve.rs`：平台面 RPC→CLI 映射臂（tenant/discovery/credit/tcoin/issued，24 臂）+ `TenantCmd` 导入 gate。
  - `main.rs`：`Ui` / `Platform(console)` / `Site` 三个常驻入口 gate。
  - `lib.rs`：`cloud` / `ui` / `site` / `platform_console` 模块 gate。
- **验证**：
  - `teamx --help` 顶层：enterprise 56 → oss 45；被裁 = `activity cloud credit discovery issued model platform site tcoin tenant ui`（11 个 ≈ 1/5）。
  - 测试：enterprise **473 绿**；oss **452 绿**；两版 `clippy -D warnings` 干净。
  - 二进制：release 16.5MB → 13.1MB（**−20%**）。
- **残留（后续）**：企业**模块**仍被 core 引用（`tcoin/credit/issued → serve.rs`、`activity → db/serve`、`platform/discovery/provision → k8s/serve`），故当前只裁到「命令面 + 独立模块」；进一步裁模块/依赖需先解除这些耦合（把 serve 的平台面 RPC 与 k8s 的 provision 引用分文件/gate）。
