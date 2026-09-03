# Teamx Console（成员端 Web 控制台）设计文档

> 前置：需求分析见 [31-member-console-requirements.cn.md](31-member-console-requirements.cn.md)
> 流程第 2 步：设计文档 → 之后为第 3 步 HTML 原型 → 第 4 步开发
> 状态：**设计（待评审）**
> 目标读者：维护者 / 实现者

---

## 0. 已确认决策（来自需求分析）

| 项 | 决策 |
|---|---|
| 命令名 | `teamx console` |
| 面板 | MVP 五面板：Teams / My Tasks / Events / Tunnels / Cloud(iframe)；Goals/Repos 后续 |
| 数据源 | 当前工作空间 `.teamx` DB + CLI 等价信息；点击 → 跑 CLI → 渲染 |
| 只读 | MVP 全只读，不写团队/任务/成员/目标状态 |
| 部署 | personal 本地运行/访问；127.0.0.1 + token |
| 风格 | 复用 teamx ui 布局/菜单/主题；品牌 "Teamx Console" |
| iframe 受限 | console 提供本地反向代理，剥离 `X-Frame-Options`/CSP 以嵌入 |
| Cloud 配置 | 项目级 `.teamx/console.json` |
| 复制 forward | Tunnels 面板提供"复制转发命令"按钮（只提示，不代执行） |
| 登录/401/404 | 复用 teamx ui 页面与文案，品牌替换为 Teamx Console |

---

## 1. 架构总览

```
浏览器 ──HTTPS (self-signed, 127.0.0.1:9528)──▶ teamx console (新模块 console.rs)
         │  ?token= 或 cookie                        │
         ▼                                          │
    [Login 页 / 单页工作台]                          │ /api/panels/<id>
        Teamx Console                                │ 执行器 run_cli(id)
        五面板 + Cloud iframe                        │   ├─ teamx <固定参数>   ← current_exe 子进程
         │                                          │   │     (cwd=启动目录，继承 TEAMX_* 环境)
         │  /api/cloud/<id>/* (反向代理)             │   │      读 .teamx/teamx.db / 走 mTLS server
         ▼                                          │   └─ 超时+错误 → JSON
    本地 127.0.0.1:<cloud.port>                      │
      (用户已 tunnel forward 的 headless/ttyd)       ▼
                                               .teamx/console.json（云节点清单）
```

关键设计：

1. **console 与 ui 并列**：`Command::Console(ConsoleCmd)`，main.rs 仿照 `Command::Ui` 提前分发；
2. **后端 = 白名单 CLI 执行器**：`/api/panels/<id>` 只能运行预定义面板对应的固定命令；不暴露任意命令执行；
3. **执行上下文 = 启动时 cwd**：子进程 `current_exe()`，不改 cwd、不改 env → 自然解析到 `<cwd>/.teamx`（DB/letters/server），与用户在该目录敲 CLI 等价；
4. **前端复用 ui 风格**：从 `ui.rs` 的 `page_html()` 提取通用外壳（顶栏/nav/主题），替换品牌与内容区；
5. **Cloud 反代**：iframe 统一走 `/api/cloud/{id}/…` 同源路径 → 后端代理到本地端口，顺手剥离 XFO/CSP 响应头。

## 2. 命令与参数

在 `cli.rs` 增加：

```rust
/// Member web console (local, view-only). Mirrors the owner dashboard's
/// look & feel but shows only the current workspace member's info.
Console(ConsoleCmd),
```

```rust
#[derive(Args, Debug, Clone)]
pub struct ConsoleCmd {
    #[arg(long, default_value = "127.0.0.1")] pub addr: String,
    #[arg(long, default_value_t = 9528)]     pub port: u16,
    /// optional actor session key; when absent, session resolution mirrors
    /// the CLI (auto from instance.json/letters) in the launch cwd
    #[arg(long)] pub session: Option<String>,
    /// cloud-node list file (default <teamx_home>/console.json)
    #[arg(long)] pub config: Option<PathBuf>,
    /// server URL override (default: TEAMX_SERVER_URL)
    #[arg(long)] pub server_url: Option<String>,
}
```

启动打印（对齐 ui）：

```
teamx console: member web console (view-only)
  open: https://127.0.0.1:9528/?token=...
  (self-signed cert; your browser will warn about it once)
  workspace: <cwd>/.teamx
  Ctrl+C to stop
```

