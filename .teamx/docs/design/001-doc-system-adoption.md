# 设计文档：团队文档体系落地（.teamx/docs + Console Documents）

- 状态：draft
- 所有者：tech-lead
- 创建者：caishu（owner）
- 创建日期：2026-09-17
- 关联：`docs/05-design-teamfile-docs.cn.md`、`crates/teamx/src/console_data.rs`（`docs_data`）、`crates/teamx/src/console_html.rs`（Documents 视图）

## 背景

`TEAM.md` 已声明 `## 文档` 契约（requirements / design / issue / pr / test-plan / release-note），
`teamx teamfile sync` 会据此生成 `.teamx/docs/_spec/*.json` 契约快照。

但 `.teamx/docs/` 下长期只有 `_spec/`，**没有任何文档实例**，因此 user-console 的
「Documents」面板只显示 6 个类型标签（计数全为 0）与「该类型暂无文档。」的空态。
本设计明确文档实例的落盘、状态流转与面板读取方式，并落地首批实例。

## 目标

- 文档实例成为**组织过程资产**：进 git、可追溯、可复用。
- user-console「Documents」面板能列出并查看实例正文。
- 状态流转遵循 TEAM.md 的声明式状态机（不进 `state.rs` 硬编码）。

## 方案

### 落盘布局

```
.teamx/docs/
├── _spec/<key>.json            # 契约快照（teamx teamfile sync 生成）
├── requirements/001-*.md
│               001-*.meta.json
├── design/001-*.md + .meta.json
├── issue/...
├── pr/...
├── test-plan/...
└── release-note/...
```

### `.meta.json` 字段

```json
{
  "doc": "requirements",
  "id": "001-enterprise-collab",
  "state": "draft",
  "owner": "owner",
  "created_at": "2026-09-17T15:13:14+00:00",
  "updated_at": "2026-09-17T15:13:14+00:00",
  "history": [{ "state": "draft", "by": "caishu", "at": "..." }]
}
```

`.meta.json` 只是便捷镜像；状态变更与审计链**以事件台账为准**（可从事件重建）。

### 面板读取

`console_data::docs_data(ctx)`：

1. 读 `.teamx/docs/_spec/*.json` → 文档类型列表（无则回退内置 `taskx`）；
2. 逐类型扫描 `.teamx/docs/<key>/*.meta.json` → 实例列表；标题取同名 `.md` 的首个 `# ` 标题；
3. 按 `(doc, updated_at)` 倒序返回 `{ types, docs }`，面板路由 `GET /api/panels/docs`。

### 状态流转与校验（声明式）

- 事件命名空间：`doc.created` / `doc.updated` / `doc.reviewed` / `doc.approved` / `doc.rejected` / `doc.reopened` / `doc.closed`。
- payload：`{ "doc": "...", "id": "...", "state": "...", "from": "...", "by": "...", "note": "..." }`。
- 校验：创建者角色 ∈ `创建者`（空 = 任意）；推进者 ∈ `审批者` 或 owner；`from -> to` 必须存在于声明状态流，否则拒绝且不写事件、不改 `.meta.json`。
- 变更响应 → 定向事件（`publish --assignee <角色>`）唤醒目标会话。

### 编号规则

每类型内顺序编号 `NNN-<slug>`（如 `001-enterprise-collab`）；创建前先查目录避免冲突。

## 接口与数据

| 项 | 位置 |
|----|------|
| 文档实例 | `.teamx/docs/<key>/<NNN-slug>.md` |
| 状态镜像 | `.teamx/docs/<key>/<NNN-slug>.meta.json` |
| 契约快照 | `.teamx/docs/_spec/<key>.json` |
| 面板 API | `GET /api/panels/docs` → `{ ok, types: [...], docs: [...] }` |
| 事件 | team ledger `doc.*` |

## 风险

| # | 风险 | 缓解 |
|---|------|------|
| R1 | `.meta.json` 与事件台账不一致 | 以台账为准，`.meta.json` 可重建 |
| R2 | 编号冲突 | 顺序编号 + 创建前查目录 |
| R3 | 实例无人维护、面板再次空转 | 纳入「设计优先/必须评审」流程，每迭代至少一份实例 |

## 测试要点

- 单元：`cargo test -p teamx console_data`（`docs_data_reads_specs_and_instances` 覆盖 `_spec` + 实例扫描）。
- 手工：Console「Documents」面板能列出本批实例（requirements/design/issue）并查看正文。

## 变更记录

| 日期 | 变更 | 作者 |
|------|------|------|
| 2026-09-17 | 初稿（draft）：明确落盘/状态/面板读取，并落地首批实例 | caishu |
