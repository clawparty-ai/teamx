# 测试方案：teamx local-team（TEAM.md → 单机多 agent）

> 对应设计 `docs/50-design-local-team.cn.md`。

## 1. 范围
- TEAM.md 解析 → 成员目录 + persona(AGENTS.md) + headless opencode + 可选 ttyd + 可选 git clone。
- 命令：`local-team up / ps / stop / logs`。

## 2. 用例清单

### 2.1 解析与 persona（纯逻辑，可单测）
| ID | 用例 | 期望 |
|---|---|---|
| L-01 | 真实 TEAM.md（含 owner + 3 agent）| 默认成员 = 非 owner 3 个；`--only owner` 含 owner |
| L-02 | 无 TEAM.md | 明确报错 |
| L-03 | 成员目录生成 | `<base>/local-team/<key>/` 建好 |
| L-04 | AGENTS.md 内容 | 含 display_name / role / description / skills / outputs 关键串 |
| L-05 | 工作规则摘录 | 若 TEAM.md 有规则，persona 含该段 |
| L-06 | 无成员 / 空 TEAM.md | 报错不 panic |

### 2.2 启动（集成/脚本；复用 agents registry）
| ID | 用例 | 期望 |
|---|---|---|
| L-07 | up 默认 | 每成员一个 headless serve（port 自动避让），registry 有记录 running |
| L-08 | ttyd | 每成员 ttyd 监听 `--ttyd-port-base`+i，`/` 可访问 |
| L-09 | --no-ttyd | 不启动 ttyd |
| L-10 | --only a | 只启 a |
| L-11 | ps | 列出成员 name/port/dir/ttyd_url/pid |
| L-12 | stop --name a | 停 a（serve+ttyd），registry 保留 stopped |
| L-13 | stop --all | 全停 |
| L-14 | 缺 opencode | 报错提示安装 |

### 2.3 git（可选，若身份/仓库可用）
| ID | 用例 | 期望 |
|---|---|---|
| L-15 | --repo+server 且可 clone | 成员目录出现 repo/ |
| L-16 | clone 失败（无身份/repo 不存在） | 警告继续，不阻塞 |

## 3. 测试策略
| 层 | 手段 |
|---|---|
| 解析/persona | Rust 单测（temp TEAM.md fixture）|
| 进程启动 | 脚本：临时 workspace + fixture TEAM.md → `up` → `ps` → curl headless/ttyd → `stop`；端口用高位避免冲突 |
| 回归 | `cargo test` + 既有 agents 测试 |

## 4. 入口/出口
- `cargo test -p teamx` 全绿。
- `tests/local-team-test.sh` 通过（up/ps/stop 生命周期）。
- 手动验证 ttyd URL 浏览器可达。
