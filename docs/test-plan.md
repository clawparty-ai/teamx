# teamx V1 测试方案（Test Plan）

## 1. 范围与目标

覆盖 V1（单机本地 CLI-only）的全部功能面，验证：

1. 事件账本正确性：append-only、per-team seq 单调且并发安全。
2. 状态机正确性：Team/Member/Goal 的合法/非法转换均按 `src/state.rs` 转换表执行。
3. 命令面完整性与授权边界：owner 专属操作、成员自助操作、非成员拒绝。
4. 边界与负面行为：重复加入、坏 token、未知角色、非法 publish 类型、越权 ask/respond 等。
5. 同步协议：`sync` 游标推进/不推进、多团队会话消歧。
6. loopx 桥接：不可用时明确提示、可用时正确抽取进度快照。
7. 插件三件套：agent/命令/工具能被 opencode 正确注册加载。

## 2. 测试策略（分层）

| 层 | 手段 | 位置 | 运行 |
|---|---|---|---|
| 单元测试 | Rust `#[cfg(test)]`：状态机转换表、事件账本 seq/游标/payload | `crates/teamx/src/state.rs`、`events.rs` | `cargo test` |
| CLI 集成测试 | 真实二进制 + 独立临时 SQLite 库的脚本化断言 | `tests/smoke.sh`、`tests/cli-test.sh` | `./tests/run-all.sh` |
| 三人协作测试 | owner+contributor+reviewer 闭环（等价 demo-3p） | `tests/three-member.sh` | 同上 |
| 并发测试 | 5 会话 × 3 并行 publish，验证 seq 严格递增（TC-301） | `tests/concurrency.sh` | 同上 |
| 插件校验 | `bunx tsc --noEmit` + `bun run build` + opencode serve 注册探测 | `opencode-plugin` | `./tests/run-all.sh` |
| 模型级验收 | 真实模型经插件调用 `teamx_*`（headless `opencode run --agent teamx`） | `tests/acceptance.sh`（消耗 token，不并入默认套件） | 手动/可选 |
| 手工 E2E（验收） | 三个真实 opencode 窗口走 `/Team` 全流程 | 人工 | 见 `docs/demo-3p.md`、`docs/test-cases.md` TM-04 |

## 3. 测试环境

- macOS / Linux，Rust 工具链（cargo 1.94+），bun（插件构建）。
- 测试一律使用 `TEAMX_DB` 指向的临时数据库（`mktemp`），**不触碰** `~/.teamx/teamx.db` 生产库。
- 无需网络、无需 provider key（不依赖模型）。

## 4. 测试数据隔离

- 每个测试脚本独立 `mktemp` DB，退出时 `trap` 清理 `*.db / *.db-wal / *.db-shm`。
- 会话标识使用合成 key（如 `s:owner`、`inst:m1`），不依赖真实 opencode session。

## 5. 入口 / 出口准则

- **入口**：功能变更或文档涉及的代码改动后，需通过 `./tests/run-all.sh`。
- **出口**：`cargo test` 全绿 + 两个 CLI 脚本全绿 + 插件 typecheck/build 通过；手工验收用例 TM-01 完成记录。

## 6. 已知限制（非缺陷）

- 并发写依赖 SQLite WAL 单写者 + `busy_timeout`，未做分布式一致性（V2 引入 server 后另行设计）。
- loopx 桥接为按需读取，不做心跳/文件监听。
- 插件 `event` hook 目前只镜像 `session.idle`，`message.updated` 活动留待 M2。

## 7. 回归清单（每次发布前）

```bash
cargo build && cargo test
./tests/smoke.sh
./tests/cli-test.sh
./tests/three-member.sh
./tests/concurrency.sh
(cd opencode-plugin && bunx tsc --noEmit && bun run build)
# 手工：三个 opencode 窗口验证 TM-04（docs/demo-3p.md）
```
