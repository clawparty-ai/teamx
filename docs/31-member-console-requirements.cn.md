# Teamx Member Console（成员端 Web 控制台）需求分析

> 配套流程：本文件为第 1 步（需求分析）→ 之后是第 2 步设计文档 → 第 3 步 HTML 原型 → 第 4 步开发。
> 状态：**需求分析（待评审）**
> 目标读者：维护者 / 产品决策人

---

## 1. 背景

teamx 已有两套面向「owner/lead」的界面：

- `teamx ui`（Teamx Enterprise）：owner 专属的活动分析看板，读 serve 机本地 DB，HTTPS + token 登录；
- `teamx gui-member`：成员端桌面托盘面板（egui），通过 CLI 子进程做 invite 导入 / 隧道映射 / SOCKS5。

普通成员日常仍然主要靠 `teamx` CLI 与 opencode 插件。缺少一个**和 teamx ui 同风格、但成员视角**的本地 Web 工作台：让成员在浏览器里看一眼「我在哪个团队、目标是什么、我有什么任务、团队发生了什么、我能访问哪些隧道/云节点」。

同时我们已经在云节点上铺好了 opencode headless + ttyd(TUI)（见 `30-cloud-node-opencode-headless-tui.cn.md`），并把它们经 teamx tunnel 映射到成员本地端口（如 `127.0.0.1:19041`）。成员 console 可以顺带把这些本地已映射的页面**用 iframe 嵌进来**，形成「团队信息 + 我自己的云节点工作台」一体视图。

## 2. 目标与非目标

### 2.1 目标

1. 提供一个**本地运行、浏览器访问**的成员 Web 控制台，登录体验与 `teamx ui` 一致（HTTPS + 启动 token）。
2. **只读查看**：成员看到的都是「我在团队中的信息」，不做管理操作、不改团队状态。
3. 数据**与 `teamx` CLI 所见一致**（点击面板 → 调用 CLI → 展示结果），不另造数据通道。
4. UI **复用 teamx ui 的页面布局 / 菜单 / 风格**，但品牌标识为 **"Teamx Console"**。
5. 支持在页面里用 **iframe 嵌入**已通过 tunnel 映射到本地的云节点（headless 与 ttyd TUI）。

### 2.2 非目标（本次不做）

- 不做 owner 管理功能（审批成员 / 改角色 / 关目标 / 分析图表）。
- 不做成员写操作（完成任务 / 关闭任务 / 隧道 expose / forward 等——除非后续阶段明确加入）。
- 不做实时推送；全部点击触发。
- 不改成服务端部署 / 公网访问（个人本地运行）。

## 3. 角色与术语

| 术语 | 含义 |
|---|---|
| member console | 本文设计的成员端 Web 控制台，命令暂定 `teamx console` |
| 当前工作空间 DB | `<cwd>/.teamx/teamx.db`（项目级 teamx 空间，见 `29-teamx-project-scoped-issue.cn.md`）；网络模式下未缓存的团队数据经 CLI RPC 从 server 获取 |
| CLI 等价信息 | 在该工作空间运行 `teamx ...` 命令能看到的同一套信息 |
| 云节点 | 部署了 opencode headless + ttyd 的远程 worker 主机 |
| 云节点入口 | 经 `teamx tunnel forward` 映射到本地的 headless Web / ttyd TUI 地址 |

## 4. 用户场景

- **S1 个人成员**（如 zhongm/caicai/zhaoz，在个人工作空间启动 console）：查看自己所在团队的目标/角色/任务、团队事件、可用隧道，并在同一页面打开自己的云节点（headless / ttyd）。
- **S2 lead**（caishu，在某个成员视角下）：用 console 快速看团队概况 + 直达任意云节点 Web（headless 聊天 / ttyd 终端），不必记 6 个端口。

## 5. 功能需求

### 5.1 启动与访问

| 编号 | 需求 | 期望行为 |
|---|---|---|
| R1 | 本地启动 | 在成员工作空间执行 `teamx console`，服务绑定 `127.0.0.1`（默认本机回环），不监听公网 |
| R2 | token 登录 | 与 `teamx ui` 一致：启动时打印一次性 token；首次访问需输入 token（cookie 记住）；支持 `?token=` 直达 |
| R3 | 身份随工作空间 | console 的数据视角 = 启动目录的 `.teamx` 空间（哪个成员/团队由该空间的 letters/DB 决定） |

### 5.2 数据

| 编号 | 需求 | 期望行为 |
|---|---|---|
| R4 | 数据源 = 当前工作空间 DB | 面板查询与「在当前目录跑 `teamx` CLI」等价：本地信息读 `<cwd>/.teamx/teamx.db`，网络信息经 CLI/证书从 server 取，不引入独立数据层 |
| R5 | 点击触发 | 每个面板/操作都是「点一下 → 跑一次 CLI → 渲染」；不做轮询/实时刷新 |
| R6 | 权限边界 | 只呈现 CLI 权限内的信息（例如我所在团队、指派给我的任务、我可访问的隧道）；不含 owner 私密/全局管理数据 |

### 5.3 面板（MVP 只读）

