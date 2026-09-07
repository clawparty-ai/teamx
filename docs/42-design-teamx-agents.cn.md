# teamx agents：headless opencode 多实例管理器设计

> 对应 5 项待办中的 T4。目标：不集成 ClawParty，由 teamx 自己提供「在一台机器上 spawn / 管理多个 headless opencode 成员实例」的能力。
> 状态：**设计（待 review → 实施）**；更新：2026-09-07
> 相关代码：`crates/teamx/src/worker.rs`（已有 headless run）、`docs/30-cloud-node-opencode-headless-tui.cn.md`、`docs/35-manual-member-cloud-node.cn.md`

## 1. 背景与动机

外部协作者（weiye741209-prog / clawparty-admin）提出「clawparty 批量铸造 Agent 作为 teamx 成员」的方案，其本质诉求是：**一台机器上快速跑起多个带团队身份的 headless opencode 成员，并能管理它们（起/停/看状态）**。clawparty 恰好提供该进程管理能力（`agents.rs` spawn 多个 `opencode serve`，各自独立 port/dir/OPENCODE_DB）。

为避免引入对 clawparty 的依赖，把这部分能力**原生做进 teamx**，让用户直接 `teamx agents spawn/list/stop`，无需外部编排器。它与现有 `teamx worker` 的区别：
- `worker`：常驻、**连一个已存在的 resident opencode serve**，只做 inbox 轮询 + headless turn。
- 新命令：**创建/销毁 opencode serve 进程本身**（进程管理器），是 worker/节点之上的"运行时层"。

## 2. 目标 / 非目标

**目标**
1. `teamx agents spawn`：在本机启动一个 headless `opencode serve`（`--hostname 127.0.0.1 --port <p>`），可指定独立 workspace 目录、opencode 二进制、额外环境变量；默认 daemon 化并记录 pid/port。
2. `teamx agents list`：列出本机已 spawn 的实例（name/port/dir/pid/running|stopped/启动时间/日志路径）。
3. `teamx agents stop`：停止指定实例（先 SIGTERM，超时 SIGKILL），可 `--remove` 注销。
4. 支持**多实例/单机多开**：实例名唯一，端口自动避让，每实例独立 `OPENCODE_DB` + cwd，互不串会话。

**非目标（本期不做）**
- 不做 agent 人格/技能文件（SOUL/AGENTS 等）生成——只 spawn 通用 headless opencode，人格由各 workspace 内容/插件决定。
- 不做"自动加入 teamx 团队"（letter 导入/席位认领仍是既有 `team import` 的职责；spawn 只负责把进程跑起来、可通过 `--env TEAMX_HOME=...` 让该实例以某个已导入 letter 的身份运行）。
- 不做跨机编排/资源限制/预算护栏（可后续演进）。
- 不做 `opencode run`（headless turn）编排——那是 `teamx worker` 的职责。

## 3. 术语

| 词 | 含义 |
|---|---|
| instance | 一个被 `teamx agents spawn` 创建的 `opencode serve` 进程，带唯一 name |
| registry | 本机实例登记表 `<teamx_home>/opencode/instances.json`，跨 CLI 调用可见 |
| workspace(dir) | 该实例的工作目录 = opencode 的 cwd，也是它 `.teamx`/身份所在处 |

## 4. CLI 设计（已确认：命令名 `teamx agents`）

```
teamx agents spawn --name <NAME> [--dir <DIR>] [--port <PORT>] [--opencode <BIN>]
                     [--env K=V ...] [--foreground]
teamx agents list [--running]
teamx agents stop  --name <NAME> [--remove]
```

