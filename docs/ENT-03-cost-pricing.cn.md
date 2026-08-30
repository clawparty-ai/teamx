# ENT-03 — 成本定价表 + token→cost 兜底/分解（方案 C，设计）

> 状态：**实施中**（enterprise 分支）
> 分支约束：本文档为 **enterprise 专属**（`ENT-` 前缀），**不得合并到 main 分支**。
> 目标读者：实现者
> 关联：`docs/ENT-02-activity-analytics.cn.md`（活动采集/看板总设计）、`opencode-plugin/src/index.ts`（event hook）、`crates/teamx/src/activity.rs`（聚合）、`crates/teamx/src/serve.rs`（activity.push）、`crates/teamx/src/db.rs`（schema）
> 日期：2026-08-30

---

## 0. 背景与目标

当前成本数据只有一条来源：opencode 的 `step-finish` 事件 `part.cost`（USD）。存在三个问题：

1. **粒度太粗**：cost 只挂在 `step_finish` 一种 kind 上，`work_session` / `tool_call` / `file_edit` 等都没有成本。
2. **无兜底**：当 `part.cost` 缺失或为 0 时，teamx 无法自己从 token 推算成本。
3. **无分解**：只有总成本，没有 input / output / cache 分项。

本方案引入 **teamx 自有定价表**，在不破坏 opencode 权威路径的前提下：

- **兜底**：`tokens × 单价` 自己算成本（当 opencode 未给 cost 时）。
- **分解**：一条 step 的总成本拆成 `input / output / cache_read / cache_write` 四部分。
- **模型归属**：activity 行记录 `model_id` / `provider_id`，支撑按模型/提供商统计。

---

## 1. 核心决策

| 决策点 | 结论 |
|---|---|
| 定价表存放位置 | serve DB 新增 `model_pricing` 表（团队/owner 单一事实源） |
| 定价来源 | 三层：`owner`（手动覆盖） > `opencode`（插件运行时上报） > `builtin`（编译进二进制的内置标价） |
| 计算位置 | **serve 侧 `activity.push` 时计算**（plugin 只上报客观事实） |
| opencode cost 优先级 | opencode 给的 `cost > 0` 时**以 opencode 为准**（`cost_source='opencode'`），定价表仅用于分解展示 |
| reasoning token | 暂视为已含在 output 内，不单独计价（见 §4.4，待对拍确认） |
| 历史数据 | 不回填（存量行无 model_id，无法回溯重算） |

---

## 2. 数据模型

### 2.1 新增 `model_pricing` 表

```sql
CREATE TABLE IF NOT EXISTS model_pricing (
  provider_id          TEXT NOT NULL,
  model_id             TEXT NOT NULL,
  price_input          REAL NOT NULL,               -- USD / 1M tokens
  price_output         REAL NOT NULL,
  price_cache_read     REAL NOT NULL DEFAULT 0,
  price_cache_write    REAL NOT NULL DEFAULT 0,
  over200k_input       REAL,                        -- >200k 上下文分层（可空=不启用）
  over200k_output      REAL,
  over200k_cache_read  REAL,
  over200k_cache_write REAL,
  source               TEXT NOT NULL DEFAULT 'builtin',  -- builtin | opencode | owner
  updated_at           TEXT NOT NULL,
  PRIMARY KEY (provider_id, model_id)
);
```

### 2.2 `activity` 表新增列（v10 迁移）

```sql
ALTER TABLE activity ADD COLUMN model_id          TEXT;
ALTER TABLE activity ADD COLUMN provider_id       TEXT;
ALTER TABLE activity ADD COLUMN tokens_cache_read  INTEGER;
ALTER TABLE activity ADD COLUMN tokens_cache_write INTEGER;
ALTER TABLE activity ADD COLUMN cost_source       TEXT NOT NULL DEFAULT 'opencode';
ALTER TABLE activity ADD COLUMN cost_input        REAL;
ALTER TABLE activity ADD COLUMN cost_output       REAL;
ALTER TABLE activity ADD COLUMN cost_cache_read   REAL;
ALTER TABLE activity ADD COLUMN cost_cache_write  REAL;
```

字段语义：
- `cost`（已有列）：最终计入聚合的成本，权威值。
- `cost_source`：`opencode` / `computed` / `none`。
- `cost_input/output/cache_read/cache_write`：分解项（可空）。
- `model_id` / `provider_id`：模型归属（定价表 key）。
- `tokens_cache_read/write`：cache token 从 detail 上移到独立列，供成本计算。

---

## 3. 定价数据来源

| 层 | source | 获取方式 | 优先级 |
|---|---|---|---|
| 内置 | `builtin` | 编译进二进制的 `include_str!` JSON（主流模型公开标价） | 最低 |
| opencode 运行时 | `opencode` | 插件启动时从 `client` 读模型列表的 `Model.cost`，经 `model.pricing_sync` RPC 上报 | 中 |
| owner 手动 | `owner` | `teamx model price set` 命令 | 最高 |

读时按 `(provider_id, model_id)` 唯一主键 upsert（后写覆盖前写），自然取到最高优先级的一行。

opencode `Model.cost` 结构（`@opencode-ai/sdk`，每百万 token USD）：

```ts
cost: {
  input: number;
  output: number;
  cache: { read: number; write: number };
  experimentalOver200K?: { input; output; cache: { read; write } };  // >200k 分层
}
```

---

## 4. 成本计算算法

### 4.1 输入 token 语义

opencode 的 `StepFinishPart.tokens.input` **已包含 cache read**（插件注释明确）。因此：

```
非缓存输入 = max(tokens.input - tokens.cache.read, 0)
```

### 4.2 分层判定（>200k context）

`tokens.input > 200_000` 且定价表存在 `over200k_*` 时采用分层价，否则用基础价。