| 编号 | 面板 | 期望展示（等价 CLI 信息） | 说明 |
|---|---|---|---|
| R7 | Teams | 我所在团队列表、当前目标（title/state）、我的角色/状态 | 对应 `teamx team list` 等 |
| R8 | My Tasks | 指派给我的任务（title/state/优先级/说明） | 对应 taskx `task list`（我相关） |
| R9 | Events | 我所在团队最近事件（progress/decision/update…） | 对应 `teamx sync` / events |
| R10 | Tunnels | 我可访问的隧道（ttyd/headless 等）及本地映射状态 | 对应 `teamx tunnel list` |
| R11 | Cloud 节点（iframe） | 以 iframe 内嵌本地已映射的 headless / ttyd 地址 | 用户先 tunnel forward，再把本地端口加入 console 的节点清单 |

> MVP 各面板全部只读：查看 = 运行对应只读 CLI。

### 5.4 风格与品牌

| 编号 | 需求 | 期望行为 |
|---|---|---|
| R12 | 复用 teamx ui 风格 | 页面布局 / 顶部导航 / 主题切换 / 卡片风格与 `teamx ui`（Teamx Enterprise）一致 |
| R13 | 品牌标识 | 标题、logo-text、登录页/404 页显示 **"Teamx Console"**，替代 "Teamx Enterprise" |
| R14 | 亮/暗主题 | 支持亮/暗主题切换并持久化，行为与 teamx ui 一致 |

### 5.5 与云节点协作

| 编号 | 需求 | 期望行为 |
|---|---|---|
| R15 | 云节点清单可维护 | 本地保存用户自定义的云节点入口（名称 + 本地 URL），如 `caicai headless → http://127.0.0.1:19034` |
| R16 | iframe 嵌入 | 点击某云节点入口 → 在控制台内以 iframe 打开对应 URL（headless 或 ttyd）；注意 iframe 可用性（无 X-Frame-Options 限制），必要时由 console 代理以剥离限制头 |
| R17 | 读取隧道映射提示 | 若用户尚未 forward，Tunnels 面板给出「本地 190xx 端口可用的转发命令」提示（只提示，不代执行） |

## 6. 非功能需求

| 编号 | 需求 | 约束 |
|---|---|---|
| N1 | 安全 | 仅绑定 127.0.0.1（默认）；token 登录；会话 cookie 仅本机有效；不引入额外监听 |
| N2 | 依赖 | 前端自包含（不依赖外部 CDN/网络），同 teamx ui 的做法 |
| N3 | 平台 | 纯 Rust 本地服务 + 浏览器；macOS/Linux 一致（沿用 ui.rs/axum 栈） |
| N4 | 性能 | 单用户查询 < 数百 ms；子进程 CLI 调用带超时与错误展示 |
| N5 | 无侵入 | 不修改团队/成员/目标状态（只读原则）；不写任何业务表 |
| N6 | 可维护 | 风格/组件尽量与 teamx ui 共享或对齐，避免大段重复代码失同步 |

## 7. 已确认的决策（来自讨论）

1. **数据源**：当前工作空间 DB（`.teamx` 下的 sqlite）；后端按「点击 → 调 CLI」取数，与 teamx CLI 信息一致。
2. **只读**：MVP 只做查看，不做写操作。
3. **部署**：personal 成员本地运行与访问；本机自己用（仍需 token 登录，同 teamx ui）。
4. **无实时**：全部点击触发。
5. **风格**：复用 teamx ui 布局/菜单；品牌 "Teamx Console"。
6. **云节点集成**：经 tunnel 映射到本地后可用 iframe 内嵌（headless / ttyd）。
7. **流程**：先需求分析 → 设计文档 → HTML 原型 → 开发。

## 8. 开放问题（供评审确认）

1. 命令名：`teamx console`？还是 `teamx ui-member` / `teamx member-ui`？（本文暂用 `teamx console`）
2. MVP 面板取舍：先做 **Teams / My Tasks / Events / Tunnels / Cloud(iframe)** 五块是否足够？Goals 与 Repos 放后续？
3. iframe 限制：如 headless Web 带 `X-Frame-Options`/CSP 导致无法嵌入，是否接受「console 本地反向代理该端口以嵌入」的方案（会增加一个小代理）？
4. Cloud 节点入口配置保存位置：沿用 `.teamx/console.json`（项目级）还是用户级 `~/.config/teamx/console.json`？
5. 是否在 MVP 中提供「一键复制 tunnel forward 命令」的只读辅助（不代执行）？
6. 登录失败/401 页面与登录页文案是否直接复制 teamx ui 但把品牌换成 Teamx Console？

## 9. 验收标准（草案）

- [ ] 在工作空间运行 `teamx console`，浏览器打开 `https://127.0.0.1:<port>/?token=...` 可登录进入主页。
- [ ] 页面头部显示 "Teamx Console"；导航/布局/主题切换与 teamx ui 观感一致。
- [ ] Teams / My Tasks / Events / Tunnels 能展示与该工作空间 CLI 一致的只读信息。
- [ ] 在 Cloud 面板添加一个本地映射地址后，可 iframe 打开（或得到明确的受限提示）。
- [ ] 全流程无对团队/成员/目标状态的写操作（可用前后 DB diff 验证）。
- [ ] `cargo build -p teamx` 与相关单测通过；新增代码不引入 clippy warning。

---

## 附：关联材料

- `30-cloud-node-opencode-headless-tui.cn.md`：云节点 headless + ttyd + tunnel 部署
- `docs/prototype/ui-details.md`：teamx ui 原型元素规格（风格基准）
- `29-teamx-project-scoped-issue.cn.md`：项目级 `.teamx` 空间（数据源约定）
- `crates/teamx/src/ui.rs`：teamx ui 实现（HTTPS+token、布局风格可复用）
- `crates/teamx/src/gui_member_panel.rs`：成员端「CLI 子进程」架构先例