| 参数 | 说明 | 默认 |
|---|---|---|
| `--name` | 实例名，本机唯一 | 必填 |
| `--dir` | 该实例工作目录（不存在则创建） | `./<name>` |
| `--port` | `opencode serve --port` | 自动选 4200+ 空闲端口 |
| `--opencode` | opencode 二进制路径 | `opencode`（PATH） |
| `--env K=V` | 追加环境变量（可重复），如 `TEAMX_HOME=...` / `TEAMX_SERVER_URL=...` | — |
| `--foreground` | 前台运行（继承 stdio，Ctrl-C 退出） | daemon 后台 |
| `list --running` | 只看 running 实例 | 全列 |
| `stop --remove` | 停止并注销 | 仅停止保留登记 |

**spawn 行为（daemon 默认）**
```
opencode serve --hostname 127.0.0.1 --port <p>    (cwd=<dir>, OPENCODE_DB=<dir>/opencode.db, +env)
stdout/stderr → <registry>/<name>.log
registry.instances[name] = { name, dir, port, opencode, pid, started_at, log }
```

**stop 行为**：查 registry → 若 pid 存活 SIGTERM → 等 ≤2s → 仍活则 SIGKILL → 返回 stopped。`--remove` 从 registry 删除。

## 5. 状态存储与进程发现

- 登记表 JSON：`<teamx_home>/opencode/instances.json`（`db::teamx_home()`，多 CLI 调用共享）。
- 存活判定：unix 用 `kill(pid,0)`；停止用 SIGTERM→SIGKILL。
- 端口避让：`TcpListener::bind(127.0.0.1:port)` 探活，从 preferred（默认 4200）往上找空闲。
- 崩溃自愈：本期不做内置守护循环（systemd/外部 supervisor 负责 Restart）；`list` 会显示 stopped 供人工处理。

## 6. 与团队身份 / 节点的关系

- 一个多开场景示例：
  ```bash
  teamx team import "<writer letter>" --name writer --session w   # 已在 work-writer 导入 letter
  teamx agents spawn --name writer --dir work-writer --port 4301 \
        --env TEAMX_HOME=$PWD/work-writer/.teamx --env TEAMX_SERVER_URL=https://server
  # 该 writer opencode 实例就带 teamx 成员身份；配 worker（teamx worker --opencode-url ...4301）
  # 或直接由外部/人工 attach 使用。
  ```
- 与既有部署的衔接：文档 30/35 中的 systemd 单元（opencode-serve-<user>）仍是"每节点常驻"方式；`teamx agents` 是**更轻量、可脚本化**的替代/补充（尤其单机多 agent、PoC、测试）。

## 7. 参考实现来源

ClawParty `agents.rs` spawn 的等价逻辑（作为行为参照，非代码搬运）：
```rust
Command::new(opencode_bin)
  .args(["serve","--port",port])
  .current_dir(agent_dir)
  .env("OPENCODE_DB", format!("{agent_dir}/opencode.db"))
```
新命令在其之上增加：统一 registry、跨调用 list/stop、env 注入、自动端口、日志落盘。

## 8. 验证方法

1. `cargo build -p teamx` + `cargo test -p teamx` 全绿；`teamx agents --help` 正常。
2. 单机双实例：
   ```bash
   teamx agents spawn --name a --dir /tmp/oc-a
   teamx agents spawn --name b --dir /tmp/oc-b          # 端口应自动错开
   teamx agents list                                     # 两条 running
   curl http://127.0.0.1:<a.port>/api/session              # a 可达
   curl http://127.0.0.1:<b.port>/api/session              # b 可达
   teamx agents stop --name a --remove
   teamx agents list --running                           # 只剩 b
   ```
3. 端口冲突：先手动占住某端口再 spawn 应自动避让；同名 spawn 应报已存在。
4. env 生效：spawn 带 `--env TEST_VAR=1`，`/api/session` 或进程 environ 可见。

## 9. 相关文档
- `docs/30-cloud-node-opencode-headless-tui.cn.md`（headless opencode 节点架构）
- `docs/35-manual-member-cloud-node.cn.md`（member 云节点部署）
- `crates/teamx/src/worker.rs`（headless run / opencode-url）
