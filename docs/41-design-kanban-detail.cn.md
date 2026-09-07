# admin-console 看板（KanBan）改进设计：卡片详情 / 排序 / 列宽

> admin-console = 原 `teamx ui`。对应 5 项待办中的 T1–T3。
> 状态：**设计（待 review → 实施）**；更新：2026-09-07
> 相关代码：`crates/teamx/src/ui.rs`（单文件全栈，页面/API/JS 均在此）、`crates/teamx/src/activity.rs`

## 1. 目标

解决 admin-console KanBan 视图的三个体验问题：

- **T1 卡片缺详细信息**：现在每张卡只展示 `work_session` 的粗粒度摘要（task/时长/cost），看不到该成员具体做了什么（调用了什么工具、编辑了哪些文件、完成哪些步骤、有无人工介入）——这些在 **Timeline / 鱼骨图** 里是有的。
- **T2 列排序固定**：列按 `display_name` 字母序，不反映"谁最近在活跃"；闲置用户夹在中间。
- **T3 列宽/溢出**：KanBan 视图容器全宽（`full-width`），列被拉得过宽或内容撑破网格造成横向溢出。

## 2. 现状（代码事实）

| 项 | 现状 | 位置 |
|---|---|---|
| 看板每列=一个人（user），同人多 seat 合并一列 | — | `api_kanban`（ui.rs 后端） |
| 每列 tasks 只取 `kind='work_session'` | 细粒度活动被排除 | ui.rs api_kanban 内 `rows(...Some("work_session")...)` |
| 前端 `renderKanban`/`renderTaskCard` | 只展示 work_session 粗卡，tooltip 全量 dump detail | ui.rs ~1268-1359 |
| 列排序 | `ORDER BY uname ASC, joined_at ASC` | ui.rs SQL |
| 列宽 | `.kanban{grid-template-columns:repeat(4,1fr)}` + 容器 full-width | ui.rs CSS 391-393 |
| 鱼骨/时间轴已有能力 | `fishDetailText`（按 kind 提炼单行）、`fishKindLabel/Color`、`activityDetailHtml`（点击展开命令/输出/文件 JSON）、`hoverSummary` | ui.rs ~1556-1719 |

**根因总结**：① 后端看板只取 work_session → 前端无细粒度可展示；② 前端卡片不是 kind-aware（硬编码 work_session 徽标、detail 全量 dump 而非按 kind 提炼）；③ 排序纯字母序；④ 容器全宽 + grid 列无 `min-width:0`。

## 3. 设计决策

### D1（T1）：看板列 = 任务卡 + 明细活动卡

后端每列返回两组数据：

| 字段 | 内容 |
|---|---|
| `tasks` | `work_session` 行（"任务段"粗卡，保持现状） |
| `events` | 该列每个 seat 最近 `limit*2` 条**非 work_session** activity（tool_call / file_edit / step_finish / command / human_*），去掉 work_session 后按时间倒序、截断 |

前端每列布局：先渲染 `tasks`（任务卡，保持现有交互），其下渲染一个"明细活动"区（`events`），每张明细卡复用鱼骨能力：

- 卡片头：kind 彩色徽标（`fishKindLabel`+`fishKindColor`）+ 成员 + 节点 + 时间；
- 卡片体：`fishDetailText` 提炼的单行（"调用工具 X · file" / "编辑 file" / "完成步骤 step" / 人工输入文本）；
- hover：`hoverSummary` 紧凑摘要（命令/输出前 400 字）；
- 点击：展开 `activityDetailHtml`（命令、输出、文件、完整 JSON），与鱼骨一致；
- meta：duration、`● human`、per-activity cost/token（`fmtItemCost`）。

实现方式：
- 后端 `api_kanban`：保留 work_session 查询；**新增一次 `activity::rows(... kind=None ...)`** 取该 seat 全 kind，过滤掉 work_session 进 `events`。
- 前端新增 `renderEventCard(e)` + 容器级事件委托 `bindKanbanEvents()`（click 展开 / mouseover hover tooltip），一次性绑定到 `#kanbanCols` 避免每列重复监听。

