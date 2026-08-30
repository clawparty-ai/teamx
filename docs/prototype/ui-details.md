# Teamx Enterprise — HTML 原型元素规格文档

> 用途：逐页逐元素列出 **数据来源 / 显示逻辑 / 功能介绍**，供后续核对研发细节使用。
> 版本基准：`docs/prototype/`（中文版）与 `docs/prototype-en/`（英文版）当前实现。
> 原型均为 **mock 数据**：JS 中 `jget(path)` 拦截 `/api/*` 请求，返回 `MOCK` 内对应数据。下文「数据来源」中 `API:` 为该元素的真实接口约定，`MOCK:` 为原型实际取值结构。

---

## 0. 总览

### 0.1 页面清单

| 页面 | 文件 | 路由 | 说明 |
|---|---|---|---|
| 登录页 | `index.html` | `/` | 输入 access token 登录 |
| 主工作台 | `dashboard.html` | `/dashboard.html?token=...` | 含 5 个视图（Dashboard/KanBan/Cost/Timeline/Members），Dashboard 为登录默认视图 |
| 成员实时监控 | `members-live.html` | `/members-live.html` | 独立页，实时会话流 + 详情 |
| 404 | `404.html` | 任意不存在路径 | 404 提示 + 5s 倒计时跳转 |

### 0.2 Mock 数据模型（`jget` 路由 → 结构）

| API | 结构 | 说明 |
|---|---|---|
| `/api/teams` | `{ teams: [{ id, name }] }` | 团队列表 |
| `/api/goal` | `{ current: { id, title, state, body, created_at, closed_at }, goals: [同 current], lifecycle: [{ type, member_id, created_at, payload }] }` | 当前目标 + 历史 + 生命周期事件 |
| `/api/timeline` | `{ items: [{ member_id, node_id, node_name, kind, started_at, ended_at, duration_ms, has_human, tokens_in, tokens_out, cost, detail }] }` | 团队活动时间线（全事件） |
| `/api/overview` | `{ overview: { duration_ms, tokens_input, tokens_output, tokens_reasoning, cost, active_members, active_nodes }, human: { count }, ai: { count }, overall: { count } }` | 汇总统计 |
| `/api/by_member` | `[{ member_id, duration_ms, tokens_input, tokens_output, cost, count }]` | 按成员聚合 |
| `/api/by_node` | `[{ node_id, node_name, duration_ms, tokens_input, cost, count }]` | 按节点聚合 |
| `/api/tools` | `[{ tool, count }]` | 工具使用次数 |
| `/api/files` | `[{ file, count }]` | 文件编辑次数 |
| `/api/members` | `{ members: [{ id, display_name, role, state, joined_at, last_seen_at, activity: { duration_ms, tokens_input, cost, count } }] }` | 成员信息 + 活动汇总 |
| `/api/kanban` | `{ columns: [{ display_name, role, summary: { duration_ms }, tasks: [{ member_id, node_name, kind, has_human, started_at, ended_at, duration_ms, tokens_in, tokens_out, cost, detail }] }] }` | 看板（按成员分列） |
| `/api/map` | `{ nodes: [{ id, name, kind(server/member), member_id, member_name, role, state, region, city, lat, lng, sessions, cost, tokens, latency_ms, ip }] }` | Dashboard 地图模式节点（经纬度 + 状态 + 活动） |
| `/api/topology` | `{ server: { id, name, version, status, region, addr }, nodes: [{ id, name, member, role, state, region, addr, live: [{ icon, text }] }], links: [{ from, to, type, name, status, detail }] }` | Dashboard 拓扑模式节点与连线（mTLS WS / reverse tunnel / SOCKS5-tun0）；`nodes[].live` 为节点实时活动流 |

`kind` 取值：`work_session` / `tool_call` / `step_finish` / `file_edit` / `human_input` / `human_approval` / `human_command` / `command`。

### 0.3 通用约定

- 主题：亮/暗双主题，CSS 变量驱动（`--primary`/`--card`/`--border` 等），右上角 ☀️/🌙 切换并持久化到 `localStorage["teamx-theme"]`。
- 全宽布局：容器 `.container` 默认 `max-width:1280px`；切到 Dashboard/KanBan/Cost/Timeline/Members 时加 `.full-width` 类铺满视口（`applyViewLayout`）。
- 时间范围：全局 `timeState`（quick/absolute），影响所有数据类视图的 `/api/*?from=&to=` 参数。
- 导航：`showView(name)` 切换 `.view.active` + nav active 态 + 按需渲染。

