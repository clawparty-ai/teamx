# admin-console 改进：Dashboard 地图/节点卡 + Timeline 性能与活动详情

> admin-console = 原 `teamx ui`。本文为设计定稿；会话记录：`docs/design/admin-console-improvements.md`
> 状态：**设计定稿（可实施）**；更新：2026-09-04

## 1. 目标（两块）

1. **Dashboard 地图/节点**：顶栏三档「🗺 地图 / Map / 🕸 拓扑」；地图区域固定 16:9；节点卡信息更丰富（+IP、+LLM）。
2. **Timeline**：activity 很多时加载慢/超长页 → 分页+虚拟滚动+日期筛选+分块；每个活动展示更多信息（如 bash 命令与输入输出，hover/展开详情）。

## 2. 决策摘要

| # | 决策 |
|---|---|
| 1 | Dashboard 顶栏三档：**地图**(高德) / **Map**(Leaflet+OSM→Esri) / **拓扑**（现状）；切换=Leaflet 换底图图层；离线/瓦片失败→降级内嵌 SVG 节点视图 |
| 2 | **地图/拓扑区域固定 16:9**：容器用 `aspect-ratio:16/9; width:100%`，内部 `overflow:hidden`；浏览器缩放高度随宽度推导、画面比例不变 |
| 3 | 节点卡/弹层字段：名称+角色+状态点、城市、**最后连接 IP**、**使用 LLM(model/provider 最近)**、sessions/工具/终端、cost/tokens、live |
| 4 | IP 来源：最近 `member_connections.ip`；LLM 来源：窗口内最新 `activity.model_id/provider_id`；缺→"—" |
| 5 | Timeline：服务端分页/游标 + 日期筛选；前端滚动加载 + **虚拟列表**；按时间分块（天/会话段）分组；甘特仅用于会话/成本聚合 |
| 6 | 活动详情：采集 tool 完成态 `{tool, state, arguments(输入), output(输出,截断~2KB), exit?}`；落库大小上限 |
| 7 | 详情 UX：hover 摘要气泡；点击行内展开（只读 pre，大输出折叠）；命令类高亮 🔧/⌨ |

## 3. 数据现状与改造点

### 3.1 节点卡数据（Rust: ui.rs `dash_nodes`）
- 现状字段：`member_id/name/role/state/city/lat/lng/sessions/running_tools/open_terminals/cost/tokens/live`。
- 新增：
  - `ip`：按 `member_id` 查最近 `member_connections`（`disconnected_at` 可为空/最近连接）的 `ip`。
  - `model`/`provider`：窗口内 `activity` 该 member 最新一条 `model_id`/`provider_id`（按 `started_at` desc）。
- server 节点可给 `ip` = server 地址（可留空显示"—"）。

### 3.2 地图三档（JS: ui.rs）
- 现状：`dashMode ∈ {map,topo}`；`dashBasemaps=[Gaode,Esri,OSM]` failover；`renderDashMap` Leaflet；`renderDashSvg` fallback/topology 共用。
- 改法：
  - `dashMode ∈ {gaode, osm, topo}`，顶栏 3 按钮（地图=高德 / Map=OSM→Esri / 拓扑）。
  - gaode/osm 都走 `renderDashMap`：gaode → `dashBasemaps=[Gaode]`；osm → `[OSM, Esri]`；仍保留 tileerror 时降级 SVG。
  - topo → `renderDashTopo`（现状）。
  - 容器 `<div id="dashMapWrap" style="aspect-ratio:16/9">` 同时包 map 与 topo；Leaflet `invalidateSize()` 需在布局稳定后调用。

### 3.3 Timeline（Rust: ui.rs + activity.rs / 渲染 JS）
- 现状：一次取全量（很可能无分页）；页面 DOM 全量。
- 改法：
  - 服务端 API：`/api/activity`（新）支持 `?from&to&kind&member&team&limit&offset`（或时间游标），返回 `{rows, has_more, next}`；行含 `id,kind,detail,model_id,provider_id,started_at,duration_ms,cost,tokens*`。
  - 复用/增强现有 `api_rows`/`api_timeline` 或新增分页端点。
  - 前端：**虚拟列表**（固定行高 + 视口切片），滚动到底加载下一页；顶部日期筛选（今天/近 7 天/自定义 from~to）与类型/成员筛选。
  - 分块：按 `started_at` 分块标题（日期 / 会话段），块内渲染该区间的行（只渲染可见块）。