### 4.3 公式

```
use_over = tokens.input > 200_000 且 over200k_* 非空
p = use_over ? over200k 价目 : 基础价目

cost_input       = (tokens.input - tokens.cache.read) * p.input / 1_000_000
cost_cache_read  = tokens.cache.read                    * p.cache_read / 1_000_000
cost_cache_write = tokens.cache.write                   * p.cache_write / 1_000_000
cost_output      = tokens.output                        * p.output / 1_000_000

total = cost_input + cost_cache_read + cost_cache_write + cost_output
```

### 4.4 reasoning token（待确认项）

`Model.cost` 无单独 reasoning 价目。**暂按 reasoning 已含在 output 内**处理（不单独计价）。落地后用单测对拍 opencode 的 `part.cost` 确认口径，若不一致再启用 `cost_output = (output + reasoning) * p.output`。

---

## 5. 计算位置与流程

### 5.1 serve `activity.push`（网络模式）

```
activity.push 收到 rows
  └─ 现有归属/校验（member_id / team_id / node_id）
  └─ apply_pricing(conn, &mut rows)   ← 新：读 model_pricing，对缺 cost 的 step_finish 行计算
  └─ push_activities(conn, &rows)
```

`apply_pricing` 对每行：
- `kind != "step_finish"` → 跳过。
- opencode 给了 `cost > 0` → `cost_source='opencode'`；若可解析定价，仍算分解项（仅供展示）。
- opencode 无 cost/为 0 → 有定价则计算并填 `cost` + 分解 + `cost_source='computed'`；无定价则 `cost_source='none'`。

### 5.2 V1 本地 `activity push`（单机）

`commands.rs` 的 `ActivityCmd::Push` 同样在 `push_activities` 前调 `apply_pricing`（本地 DB 也有 `model_pricing` 表，内置种子可用）。

---

## 6. 新增 RPC / CLI

### 6.1 RPC：`model.pricing_sync`（plugin 上报，成员可调用）

```
model.pricing_sync { rows: [{ provider_id, model_id, price_input, price_output,
                              price_cache_read, price_cache_write, over200k_* }] }
  → upsert 到 model_pricing（source='opencode'）
```

> plugin 经 `runCli(["model", "pricing-sync", "--data", "<json>"])` 调用；服务端同时接受 `rows`（数组）与 `data`（JSON 字符串）两种入参。

### 6.2 CLI：`teamx model`（owner 覆盖）

```
teamx model set <provider>/<model> --input X --output Y [--cache-read] [--cache-write] [--over200k-*]
teamx model ls
```

---

## 7. 数据流全链路改动点

| 层 | 文件 | 改动 |
|---|---|---|
| plugin | `opencode-plugin/src/index.ts` | hook `message.updated` 建 `sessionModel`；`step_finish` 落行带 `model_id`/`provider_id`/cache 列；启动时上报模型定价 |
| plugin | `opencode-plugin/src/activity.ts` | `ActivityRow` 增 `model_id`/`provider_id`/`tokens_cache_read`/`tokens_cache_write` |
| plugin | `opencode-plugin/src/client.ts` | RPC 方法映射加 `model.pricing_sync` |
| serve | `crates/teamx/src/serve.rs` | `activity.push` 内调 `apply_pricing`；新增 `model.pricing_sync` RPC |
| 数据 | `crates/teamx/src/db.rs` | v10 迁移：`model_pricing` 表 + `activity` 9 列 + 内置定价种子 |
| 数据 | `crates/teamx/src/activity.rs` | `ActivityRow` 加字段；`ModelPrice`/`compute_cost`/`apply_pricing`/`load_prices`；聚合输出分解；`summary` 扁平字段 |
| CLI | `crates/teamx/src/cli.rs` / `commands.rs` | `model price set/ls`；V1 push 调 `apply_pricing` |
| 展示 | `crates/teamx/src/ui.rs` | 修 overview 扁平字段 bug；Cost 视图分解展示 |

---

## 8. DB 迁移（v10）

沿用 `user_version` 模式（当前 9）：

```rust
if version < 10 {
    // 1) 建 model_pricing 表（SCHEMA 已含，幂等）
    // 2) activity 加列（PRAGMA table_info 判重，幂等 ALTER）
    // 3) 内置定价种子 INSERT OR IGNORE
}
conn.pragma_update(None, "user_version", 10)?;
```

---

## 9. 边界情况

| 边界 | 处理 |
|---|---|
| opencode 给 cost 且定价表也有 | opencode 为准（`cost_source='opencode'`），定价仅分解 |
| tokens 为空但 cost 有值 | 保留 cost，分解为空 |
| model_id 未知（无定价） | `cost_source='none'`，不编造 |
| `input` 含 cache.read | `input - cache.read` 算非缓存输入（§4.1，需对拍确认） |
| >200k 分层价缺 `over200k_*` | 回退基础价 |
| 历史数据 | 不回填，仅新数据生效 |

---

## 10. 实施里程碑

| 阶段 | 内容 | 验收 |
|---|---|---|
| **P0** | `model_pricing` 表 + activity 9 列 + v10 迁移 + 内置种子 | `cargo test -p teamx` 全绿；迁移幂等 |
| **P1** | `compute_cost` / `apply_pricing` / `load_prices` + `push_activities` 集成 + 单测 | 单测覆盖各分支；对拍口径 |
| **P2** | 插件模型归属 + 定价上报；`model.pricing_sync` RPC | 实测 step 落库带 model、定价表有 opencode 行 |
| **P3** | `teamx model price set/ls` | owner 可覆盖/查看定价 |
| **P4** | 聚合输出分解；修 overview 扁平字段 bug；Cost 视图分解展示 | UI 成本卡片数字正确、明细可看分解 |
