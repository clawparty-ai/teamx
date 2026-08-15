# teamx

团队协作状态内核 + opencode 插件。记录 team / member / goal 的持久化状态与行为历史（状态机，参考 loopx），并通过 opencode 内的 `/Team` agent 让用户创建/加入团队、选择角色、汇报进展、实时同步，直到达成团队目标。

V1：单机本地（CLI-only，无 server），全局 SQLite `~/.teamx/teamx.db`。

## 快速开始

```bash
./install.sh        # 构建 Rust CLI 并安装插件三件套到 opencode 配置
# 重启 opencode 后：
#   /Team → 进入团队协作 agent
#   /Team 创建团队 "名字"      → 你是 owner，得到 invite_token
#   /Team 加入 <token> --name 我 → 成为 member（pending），等待 owner 审批
```

## 结构

```
crates/teamx/           Rust CLI（SQLite 事件账本 + 状态机 + loopx 桥接）
packages/opencode-plugin/  opencode 插件（17 个 teamx_* 工具 + /Team agent + command）
install.sh              一键安装 / --uninstall 卸载
tests/                  run-all.sh、smoke.sh、cli-test.sh、concurrency.sh
docs/                   v1-spec、loopx-bridge、test-plan、test-cases、v2-design、demo、demo-3p、manual-test
.github/workflows/      CI（cargo test+clippy、CLI 测试、插件 typecheck+build）
```

## 测试

测试方案与案例见 `docs/test-plan.md`、`docs/test-cases.md`。一键运行全部测试：

```bash
./tests/run-all.sh    # cargo test + smoke.sh + cli-test.sh + concurrency.sh + 插件 typecheck/build
```

手工验收：二人闭环见 `docs/manual-test.md`、`docs/demo.md`；三人闭环（owner+contributor+reviewer）见 `docs/demo-3p.md`。

## 安全定位（重要）

V1 **无真实鉴权**：`session_key` 调用方自报、`invite_token` 对全队成员可见，是"信任本机"的协作约定——"owner 审批/角色"是协作语义，不是安全边界。真实鉴权在 V2（见 `docs/v2-design.md`）。详见 `goal-v1.md`「信任模型」。

## 手动验证（双会话闭环）

```bash
cargo build
export TEAMX_DB=/tmp/t.db   # 用独立库，不污染全局
teamx init
teamx team create "Demo" --session inst:alice --json
teamx team join <token> --name Bob --session inst:bob --json
teamx team approve <member_id> --session inst:alice --json
teamx role set contributor --session inst:bob --json
teamx goal set "Ship MVP" --session inst:alice --json
teamx goal share --session inst:alice --json
teamx publish progress --data '{"message":"done auth"}' --session inst:bob --json
teamx publish achieved --data '{}' --session inst:bob --json
teamx goal close --session inst:alice --json
teamx sync --session inst:bob --json
```

## 环境变量

- `TEAMX_DB`：数据库路径（默认 `~/.teamx/teamx.db`）
- `TEAMX_BIN`：插件调用的 teamx 可执行名（默认 `teamx`）
- `TEAMX_HOME`：实例 UUID 存放目录（默认 `~/.teamx`）
