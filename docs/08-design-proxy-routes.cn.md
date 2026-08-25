# Proxy 出口路由（按目标域名/IP 分流）设计方案

- 文档类型: design
- 关联: `06-design-proxy.md`（SOCKS5 出站代理）、`20-manual-tunnel-proxy-cli.md`（运维手册）
- 日期: 2026-08-22
- 目标版本: teamx CLI（Rust）>= 0.2.1（在隧道 WS 心跳 + 自动重连修复之上）

## 1. 背景与动机

多台云主机各自运行 `proxy exit <name>`（egress / egress2 / ...），本地可同时开多个
`proxy start --port <本地端口> --exit <名字>`。当前**一个本地 SOCKS5 端口只能绑定一个 exit**，
所有流量走同一个出口。用户希望：

> 在本地**一个 SOCKS5 端口**上，根据**目标域名或 IP** 自动选择不同的 egress 出口。

例如：`*.cn` 走 egress2（35.79.166.197）、`example.com` 走 egress（81.70.41.108）、
其余走默认出口。

## 2. 可行性结论（代码证据）

按目标分流在协议层**完全可行**，且不需要额外 DNS 解析：

- `socks5.rs:14-17`：`parse_connect_request` 返回 `SocksTarget { host, port }`，
  **host 是 CONNECT 请求里原样的域名或 IP**（`curl --socks5-hostname` 发域名，
  `--socks5` 发解析后 IP）。数据在本地就知道目标是谁。
- `tunnel_client.rs:556-576`：每个连接解析出 `target` 后**才**建立 WS 并发送
  `{"type":"connect","name":"<exit>","target":"host:port"}`（`:604`）。
- `tunnel_client.rs:510`：`run_socks5_proxy(server_url, exit_name, local_port)` 的
  `exit_name` 目前是固定参数。

**结论**：把"固定 exit_name"换成"在解析出 target 后，查一张 `host/IP → exit` 路由表得到
exit_name"，即可实现按目标分流。改动集中在本地消费侧，**server 与 exit 侧零改动**。

## 3. 设计

### 3.1 新路由表格式（JSON 文件，`--routes <path>`）

```json
{
  "default": "egress",
  "rules": [
    { "match": "*.cn",             "exit": "egress2" },
    { "match": "api.example.com",  "exit": "egress" },
    { "match": "10.0.0.0/8",       "exit": "egress2" },
    { "match": "192.168.1.5",      "exit": "egress" },
    { "match": "2001:db8::/32",    "exit": "egress2" }
  ]
}
```

- `default`（必填）：无规则命中时使用的 exit 名。
- `rules[]`（可选）：有序规则，**第一条命中生效**（first-match）。
- `match` 支持三种形式：
  | 形式 | 示例 | 语义 |
  |------|------|------|
  | 域名通配 | `*.cn`、`api.example.com` | 后缀匹配；`*.cn` 匹配 `www.baidu.com`、`a.b.cn`，**不**匹配 `cn.com` |
  | 精确域名 | `example.com` | 完全等于该域名（不匹配 `api.example.com`） |
  | IPv4/IPv6 CIDR | `10.0.0.0/8`、`2001:db8::/32` | 目标为 IP 时按网段匹配 |
  | 精确 IP | `192.168.1.5` | 目标为 IP 时精确匹配（CIDR /32 或 /128 的简写） |

匹配顺序建议（规则内先于类型）：
1. 若目标是域名：先查**精确域名**，再查**最长后缀通配**（`api.example.com` 优于 `*.example.com` 优于 `*.com`）——但保持**规则文件中的先后顺序**为最终裁决（first-match，见 §5 争议）。
2. 若目标是 IP：精确 IP > 更具体的 CIDR（前缀更长者优先）。

### 3.2 CLI 变更（`cli.rs`）

```rust
Start {
    port: u16,                    // 不变
    exit: Option<String>,         // 改为可选：无 SQLite 配置且无 -f 时的固定 exit
    routes: Option<PathBuf>,      // 新增：路由表 JSON 文件；-f 为短别名
    server: Option<String>,       // 不变
}
```

**路由来源优先级**（`proxy start`）：
1. `-f <file>` / `--routes <file>` — 显式 JSON 文件（本次调用临时生效，不写 DB）
2. **SQLite 路由表**（`proxy_routes` / `proxy_settings`，默认行为）
3. `--exit <name>` — 固定 exit（向后兼容）
4. 都缺失 → 启动报错，提示用 `proxy routes set-default` / `-f` / `--exit`

**新增 `proxy routes` 子命令**（管理 SQLite 路由表）：

```
teamx proxy routes list                 # 显示 default + 规则
teamx proxy routes add <match> <exit> [--seq N]   # 追加或指定位置
teamx proxy routes remove <match>       # 按 match 删除
teamx proxy routes set-default <exit>   # 设置默认 exit
teamx proxy routes import <file.json>   # 从 JSON 导入（整表替换）
teamx proxy routes clear                # 清空规则（保留 default）
```

