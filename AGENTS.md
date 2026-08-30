# AGENTS.md

本文件为 AI 助手（agent）在此仓库中工作时必须遵守的项目级规则。

## 分支与 Git 规则

- **不要 push `enterprise` 分支到任何 remote**：`enterprise` 分支仅保留在本地，不得 push 到 `origin` 或其他任何 remote。
- 远程分支管理：GitHub 上的 `origin/enterprise` 已被删除；如再次出现，视为违规或误操作，应提醒并保持本地-only。
- 其他分支（如 `main`）不受此限制。
- **禁词约束**：所有文档、提交信息中不得出现该词（C 开头 + "l" + "a" + "s" + "h" 结尾的冲突代理工具名）。

## 构建与测试

- 单 crate workspace：`crates/teamx`（二进制 `teamx`）。
- 构建：`cargo build -p teamx`；单元测试：`cargo test -p teamx`（当前 117 个全绿）。
- CI（`.github/workflows/ci.yml`）在 push 到 `main`/`dev` 或 PR 时运行：
  - `cargo build`
  - `cargo clippy --all-targets -- -D warnings`（**注意：当前代码存在 31 个存量 clippy 错误**，多数集中在 `tun_stack.rs`/`analyze.rs`/`capture.rs`；修复代码后需确保 clippy 干净）
  - `cargo test`
  - shell 脚本 `tests/smoke.sh`、`tests/cli-test.sh`、`tests/concurrency.sh`
  - opencode-plugin：`bun install && bunx tsc --noEmit && bun run build`
- `.github/` 被 `.gitignore` 忽略，CI workflow 尚未实际生效（注释注明 "pending workflow-scope push"）。

## Enterprise capture/analysis/replay 测试

- 三个验收脚本按顺序依赖，均使用 `TEAMX_CAPTURE_DB`（默认 `/tmp/teamx-capture-test.db`），需要系统装有 `sqlite3`：
  1. `bash tests/capture-test.sh` — 抓包 P1；网络断言以 `TEAMX_SERVER_URL`（默认 `https://81.70.41.108:8888`）连通性为 gate，离线时 SKIP。
  2. `bash tests/analysis-test.sh` — 分析 P2；需要先跑 capture-test 产生 TLS flow。
  3. `bash tests/replay-test.sh` — 回放 P3；T-C1 需要 TLS flow（先跑 capture-test）。
- 跑 capture-test 前建议删除旧 DB：`rm -f "$TEAMX_CAPTURE_DB"`，否则 SNI 断言可能因残留数据不稳定。
- tun0 相关测试需要 root：`CAPTURE_WITH_TUN0=1 bash tests/capture-test.sh`。
- 完整套件：`bash tests/run-all.sh`（14 步；部分步骤需要 bun）。

## 架构要点

- **vendor 目录不可删**：`Cargo.toml` 通过 `[patch.crates-io] smoltcp = { path = "vendor/smoltcp" }` 本地 patch smoltcp，使其支持 `listen(0)` any-port 通配（tun0 透明代理依赖）。删除或改动 vendor 会导致构建失败或代理行为异常。
- 模块结构（`crates/teamx/src/`）：
  - `capture.rs` / `capture_cli.rs` — 抓包存储（SQLite `flows`/`stream_chunks`/`tls_keys` 等）与 CLI。
  - `analyze.rs` — 流量分析（stats/list/http 重建/TLS 1.3 解密）。
  - `replay.rs` / `replay_cli.rs` — 回放（mock serve / tcp replay / HAR / curl）。
  - `tls_parse.rs` — TLS 协议指纹/ClientHello/记录切分（纯函数）。
  - `tun_*.rs` / `tunnel_client.rs` / `socks5.rs` — tun0 透明代理与 SOCKS5 出口桥接。
  - `serve.rs` / `pki.rs` / `state.rs` — mTLS server / 证书 / 团队状态机。
- 网络抓包的数据路径：数据面 tap（`CaptureTap`）→ 有界 channel → 后台 writer 批量写入 capture DB；转发路径不阻塞。
- opencode-plugin（TypeScript）使用 bun；`app/` 是 Swift macOS 客户端；`docs/` 含中英双语设计文档，enterprise 专属设计以 `ENT-` 前缀见 `ENT-04-traffic-capture.cn.md`、`ENT-05-traffic-analysis.cn.md`、`ENT-06-traffic-replay.cn.md`。
