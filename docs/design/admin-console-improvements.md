---
status: completed
protocol_version: 1
owner: pre-team human leader (caishu)
created_at: 2026-09-04
updated_at: 2026-09-04
---

# admin-console 改进：Dashboard 地图/节点卡 + Timeline 性能与活动详情

## Context

admin-console = 原 `teamx ui`（owner/lead 企业看板）。本轮聚焦两块（用户提出）：

1. **Dashboard 地图/节点**：
   - 现状顶栏是「🗺 地图 / 🕸 拓扑」两个 `data-dmode`。其中"地图"= Leaflet + 瓦片链 Gaode→Esri→OSM 自动降级。
   - 目标：改成**三个**选项——「地图」(高德 Gaode) / 「Map」(Leaflet，OSM 等) / 「拓扑」(现状拓扑)；每个 member 节点卡信息更丰富（如增加 IP、使用的 LLM 等）。

2. **Timeline**：
   - 现状：activity/事件很多时页面加载慢、超长页滚动体验差。
   - 目标：重新设计加载与展示（性能 + 分页/虚拟滚动等）。
   - 另外：每个 activity 收集/展示更多信息，如「调用工具 bash」应能看到调的 bash 命令是什么、输入输出是什么（例如 hover 弹详情）。

### 已探明的事实（代码）
- 地图/节点：`ui.rs` 顶栏 `data-dmode="map"/"topo"`；`renderDashMap()` 用 Leaflet + `dashBasemaps`（Gaode/Esri/OSM 顺序 failover），无 Leaflet 时降级 SVG(`renderDashSvg`)；`renderDashTopo()` = 拓扑。
- 节点数据：`dash_nodes()` 输出 `{member_id,name,role,state,city,lat,lng,sessions,running_tools,open_terminals,cost,tokens,live}`——**没有 ip、没有 model/LLM**。
- Timeline：activity 存在 DB `activity` 表（含 `kind,detail,model_id,provider_id,tokens*,cost`）。
- 插件采集：`tool_call` 的 `detail={tool,state,arguments}`（含命令等输入）；`command.executed` 的 `detail={name,args}`；**未记录工具输出/结果**。
- 展示：admin-console 的 Timeline/活动表格只显示摘要列，未见 hover 详情；详情 JSON 存于 `activity.detail`。

### 需要 FR 确认
- FR-0001：opencode plugin 事件里能否拿到**工具输出/结果**（完成态 part 是否含 output/result）？决定 activity 详情能否补"输出"。本地 `@opencode-ai/plugin` 类型待核；若插件事件不可得，需决定替代采集或降级（只展示输入/命令，不展示输出）。

## Settled Decisions

| DQ | 决策 |
|---|---|
| A1 三模式 | Dashboard 顶栏改三档：**地图**(高德) / **Map**(Leaflet+OSM→Esri) / **拓扑**(现状)；切换=换 Leaflet 底图图层；瓦片不可用→降级内嵌 SVG 节点视图；**地图区域固定 16:9 宽高比**（浏览器缩放时不变，容器 overflow） |
| A2 节点卡 | member 节点卡/弹层含：名称+角色+状态点、城市、**最后连接 IP**、**使用 LLM(最近 model/provider)**、sessions/工具/终端、cost/tokens、live |
| A3 数据源 | IP=最近 `member_connections.ip`；LLM=窗口内最新 `activity.model_id/provider_id`；缺则"—"，纯读不改采集 |
| B4 加载 | Timeline 服务端按时间窗+条数分页/游标；前端滚动加载+虚拟列表；支持**日期筛选** |
| B5 展示 | 按时间分块（天/会话段）分组；活动明细走虚拟滚动列表，避免超长 DOM；甘特仅用于会话/成本聚合 |
| B6 活动详情 | `detail={tool,state,arguments(输入),output(输出,截断~2KB),exit?}`；落库做大小上限；FR-0001 已确认输出可得；若不可得则只显示输入 |
| B7 详情 UX | hover 摘要气泡 + 点击**行内展开**（arguments/output 只读 pre，大输出折叠）；bash 等命令类高亮 🔧/⌨ |

## FR-0001 结论（已查证）

**可拿到工具输出**：`@opencode-ai/sdk` 类型 `ToolStateCompleted = { status:"completed"; input; output: string; title; metadata; time:{start,end} }`。即 `part.updated` 在工具完成时 `part.state.output` 含完整输出；bash 命令本身在 `tool=bash` 的 `state.input.command`。采集应在**完成态一次性记录 input+output**（避免 pending/running/completed 多次 event 产生重复行）。

## Current Frontier

空（实现细节在正式设计文档定稿；无未决 DQ）。

## Remaining Branches

无。等待 owner 确认后：产出正式设计文档（`docs/37-…`）→ 实现。

## Fact Requests and Reports

- FR-0001：**已回报** —— `ToolStateCompleted.output` 可得工具输出（见上方结论）。

## Related Artifacts

- `crates/teamx/src/ui.rs`（dash_nodes / map / topo / timeline / activity 渲染）
- `crates/teamx/src/activity.rs` + `db.rs`（activity schema）
- `opencode-plugin/src/activity.ts` / `index.ts`（activity 采集）
- `docs/35-manual-member-cloud-node.cn.md`（admin-console 命名/部署）