---

## 1. 通用头部 / 工具栏（dashboard.html）

### 1.1 顶部导航 header

| 元素 | 选择器 | 数据来源 | 显示逻辑 | 功能介绍 |
|---|---|---|---|---|
| Logo | `.logo-icon` / `.logo-text` | 静态 | 固定显示 "Tx" + "Teamx Enterprise" | 品牌标识 |
| 导航按钮 | `nav button[data-view]` | 静态（可见 5 个：Dashboard/KanBan/Cost/Timeline/Members；另有隐藏的 Goal，`display:none`） | 当前视图加 `.active`（primary 色 + 下划线）；Dashboard 为默认激活视图；Goal 按钮隐藏未启用 | 切换视图 |
| 主题切换 | `#themeToggle` | 静态 | ☀️/🌙 图标随主题切换 | 亮暗主题切换，持久化 |
| 用户信息 | `.user-info .user-avatar` + span | 静态 "Owner" | 固定 | 当前用户标识 |

### 1.2 工具栏 toolbar

| 元素 | 选择器 | 数据来源 | 显示逻辑 | 功能介绍 |
|---|---|---|---|---|
| Team 选择器 | `#team` | `API:/api/teams` → `MOCK.teams.teams[]` | 默认"All" + 团队名选项；变更时重置目标筛选并 `refresh()` | 按团队过滤所有数据 |
| Goal 选择器 | `#goal` | `API:/api/goal` → `MOCK.goal.goals[]`（`fillGoalSelect` 填充） | 默认"All"；选择后 `selectedGoalId` 生效，影响各视图数据范围 | 按目标过滤 |
| 时间选择器 | `#timeBtn` + `#timePopover` | `timeState` 全局状态 | 按钮显示当前范围（如 "Last 30 days"）；Quick/绝对时间两种面板 | 设置全局时间窗口，改变 `/api/*` 的 from/to 参数 |
| Timeline 视图切换 | `#tlViewToggle` | 静态（甘特图/时间轴两按钮） | 仅 Timeline 视图激活时显示；激活项高亮 | 切换 Timeline 页的甘特图/鱼骨图两种呈现 |
| Dashboard 模式切换 | `#dashModeToggle` | 静态（地图模式/拓扑模式两按钮） | 仅 Dashboard 视图激活时显示；激活项高亮；默认地图模式 | 切换 Dashboard 页的地图/拓扑两种呈现（`setDashMode`） |

---

## 2. 登录页 index.html

| 元素 | 选择器 | 数据来源 | 显示逻辑 | 功能介绍 |
|---|---|---|---|---|
| 标题 | `h1` | 静态 "Teamx Enterprise" | 固定 | 品牌 |
| 副标题 | `.subtitle` | 静态 "Enter your access token to login" | 固定 | 引导 |
| token 输入框 | `#tokenInput` | 用户输入 | placeholder "Paste token here..."；`autofocus` | 输入 access token |
| 登录按钮 | `#loginBtn` | — | 空 token 时聚焦输入框不跳转；点击后显示 "Logging in..." 并禁用 | 校验非空后跳转 `dashboard.html?token=<token>` |
| 错误提示 | `#errMsg` | 静态 | 默认隐藏，仅在 JS 校验失败时展示（当前仅做非空校验，真实校验需后端） | 占位错误提示 |

---

## 3. Dashboard 视图（view-dashboard）

入口：nav Dashboard（**登录默认视图**）。含两个模式：**地图模式**（Leaflet + OSM 真实瓦片）与**拓扑模式**（SVG 网络拓扑），由 `#dashModeToggle` 切换。

### 3.1 统计卡