### 3.4 活动详情采集（TS: opencode-plugin）
- 现状（`index.ts`）：`tool_call` 每次 `part.updated` 都 record，`detail={tool,state,arguments:input}`；无输出。
- 改法：
  - 仅在工具**完成态**（`state.status==="completed"`）记录一条；`detail = { tool, state:"completed", arguments: input, output: state.output }`。
  - 若运行态也想留痕可保留 running 事件但**不重复**（同一 callID 只记 completed）。
  - 控制台命令 `command.executed` 仍记 `{name,args}`（无输出时该事件无 output）。
- 大小/安全：`output` 在前端/服务器截断（如 2KB/4000 字符）；detail JSON 落库上限（截断）；不做明文 secret 脱敏（由 opencode 权限控制已有命令批准）；UI 只读展示。

## 4. UI 结构改动点（ui.rs）

- 顶栏：`data-dmode` 三个按钮；映射 `setDashMode`。
- CSS：`#dashMapWrap{aspect-ratio:16/9;width:100%;position:relative;overflow:hidden}`；`.dash-map{position:absolute;inset:0}`。
- 节点卡 popup：加 `IP`、`LLM` 行。
- 拓扑卡片（`renderDashSvg` 卡）：加 IP/LLM 行。
- Timeline：重构渲染函数为「头部筛选 + 虚拟列表容器 + 加载更多」；详情行内展开 `<details>`/折叠 pre。
- hover 摘要：`title` 或自定义 tooltip，显示 tool/命令摘要。

## 5. 服务端 API

- `/api/map`：返回 nodes 增补 `ip`、`model`、`provider`。
- Timeline 数据：新增 `/api/activity`（分页+筛选）或扩展现有；字段含详情 JSON。
- `activity` 表：现有 `detail TEXT`（JSON）足够；无需 schema 变更（输出截断后存 detail）。可选加 `has_output` 标志（不必须）。

## 6. 测试与验收

1. Dashboard：三档切换正常；断网/瓦片失败降级 SVG；resize 后地图仍是 16:9（画面不拉伸变形）。
2. 节点卡：member 节点显示 IP 与最近 LLM；缺失字段显示"—"。
3. Timeline：注入大量 activity（>1000 行）→ 首屏快、滚动加载下一页、无整页长 DOM；日期/类型/成员筛选生效。
4. 详情：一条 `tool=bash` 的 activity hover/展开能看到命令与输出（输出截断合理）；无输出的显示"（无输出）"。
5. 采集：同一工具调用不产生重复 completed 行；输出截断阈值生效。

## 7. 实现落点

| 模块 | 改动 |
|---|---|
| `crates/teamx/src/ui.rs` | dash_nodes 增 ip/model/provider；地图三档 + 16:9 容器；Timeline 分页 API + 前端虚拟列表/筛选/行内详情；节点卡与 hover |
| `crates/teamx/src/activity.rs` | 新增分页查询 helper（按 team/member/time/kind + limit/offset） |
| `crates/teamx/src/commands.rs` | 新增 `/api/activity` 对应的 `cmd_*`（或在 ui.rs 直接处理） |
| `opencode-plugin/src/index.ts` | 工具完成态一次性 record input+output |
| 文档 | 本文件 + `docs/35` admin-console 段同步 |

## 8. 关联

- `crates/teamx/src/ui.rs`、`activity.rs`、`db.rs`
- `opencode-plugin/src/index.ts`、`activity.ts`
- `docs/design/admin-console-improvements.md`

## 9. 节点位置维护（AI 提供，非 geoip）

admin-console 的地图/拓扑坐标**不依赖任何 geoip 库**，读 `<teamx_home>/ui-locations.json`（server 上为 `/root/.teamx/ui-locations.json`）：

```json
{
  "server": { "city": "Hong Kong", "lat": 22.3193, "lng": 114.1694 },
  "members": {
    "zhongm-worker": { "city": "Tokyo", "lat": 35.6762, "lng": 139.6503, "ip": "35.77.5.99" }
  }
}
```

规则（保持简单，不加命令）：
- 每个 member 一条记录，含 `city`（必要时 `lat/lng`），并可选绑 `ip`；
- **IP 不变 → 不更新**；**IP 变 → 由 teamx agent 用 AI 判定新城市/坐标并更新**该记录与 `ip` 字段；
- 无 `ip` 的条目（本地/个人）视为与 IP 无关，保持不变。