### 3.3 新模块 `routes.rs`（纯函数 + SQLite 持久化，可单测）

```rust
pub struct RouteRule {
    pub match_type: MatchType,   // ExactDomain | SuffixDomain | Cidr
    pub pattern: String,         // 原始 pattern 文本（域名/CIDR）
}

pub struct RouteTable {
    pub default: String,
    pub rules: Vec<RouteRule>,
}

impl RouteTable {
    /// 从 JSON 文本解析（校验 default 存在、rule 的 match 合法）。
    pub fn parse(json: &str) -> Result<RouteTable, String>;
    /// 给定 SOCKS5 目标 host（域名或 IP），返回应使用的 exit 名。
    pub fn resolve(&self, host: &str) -> &str;
}
```

- `resolve` 内部：
  - 尝试把 `host` 解析为 IP（`IpAddr::from_str`）；成功 → 按 CIDR/精确 IP 匹配。
  - 失败 → 按域名匹配（精确 + 通配后缀）。
  - 均未命中 → `default`。
- 解析 `IpAddr` 失败即域名（不 panic）；空 host 直接 `default`。

**SQLite 持久化**（`proxy_routes` 表存规则 + `proxy_settings` 存 default）：

```rust
pub fn load_from_db(conn) -> Result<Option<RouteTable>, String>;  // None = 未配置
pub fn save_to_db(conn, table) -> Result<(), String>;             // 整表替换
pub fn upsert_rule(conn, seq, pattern, exit) -> Result<i64, String>;
pub fn remove_rule(conn, pattern) -> Result<bool, String>;
pub fn set_default(conn, exit) -> Result<(), String>;
pub fn clear_rules(conn) -> Result<(), String>;
pub fn to_json(table) -> serde_json::Value;                       // list 输出
```

### 3.4 消费侧改造（`tunnel_client.rs`）

- `socks5_proxy(server_url, exit_name, local_port, routes: Option<RouteTable>)` —
  新增 `routes` 参数（`None` → 固定 exit，向后兼容）。
- `run_socks5_proxy` 循环里：解析出 `target` 后，
  ```rust
  let exit = routes.as_ref().map(|t| t.resolve(&target.host)).unwrap_or(exit_name);
  ```
  然后照常建立到 `exit` 的 WS 连接（每连接一条，`exit` 名动态）。
- **每连接解析一次**：同一 SOCKS5 连接只发一个 CONNECT，天然精确。

### 3.5 接线（`commands.rs`）

```rust
ProxyCmd::Start { port, exit, routes, server } => {
    let table = match routes {
        Some(path) => RouteTable::parse(&fs::read_to_string(path)?),   // 文件优先
        None => load_from_db(conn)?,                                   // SQLite 兜底
    };
    let exit_name = table 的 default  || --exit || 报错;
    socks5_proxy(&url, &exit_name, *port, table)
}
ProxyCmd::Routes(rc) => proxy_routes_cmd(conn, rc),  // 管理 SQLite 表
```

### 3.6 server / exit 侧

**零改动**。exit 侧只收到 `{"type":"connect","name":"<exit>","target":...}`，
名字来自本地路由决策；server 侧只按名字查隧道。多 exit 并存能力已经具备
（`tunnel.rs:178` 名字唯一约束）。

### 3.7 数据模型（`db.rs` 迁移 v6）

```sql
CREATE TABLE IF NOT EXISTS proxy_routes (
  id       INTEGER PRIMARY KEY AUTOINCREMENT,
  seq      INTEGER NOT NULL,          -- 规则顺序（first-match）
  match    TEXT NOT NULL,             -- "*.cn" / "10.0.0.0/8" / IP / 域名
  exit     TEXT NOT NULL,
  UNIQUE (seq)
);
CREATE TABLE IF NOT EXISTS proxy_settings (
  key   TEXT PRIMARY KEY,             -- 'default_exit'
  value TEXT NOT NULL
);
```

- 本机全局（不分 team）：一个 SOCKS5 端口一套路由，符合使用场景。
- `proxy_routes` 存规则、`proxy_settings` 存 default exit。

## 4. 测试计划

### 4.1 单元测试（`routes.rs` 内嵌 `#[cfg(test)]`）