| 元素 | 选择器 | 数据来源 | 显示逻辑 | 功能介绍 |
|---|---|---|---|---|
| 统计卡容器 | `#dashCards` | `API:/api/map` + `API:/api/topology` + `API:/api/overview` | 4 张卡：在线成员（x/y）/ 活跃会话 / 活跃隧道 / 总成本 | 团队实时概览 |
| 在线成员 | 卡片 1 | `map.nodes[].kind==member && state∈{online,busy}` 计数 | `在线数 / 成员总数` | 成员在线占比 |
| 活跃会话 | 卡片 2 | `map.nodes[].sessions` 求和 | 数字 | 当前活跃会话总数 |
| 活跃隧道 | 卡片 3 | `topology.links[]` 中 type 含 tunnel/SOCKS5 且 status=up 计数 | 数字 | 活跃反向隧道/代理数 |
| 总成本 | 卡片 4 | `overview.overview.cost` | `$x.xxxx` | 总成本 |

### 3.2 地图模式（`#dashMap`）

| 元素 | 选择器 | 数据来源 | 显示逻辑 | 功能介绍 |
|---|---|---|---|---|
| 地图容器 | `#dashMap` | 静态 560px 高 + Leaflet | `L.map` 居中中国 [34.5,108.5] zoom 5 + 多底图瓦片层（`addBasemap`） | 真实地图渲染 |
| 底图服务 | `BASEMAPS[]` | 静态（3 个 provider） | 默认 **高德地图**（`webrd0{1-4}.is.autonavi.com`，GCJ-02）；`tileerror` 累计超 12 次自动降级：高德 → Esri 街道图 → OpenStreetMap；全部失败才显示 `#dashMapEmpty` | 国内/国外均可用的底图，自动 failover |
| 节点标记 | `.map-pin`（`L.divIcon`） | `API:/api/map` → `nodes[]` | 按 `mapState` 着色：online 绿 / busy 橙 / offline 灰 / server 蓝；钉形气泡显示成员首字母（server 显示 "S"） | 在地图上标出成员/服务器位置 |
| 标记弹窗 | `.leaflet-popup` | 节点字段 | 节点名 + 角色 badge + 区域/会话/成本/延迟/IP 键值 | 节点详情 |
| 离线降级 | `#dashMapEmpty` | 静态 | `window.L` 未定义（Leaflet CDN 加载失败）或全部底图 provider 的瓦片均失败时显示 | 离线兜底提示 |
| 暗色适配 | `.dark #dashMap .leaflet-tile` | 静态 | CSS `filter` 反色 | 暗色主题下瓦片降亮 |

**显示逻辑补充**：`dashMarkers` 为 `L.layerGroup`，每次 `renderMap` 先 `clearLayers()` 再重加，避免定时刷新重复堆叠标记；底图 `addBasemap` 每次切换 provider 时移除旧 `dashTileLayer` 并重置 `dashTileErr`。高德 GCJ-02 与 WGS-84 有数百米偏移，在整国/城市缩放级别下不可见，故标记不做坐标转换。

### 3.3 拓扑模式（`#dashTopo`）

| 元素 | 选择器 | 数据来源 | 显示逻辑 | 功能介绍 |
|---|---|---|---|---|
| 拓扑容器 | `#dashTopo` | `API:/api/topology` → server/nodes/links | SVG `viewBox 0 0 1200 540`，server 居中，成员节点环绕，公网出口节点在侧 | 网络拓扑图 |
| server 节点 | `<foreignObject>` → `.topo-node-box.server` | `topology.server` | 蓝色描边方框：头部（状态点 + 名称 + Tx）+ meta（版本·区域 / 地址） | 中心服务器 |
| 成员节点 | `<foreignObject>` → `.topo-node-box` | `topology.nodes[]`（非 external） | 方框：头部（状态点 + 成员名 + 角色 badge + 节点名）+ 实时活动区；边框色按状态（online 绿/busy 橙/offline 灰） | 成员节点 |
| 节点实时活动 | `.tnb-live` / `.tnb-line` | `nodes[].live[]`（`{icon,text}`） | 方框内最多 **5 行**，初始取 `live.slice(0,5)`；定时器（`spawnTopoActivity`，每 3s）随机选一个在线节点 prepend 新行并裁剪到 5 行，新行带淡入动画；离线节点显示空态 | 展示节点当前正在执行的任务（滚动） |
| 公网出口 | `<foreignObject>` → `.topo-node-box.external` | `nodes[]`（state=external） | 虚线描边方框 + "internet" + 区域 | 透明代理公网出口 |
| 连线 | SVG `<path>` | `topology.links[]` | mTLS WS 蓝 / reverse tunnel 紫 / SOCKS5-tun0 橙；down 虚线 + 降透明；非 mTLS 带箭头；端点按 `clipEdge` 裁剪到方框边界 | 连接关系 |
| 连线标签 | SVG `<text>` | `links[].type`/`name` | 中点上方标注 `type · name` | 连线类型 |
| 图例 | `.topo-legend` | 静态 | mTLS WS / reverse tunnel / SOCKS5-tun0 / down（虚线） | 连线图例 |