`main.rs` 在 `Command::Ui` 分支旁增加同构分支，进入 `console::run(cmd)`。

## 3. 数据层（执行器）

模块内定义：

```rust
struct PanelSpec { name: &'static str, args: &'static [&'static str] }
static PANELS: &[PanelSpec] = &[ /* 见 §5 */ ];

fn run_panel(id: &str) -> Result<PanelOutput, String>
// PanelOutput { ok: bool, stdout: String, stderr: String, elapsed_ms: u64 }
```

- 用 `std::process::Command::new(std::env::current_exe()?)` 组装固定参数，`--json` 全局开关视面板需要；
- 继承环境（`TEAMX_SERVER_URL`/`TEAMX_HOME`/`TEAMX_SESSION` 等），不 set cwd；
- 子进程超时（默认 10s，超时杀进程返回错误）；
- 面板只读：所有命令均为查询类；开发阶段逐个核对无写副作用（`git setup`/`task ack`/`sync` 光标推进除外——`sync` 用 `--no-advance`，绝不推进游标）。

**同步注意**：Events 面板若对应 `sync`（默认会推进本地游标），console 一律传 `--no-advance`，保持只读语义。

## 4. HTTP / API（axum）

| 路由 | 方法 | 说明 |
|---|---|---|
| `/` | GET | 单页工作台（token 校验同 ui） |
| `/login` | GET | 登录页（复用 ui，品牌 Teamx Console） |
| `/api/panels/<id>` | GET | 运行白名单面板命令，返回 `{ok, stdout, stderr, elapsed_ms}` |
| `/api/cloud` | GET | 读 `.teamx/console.json` 云节点清单 |
| `/api/cloud` | POST | 保存清单（写 `.teamx/console.json`，非团队状态） |
| `/api/cloud/{id}/*` | GET/`ws` | 同源反代到该节点本地端口（剥离 XFO/CSP），支持 WebSocket 升级（ttyd/xterm 需要） |
| `/api/forward-hint/<tunnel>` | GET | 返回可复制的 forward 命令串（来自 `tunnel list` + 命名规则） |
| fallback | — | 404（复用 ui，品牌 Teamx Console） |

鉴权：与 ui 一致——cookie `teamx_console_token`；`?token=` 直达置 cookie；静态/API 未带 token 一律 401/登录页。

## 5. 面板 → CLI 映射（候选，开发阶段在插件/CLI 实测后锁定）

| 面板 | 视图动作 | 底层命令（候选） | 说明 |
|---|---|---|---|
| Teams | 概览 | `teamx sync --no-advance` + 状态渲染（对齐 opencode 插件 `teamx_status` 的底层命令） | 显示团队/目标/我的角色/成员/最近事件摘要 |
| My Tasks | 列表 | `teamx task list --json`（按我/角色过滤参数开发阶段核对） | 卡片/表格：title、state、priority |
| Events | 列表 | `teamx events --json`（`--after` 翻页可选） | 最近事件时间线 |
| Tunnels | 列表 | `teamx tunnel list --json` | 隧道名/模式/端口/provider + "复制 forward" |
| Cloud | 清单 | 读 `.teamx/console.json`；点击入口走 `/api/cloud/{id}/` | iframe 内嵌 headless/ttyd |

> 说明：面板命令精确化放到开发第 1 小步（用 `teamx --help` 与插件的 runCli 映射逐条对齐并记录）。

## 6. `.teamx/console.json` schema

```json
{
  "version": 1,
  "nodes": [
    {
      "id": "caicai-headless",
      "name": "caicai · headless",
      "kind": "headless",
      "url": "http://127.0.0.1:19034"
    },
    {
      "id": "caicai-ttyd",
      "name": "caicai · ttyd",
      "kind": "ttyd",
      "url": "http://127.0.0.1:19033"
    }
  ]
}
```

- 页面提供"添加节点"（名称 + 本地 URL + kind），写回该文件；
- URL 校验：仅允许 `http://127.0.0.1:<port>` / `http://localhost:<port>`，防 SSRF。

## 7. Cloud 反向代理要点