| 用例 | 输入 | 期望 |
|------|------|------|
| 默认兜底 | 无规则命中 | 返回 `default` |
| 精确域名 | `api.example.com` 规则命中 | 返回对应 exit |
| 通配后缀 | `*.cn` 命中 `www.baidu.com`；`cn.com` 不命中 | 正确分流 |
| 通配不越级 | `*.example.com` 不匹配 `example.com` 本身 | 不命中 |
| 精确 IP | `192.168.1.5` | 命中 |
| CIDR IPv4 | `10.0.0.0/8` 命中 `10.1.2.3` | 命中 |
| CIDR IPv6 | `2001:db8::/32` | 命中 |
| 规则顺序 | first-match | 首个命中者生效 |
| 空 host | `""` | 返回 default |
| 非法 JSON | 缺 default / 坏 match | 解析报错 |
| SQLite 往返 | save → load 一致 | 相等 |
| SQLite 默认 | set_default + add 后 load | default + 规则正确 |
| SQLite upsert | 指定 seq 覆盖 | 原位替换 |

### 4.2 集成测试（`tests/proxy-routes-test.ts`）

端到端验证（复用现有 serve/mTLS/SOCKS5 基建）：
1. 起**两个** proxy exit（`egress` 暴露 IPv4 服务 svc-a，`egress2` 暴露 IPv6 服务 svc-b）。
2. `proxy start --routes routes.json`（default=egress，`::1 → egress2`）：
   - CONNECT `::1` → 命中 egress2 → 返回 svc-b 内容。
   - CONNECT `127.0.0.1` → default egress → 返回 svc-a 内容。
3. 回归：`proxy start --exit egress2`（无 routes）→ 固定出口仍生效。
4. **SQLite 路由**：`proxy routes set-default egress` + `proxy routes add ::1 egress2`，
   然后 `proxy start`（无 `--exit` / 无 `-f`）→ 从 DB 读取，分流正确。

## 5. 设计权衡与决策记录

| 问题 | 选项 | 决策 | 理由 |
|------|------|------|------|
| 规则匹配优先 | 最长后缀 vs 文件顺序 | **文件顺序 first-match**（配合精确优先的隐式约定） | 直观、可预测；用户按意图排序即可 |
| 域名 vs IP 是同一张表 | 是 | 是 | host 字符串统一进 `resolve`，内部按"能否解析成 IP"分流 |
| 热加载 | 启动时读一次 vs watch | 启动时读一次（v1） | 改动最小；热加载列为后续增强 |
| 旧入口兼容 | 破坏性改签名 vs 新增入口 | **`socks5_proxy` 加 `Option<RouteTable>` 参数** | 不破坏 `proxy exit` 与既有测试 |
| `--exit` 与 `-f/--routes` 关系 | 冲突 vs 互补 | `-f` 提供时优先，SQLite 次之，`--exit` 兜底 | 灵活且向后兼容 |
| 配置存储 | JSON 文件 vs **SQLite** | **默认 SQLite**（`proxy routes` 管理）；`-f` 临时文件覆盖 | 与团队状态同库，持久化、可查询、命令可管理 |

## 6. 实施步骤（已完成）

1. 新增 `crates/teamx/src/routes.rs`：匹配器（`MatchType` / `RouteRule` / `RouteTable`）+ SQLite 读写 + 单元测试。✅
2. `main.rs`：注册 `mod routes;`。✅
3. `cli.rs`：`ProxyCmd::Start` 增加 `-f/--routes`；`exit` 改可选；新增 `Routes(RoutesCmd)` 子命令（list/add/remove/set-default/import/clear）。✅
4. `tunnel_client.rs`：`socks5_proxy` 增加 `routes: Option<RouteTable>`；`run_socks5_proxy` 按 target 动态解析 exit。✅
5. `commands.rs`：`proxy start` 路由优先级（文件 > SQLite > --exit）；`proxy routes` 子命令接线。✅
6. `db.rs`：迁移 v6 新增 `proxy_routes` + `proxy_settings` 表。✅
7. 测试：单元（routes.rs 匹配 + SQLite 往返）+ 集成（`tests/proxy-routes-test.ts` 文件路由 + SQLite 路由 + 固定 exit 回归）。✅
8. 文档：`docs/08-design-proxy-routes.md`（本文件）+ `docs/20-manual-tunnel-proxy-cli.md`（使用示例）+ CHANGELOG。

## 7. 使用示例

```bash
# 方式一：SQLite 配置（默认）
teamx proxy routes set-default egress
teamx proxy routes add '*.cn' egress2
teamx proxy routes add '10.0.0.0/8' egress2
teamx proxy start --port 1080        # 无 --exit / 无 -f，从 SQLite 读路由

# 方式二：临时 JSON 文件（不写 DB）
cat > routes.json <<'EOF'
{ "default": "egress", "rules": [ { "match": "*.cn", "exit": "egress2" } ] }
EOF
teamx proxy start --port 1080 -f routes.json

# 方式三：固定出口（向后兼容）
teamx proxy start --port 1080 --exit egress2
```

## 8. 非目标（v1 范围外）

- 路由表热加载 / 动态重读
- 按端口 / 按协议（TCP/UDP）分流
- 规则命中统计 / 日志
- 基于 exit 在线状态自动故障转移（可在路由表之上做，但不在本次范围）