**显示逻辑补充**：`topoPos` 以 `TOPO_POS`（固定 id→坐标）布局；`topoBoxSize` 给出各节点方框尺寸（member 368×122 / server 170×74 / external 122×50），连线端点经 `clipEdge` 裁剪到方框边界；srv↔n1 的 mTLS 与 reverse tunnel 反向边通过二次曲线（`edgePath` curve=34）错开避免重叠；`renderTopology` 以 `lastTopoData === data && el.dataset.ready==="1"` 守卫，刷新时跳过重建以保留实时活动流；节点/连线 hover 用 `<title>` 显示详情。

---

## 4. KanBan 视图（view-kanban）

入口：nav KanBan。

| 元素 | 选择器 | 数据来源 | 显示逻辑 | 功能介绍 |
|---|---|---|---|---|
| 看板容器 | `#kanbanCols` | `API:/api/kanban?team&from&to` → `MOCK.kanban.columns[]` | 网格 `repeat(auto-fit,minmax(280px,1fr))`，每列一个成员 | 按成员分列展示工作会话 |
| 列头标题 | `.kb-col-title` | `column.display_name` + `column.role` | 成员名 + 角色 badge（contributor 绿 / reviewer 紫 / observer 橙） | 标识该列成员 |
| 列头 meta | `.kb-col-meta` | `column.summary.duration_ms`、`tasks.length`、`tasks[].has_human`、`tasks[].cost/tokens` | `工作时长 · N tasks · M with human · ⚡ tokens · $cost`；成本为列内任务求和 | 列级汇总（含成本） |
| 任务卡片 | `.kb-card` | `tasks[]`（`renderTaskCard`） | 每卡片含 head（成员名+human-in-loop/auto badge）、body（描述或 Worked Xh）、meta（时间区间 + cost） | 展示单个 work_session |
| 卡片 meta 成本 | `.kb-meta .cost` | `task.tokens_in/tokens_out/cost` | `⚡ <total tokens> tok · $<cost>`，橙色；过长自动换行（`overflow-wrap:anywhere`） | 展示本次会话 token 消耗与费用 |
| 悬停详情 | `.kb-tip` | `task.detail` 各字段 + tokens/cost | 默认隐藏；hover/focus 卡片时显示在**卡片下方**（`top:calc(100%+10px)`）；内含 member/node/started/ended/duration/tokens/cost/human 等键值 | 会话详情 tooltip |
| 空状态 | `.kb-empty` | 该列无任务时 | "no work in this range" | 兜底提示 |

**显示逻辑补充**：最多渲染 12 个任务（`tasks.slice(0,12)`）；`has_human` 为 1 显示 human-in-loop。

---

## 5. Cost 视图（view-cost）

| 元素 | 选择器 | 数据来源 | 显示逻辑 | 功能介绍 |
|---|---|---|---|---|
| 统计卡片 | `#costCards` | `API:/api/overview` | 6 张卡：Total tokens / Input / Output / Reasoning / Cost / Avg cost per row | 成本总览 |
| 成员成本环形图 | `#chartCostDonut` | `API:/api/by_member` | SVG donut + 图例 | 成员成本占比 |
| 节点成本柱状图 | `#chartCostBars` | `API:/api/by_node` | SVG 柱状 | 节点成本对比 |
| Token 柱状图 | `#chartTokensBars` | `API:/api/by_member` | SVG 柱状 | 成员 token 消耗 |
| Human vs AI 环形图 | `#chartHumanDonut` | `overview.human/ai.count` | SVG donut | 人类/AI 动作占比 |
| 成本明细表 | `#costByMember` / `#costByNode` | `by_member` / `by_node` | 列含 In/Out tokens、Cost、Actions | 明细核对 |
| 工具使用表 | `#costTools` | `API:/api/tools` | Tool/Count | 工具使用统计 |
| 文件编辑表 | `#costFiles` | `API:/api/files` | File/Count | 文件编辑统计 |