- 匹配 `/api/cloud/{id}/*`：查 node，`reqwest`/手动 TCP 转发到 `url` 对应端口，路径/query 透传；
- 响应头删除 `X-Frame-Options` 与 `Content-Security-Policy`（允许被 iframe 嵌入）；保留其他；
- 需要支持 WebSocket（ttyd 的 `/ws` 与 opencode headless 的会话 WS），开发时按 `Upgrade: websocket` 做隧道升级（参考 ui.rs 的 pty WS 或 tunnel_client 连接方式）；
- 若某节点页面仍因 CSP/COOP 无法嵌入，给出明确的"受限"提示卡片。

## 8. 前端（单页）

- **外壳**：复制/抽取 ui.rs `page_html()` 的 CSS 变量与顶栏布局；顶栏左侧 logo-text 显示 **"Teamx Console"**（图标沿用 Tx），右侧主题 ☀️/🌙、用户徽标 "Member"；
- **导航**：Teams（默认）/ My Tasks / Events / Tunnels / Cloud；当前项高亮，切换渲染对应 `.view`；
- **数据渲染**：后端返回 JSON（CLI `--json` 输出解析）→ 表格/卡片；解析失败时退化为 `<pre>` 原文展示，保证信息不丢；
- **Cloud 面板**：节点卡片列表（点开 → 下方/右侧 iframe 区）；内嵌高度撑满视口，TUI 终端为真实键盘操作目标；
- **Tunnels 面板**：每行提供"复制 forward 命令"按钮（`navigator.clipboard`），例如：
  `teamx tunnel forward <name> --local-port <p> --session <my>`；
- 无外部依赖，全部内联；主题持久化 `localStorage["teamx-console-theme"]`。

## 9. 安全

| 威胁 | 缓解 |
|---|---|
| 他人访问 console | 仅绑 127.0.0.1；启动随机 token；cookie HttpOnly + SameSite |
| 任意命令执行 | `/api/panels` 只接受白名单 id，参数为编译期常量；不拼接用户输入 |
| SSRF | `/api/cloud` 仅允许回环地址；URL 来自本地配置文件 + 页面校验 |
| 数据写坏 | 面板命令全只读；`sync` 用 `--no-advance`；console 自身只写 `.teamx/console.json` |

## 10. 实施计划（文件改动）

| 步骤 | 改动 |
|---|---|
| S1 CLI 接线 | `cli.rs` 加 `Console` 变体与 `ConsoleCmd`；`main.rs` 提前分发；`lib.rs` 注册 `pub mod console;` |
| S2 骨架跑通 | `console.rs`：HTTPS+token、登录/401/404（复用 ui 文案改品牌）、空工作台页；`cargo build` 通过 |
| S3 执行器 + 面板接口 | `run_panel` + `/api/panels/<id>` + 五个面板的白名单命令；逐一核对只读 |
| S4 数据渲染 | 前端 Teams/My Tasks/Events/Tunnels 渲染（`--json` 解析 + `<pre>` 兜底） |
| S5 Cloud | `.teamx/console.json` 读写 + `/api/cloud` + 反代（含 WS）+ iframe 嵌入 |
| S6 打磨 | Tunnels 复制按钮、受限提示、空态/错误态、主题细节 |

每步后 `cargo build -p teamx` 与 `cargo test -p teamx`（存量 117+ 需保持绿）；新增代码零 clippy warning。

## 11. 验证（草案）

1. 在工作空间 `cd ~/Desktop/nano激光/deliver/用研 && teamx console`，浏览器开 `https://127.0.0.1:9528/?token=...` → 登录页品牌 Teamx Console；
2. Teams/My Tasks/Events/Tunnels 内容与同目录命令行输出一致；
3. `.teamx/console.json` 加 `caicai headless http://127.0.0.1:19034` → Cloud 面板 iframe 打开可用（ttyd 可交互、headless 可加项目/建会话）；
4. 页面无任何写团队状态操作（跑前后对 server/DB 做 diff 无变化，console.json 除外）；
5. 误输 `?token=` 错值 → 401 登录页；未授权 `/api/cloud/../..` → 404。

## 12. 里程碑

1. 原型 HTML（第 3 步）：静态 mock 五面板 + Teamx Console 品牌 → 评审观感；
2. 开发 S1–S4：console 可真实显示成员信息；
3. 开发 S5–S6：Cloud iframe + 打磨 → 真机验证（东京/首尔/美国入口）。
