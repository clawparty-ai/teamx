# teamx TEAM.md 团队初始化 — 测试方案（Test Plan）

> 关联设计：`docs/05-design-teamfile.md`；本计划针对 main 分支新增的 TEAM.md 驱动初始化功能。

## 1. 范围与目标

覆盖 `team create` 检测 `.teamx/TEAM.md` 并自动初始化项目的全部功能面：

1. **TEAM.md 解析器**：合法/非法/缺失章节文件的解析结果。
2. **创建集成**：检测到 TEAM.md 时自动完成 goal set、成员 letter 签发、AGENTS.md 生成、工作目录创建。
3. **成员 AGENTS.md 融合**：项目根 AGENTS.md + TEAM.md 成员描述正确合并。
4. **letter 双输出**：既写文件（`.teamx/members/[name]/invitation.letter`）也打印到 CLI。
5. **兼容性**：无 TEAM.md 时 `team create` 保持原行为；无效 TEAM.md 不阻塞（告警降级）。

## 2. 测试策略（分层）

| 层 | 手段 | 位置 | 运行 |
|---|---|---|---|
| 单元测试 | `teamfile.rs` 解析器（Rust `#[cfg(test)]`） | `crates/teamx/src/teamfile.rs` | `cargo test` |
| CLI 集成测试 | 真实二进制 + 临时 TEAM.md 目录脚本化断言 | `tests/teamfile-test.sh`（新增） | `./tests/run-all.sh` |
| 回归 | 现有全量套件（确保无 TEAM.md 时行为不变） | `tests/run-all.sh` | 同上 |

## 3. 测试环境

- macOS / Linux，Rust 工具链，bun（插件构建，若涉及插件侧）。
- 测试使用 `TEAMX_HOME` + `TEAMX_DB` 指向临时目录/库（`mktemp`），不触碰 `~/.teamx/teamx.db`。
- 在临时项目目录下构造 `.teamx/TEAM.md`（与测试项目根隔离）。

## 4. 测试数据隔离

- 每个用例独立临时项目目录（含 `.teamx/TEAM.md`）。
- 退出时 `trap` 清理临时目录（`TEAMX_HOME`、项目目录、`teamx.db*`）。

## 5. 入口 / 出口准则

- **入口**：功能变更后需通过 `./tests/run-all.sh`。
- **出口**：`cargo test` 全绿 + `teamfile-test.sh` 全绿 + 现有套件不回归；手工验收记录 TEAM.md 初始化演示。

## 6. 已知限制（非缺陷）

- TEAM.md 解析为宽松容错；不支持的章节被忽略（不报错）。
- letter 保存后不自动清理（保留审计）。
- 成员角色允许任意 role key；不强制内置角色。