---

## 6. Timeline 视图（view-timeline）

默认 **鱼骨图**（`tlMode="fishbone"`）。

### 6.1 视图切换

| 元素 | 选择器 | 数据来源 | 显示逻辑 | 功能介绍 |
|---|---|---|---|---|
| 切换控件 | `#tlViewToggle` | 静态 | 仅 Timeline 页显示；激活项高亮 | `setTimelineMode` 切换 gantt/fishbone 面板显隐并重渲染 |

### 6.2 甘特图 #gantt

| 元素 | 选择器 | 数据来源 | 显示逻辑 | 功能介绍 |
|---|---|---|---|---|
| 甘特图 SVG | `#gantt svg` | `API:/api/timeline` → items | 每成员一泳道；work_session 渲染为渐变蓝条；human 事件渲染为绿色小条；today 虚线标记；自适应刻度（hour/day/week/month） | 按成员 × 时间的工作会话横道图 |
| 泳道计数器 | svg text | 泳道内 `tool_call/step_finish/file_edit` 计数 | `N tools · N steps · N files` | 细粒度事件聚合 |
| 图例 | `.gantt-legend` | 静态 | work session / human action / today 三色 | 图例说明 |

### 6.3 鱼骨图 #fishbone（垂直）

| 元素 | 选择器 | 数据来源 | 显示逻辑 | 功能介绍 |
|---|---|---|---|---|
| 主轴 | `.fb-spine` | 静态 | 中央竖线 | 时间主轴（最早在上，最新在下） |
| 起点框 | `.fb-start-box` | `renderFishbone(title)` 参数，默认 "Start" | 顶部方框 + 方块图标，primary 色 | 时间轴起点标识 |
| 日期分隔 | `.fb-day` | item 日期变化 | 居中日期行（如 "Aug 27 · Wed"） | 按天分组 |
| 活动行 | `.fb-row` | items（全部 kind） | 卡片左右交替；圆点颜色按 kind；骨线连接主轴 | 展示团队全部活动 |
| 活动卡片 | `.fb-card` | item 各字段 | 时间/成员/kind badge/主文本/时长/节点/human-in-loop/cost | 单活动详情 |
| 卡片成本 | `.fb-cost` | `tokens_in/out`、`cost` | `⚡ N tok · $x.xx` 橙色 | 活动 token 消耗与费用 |

**显示逻辑补充**：`renderFishbone` 对 items 按 `started_at` 升序排序；无数据时显示 "no activity in this range"；每类事件（tool/step/file/human/work）都可成卡。

---

## 7. Members 视图（view-members）

| 元素 | 选择器 | 数据来源 | 显示逻辑 | 功能介绍 |
|---|---|---|---|---|
| 统计卡片 | `.view-members .cards` | 静态 | 4 张：在线成员(2/3)、活跃会话(4)、正在执行工具(2)、打开的终端(3) | 实时概览 |
| 成员列表 | `.member-item` | 静态（memberMeta 驱动） | 头像字母/姓名/角色 tag/状态点/会话数/节点/累计 cost；在线有 LIVE 徽标 | 选择成员查看详情 |
| 状态点 | `.status-dot` | `memberMeta.dot` | online 绿 / busy 橙 / offline 灰 | 成员在线状态 |
| 累计成本 | `.mi-sub .cost` | 静态（Alice $4.82 / Bob $4.51 / Carol $3.01） | 橙色 mono | 成员累计费用 |
| 空状态 | `#emptyState` | 静态 | 默认隐藏（`selectMember` 默认选中 Alice）；选中离线成员时显示 "😴 X 当前离线，无会话" | 无会话兜底 |
| 详情头部 | `.detail-header` | `memberMeta` | 成员 chip + 会话标题 + running tag + **cost tag** + 中断/发送消息/打开终端按钮 | 当前成员会话操作区 |
| 详情 cost | `#detailCost` | `memberMeta.cost`（如 ⚡ 12.4k tok · $0.36） | 橙色 tag，随选中成员切换；离线成员为空 | 本会话 token/费用 |
| Tab 栏 | `.detail-tabs button[data-dtab]` | 静态 | live/messages/terminal 三 tab，激活高亮 | 切换详情内容 |
| 实时活动 tab | `#dtab-live .live-line` | 静态初始 + **动态生成器** | 每 3-5 秒随机插入一条（时间/图标/文本），保留最新 12 条，带淡入动画 | 模拟成员实时工具调用流 |
| 消息 tab | `#dtab-messages` | 静态 | User/Assistant/Tool/Tool result 消息列表 | 会话消息 |
| 终端 tab | `#dtab-terminal` | 静态初始 + **动态生成器** | 每 2.5-5 秒追加一条命令+输出（保留 40 行，滚动到底） | 模拟实时终端 |
| 终端最大化 | `#termMax` + `.term-backdrop` | 静态 | 点击后 `.term-window.maximized` 全屏浮层（inset 24px）+ 半透明遮罩，图标 ⛶/🗕 切换；点遮罩还原 | 终端放大查看 |