### D2（T2）：列按最近活跃排序，无活跃沉底

- 每列计算 `last_active = max(tasks ∪ events 的 started_at)`（空列=""）。
- 后端在构造完所有列后排序：
  1. 有活动的列（`last_active != ""`）在前，按 `last_active` **降序**（最新活跃最前）；
  2. 无活动列沉底，按其 `display_name` 字母序稳定排列。
- 排序在后端做（`out.sort_by`），前端无需改逻辑。

### D3（T3）：最多 4 列且不横向溢出

- grid 列模板改为 `repeat(4, minmax(0, 1fr))`（`minmax(0,1fr)` 让内容不把列撑破），media query 对应改为 `minmax(0,1fr)`。
- KanBan 视图容器保留 full-width（大屏铺开）但给内部 `.kanban` 设 `max-width:1560px; margin:0 auto`，避免列被拉得超宽。
- 列/卡片防溢出：`.kb-col{min-width:0; overflow:hidden}`；`.kb-task/.kb-who` 保持 ellipsis；新增明细卡文字 `word-break/overflow-wrap:anywhere`；展开区 `.tl-pre` 限 `max-width:100%; overflow-x:auto`。
- 第 5+ 个用户：仍以"换行成第二排 4 列"呈现（不做横向滚动队列）——保持现有心智，最多一行 4 列 = 视觉无横向溢出。

## 4. 数据流（改动点一览）

```
前端 renderKanban()
  ├─ GET /api/kanban?team&from&to
  │    └─ api_kanban（Rust）
  │         ├─ 成员列表（按 user 分组）              [不变]
  │         ├─ tasks = rows(kind=work_session)      [不变]
  │         ├─ events = rows(kind=None) − work_session  [新增]
  │         ├─ last_active = max(tasks+events.started_at) [新增]
  │         └─ out.sort_by(有活动→last_active 降序; 无活动→display_name)  [新增]
  ├─ 每列：列头(不变) + 摘要(加 events count/cost)
  │        + tasks.slice(0,12)（renderTaskCard 不变）
  │        + events.slice(0,8)（renderEventCard 新增, 鱼骨同款）
  └─ bindKanbanEvents() 委托: click展开 / hover tooltip   [新增]
CSS: grid minmax(0,1fr)、.kanban max-width、.kb-col min-width:0、
     .kb-ev-* 明细卡样式、.tl-pre 溢出保护                        [新增]
```

## 5. 约束与兼容

- admin-console owner/team 隔离不变：任何新查询继续以 `team=owned_team` + 成员集合为范围（后端按成员逐 seat 查 activity，天然不跨队）。
- `detail` 在 `activity::rows` 是**原始 JSON 字符串**；前端渲染统一走 `try{JSON.parse}`（现有鱼骨同款）。
- 明细卡数据量控制：events 每列上限 `limit*2`（limit 默认 30→events≤60），前端只渲染前 8 条并折叠其余，避免列高爆炸。
- UI 是内嵌 raw string，改动后需 `cargo build` + JS 语法检查；不新增 clippy 告警。

## 6. 验证方法

1. `cargo build -p teamx` + `cargo test -p teamx` 全绿。
2. 起 `teamx ui`，进 KanBan：
   - 某列应同时出现任务卡 + "detail activity" 明细卡；明细卡 hover 有摘要、点击展开命令/输出/文件；
   - 一个 team 里正在活跃的用户列排最前；多日无活动的用户列沉底且按名字排序；
   - 超宽屏下列数仍最多一行 4 个、无横向滚动条；列内长命令自动换行/截断不撑破。
3. 回归：Timeline / Cost / Dashboard 不受影响。

## 7. 相关文档
- `docs/37-admin-console-improvements.cn.md`（admin-console 既有设计）
- `crates/teamx/src/ui.rs`、`crates/teamx/src/activity.rs`
