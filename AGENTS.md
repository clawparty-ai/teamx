# AGENTS.md

本文件为 AI 助手（agent）在此仓库中工作时必须遵守的项目级规则。

## 分支与 Git 规则

- **不要 push `enterprise` 分支到任何 remote**：`enterprise` 分支仅保留在本地，不得 push 到 `origin` 或其他任何 remote。当前工作分支为 `main`。
- 远程分支管理：GitHub 上的 `origin/enterprise` 已被删除；如再次出现，视为违规或误操作，应提醒并保持本地-only。
- **禁词约束**：所有文档、提交信息中不得出现该词（C 开头 + "l" + "a" + "s" + "h" 结尾的冲突代理工具名）。

## 构建与测试

- 单 crate workspace：`crates/teamx`（二进制 `teamx`）。
- 构建：`cargo build -p teamx`；单元测试：`cargo test -p teamx`（当前 73 个全绿）。
- CI（`.github/workflows/ci.yml`）在 push 到 `main`/`dev` 或 PR 时运行：
  - `cargo build`
  - `cargo clippy --all-targets -- -D warnings`（**注意：当前代码存在 21 个存量 clippy 错误**；修复代码后需确保 clippy 干净）
  - `cargo test`
  - shell 脚本 `tests/smoke.sh`、`tests/cli-test.sh`、`tests/concurrency.sh`
  - opencode-plugin：`bun install && bunx tsc --noEmit && bun run build`
- `.github/` 被 `.gitignore` 忽略，CI workflow 尚未实际生效（注释注明 "pending workflow-scope push"）。
- 完整套件：`bash tests/run-all.sh`（约 13 步；部分步骤需要 bun，network 步骤需要可用网络）。

## 架构要点

- **vendor 目录不可删**：`Cargo.toml` 通过 `[patch.crates-io] smoltcp = { path = "vendor/smoltcp" }` 本地 patch smoltcp，使其支持 `listen(0)` any-port 通配（tun0 透明代理依赖）。删除或改动 vendor 会导致构建失败或代理行为异常。
- 模块结构（`crates/teamx/src/`）：
  - `serve.rs` / `pki.rs` / `state.rs` / `db.rs` — mTLS server / 证书 / 团队状态机 / SQLite 事件台账。
  - `commands.rs` / `cli.rs` — CLI 入口与命令分发。
  - `tun_*.rs` / `tunnel_client.rs` / `tunnel.rs` / `socks5.rs` / `routes.rs` — tun0 透明代理、SOCKS5 代理、反向隧道、出口路由。
  - `teamfile.rs` / `events.rs` / `broadcast.rs` / `metrics.rs` — TEAM.md 引导、事件、广播、指标。
  - `gui.rs` / `gui_panel.rs` — 桌面托盘（feature `gui`，非默认）。
- **enterprise 模块不在 main 分支**：`capture.rs`、`analyze.rs`、`replay.rs`、`tls_parse.rs` 等抓包/分析/回放模块只在 `enterprise` 分支存在。main 分支上不要假设这些模块存在。
- opencode-plugin（TypeScript）使用 bun；`app/` 是 Swift macOS 客户端；`docs/` 含中英双语设计文档。