**显示逻辑补充**：`boot()` 时 `selectMember(document.querySelector(".member-item"))` 默认选中第一个成员（Alice），因此进入页面即显示详情而非空状态。

---

## 8. members-live.html（独立实时监控页）

| 元素 | 选择器 | 数据来源 | 显示逻辑 | 功能介绍 |
|---|---|---|---|---|
| 导航 | `nav` | 静态 5 按钮（Members 激活） | 同主工作台样式 | 页面标识（无实际跳转） |
| 工具栏 | `.toolbar` | 静态 | 左侧 Team 选择；右侧 `auto-refresh 5s` + LIVE 红标 + 刷新按钮 | 模拟自动刷新 |
| 统计卡片 | `.cards` | 静态 | 在线成员/活跃会话/正在执行工具/打开的终端 | 实时概览 |
| 成员列表 | `.member-item` | 静态（memberMeta） | 同主工作台 Members 视图 | 选择成员 |
| 详情头部 | `.detail-header` | `memberMeta` | 含 cost tag | 会话操作 |
| 实时活动/消息/终端 tab | 同 dashboard | 静态 + 动态生成器 | 同主工作台 | 实时会话流、消息、终端（含最大化） |
| 终端说明 | `.mt16.muted` | 静态 | "该终端运行在 Alice 的机器（node-01），通过 mTLS 隧道实时连接…" | 终端能力说明 |

> 与 dashboard Members 视图功能一致，为独立页面形态（可被 iframe/独立路由嵌入）。

---

## 9. 404.html

| 元素 | 选择器 | 数据来源 | 显示逻辑 | 功能介绍 |
|---|---|---|---|---|
| 404 代码 | `.code` | 静态 "404" | 蓝色大号 mono | 错误码 |
| 标题/说明 | `h1` / `.msg` | 静态 | "Page Not Found" + 说明 | 提示 |
| 倒计时 | `#cd` | 静态（初始 5） | 每秒递减 | 倒计时结束后 `location.href="/login"` 跳转 |
| 立即登录链接 | `.hint a` | 静态 `/login` | 链接 | 手动跳转 |

---

## 10. 动态模拟（非数据来源，属于显示逻辑增强）

| 模拟 | 触发 | 规则 | 实现 |
|---|---|---|---|
| 实时活动生成 | `#dtab-live` 可见时 | 每 `3000+random()*2000` ms 插入一条，保留 12 条 | `spawnLiveActivity` / `scheduleLiveActivity` |
| 终端命令生成 | `#dtab-terminal` 可见时 | 每 `2500+random()*2500` ms 追加，保留 40 行 | `spawnTerminalLine` / `scheduleTerminalLine` |

---

## 附：成本（Cost）字段口径

- **活动级 cost**：`item.tokens_in + tokens_out`（token 数）与 `item.cost`（美元），显示 `⚡ N tok · $x.xx`。
- **聚合级 cost**：`/api/overview`、`/api/by_member`、`/api/by_node` 返回聚合值。
- **看板列级 cost**：前端对列内任务 `cost`/`tokens` 求和。
- **成员累计 cost**：静态展示（Alice $4.82 / Bob $4.51 / Carol $3.01），对应 `members[].activity.cost`。
