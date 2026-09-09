# 测试方案：local-team server 自举 + 成员 auto-approve

> 对应设计 `docs/52-design-local-team-server.cn.md`；叠加 `docs/51` 的进程测试。

## 1. 范围
- `local-team up` 无 serve → 自举本 ws serve（端口自动找空闲）。
- 有 serve（TEAMX_SERVER_URL 可达）→ 加入，不起新 serve。
- team create + TEAM.md bootstrap → letter 落盘；每非 owner 成员 import + approve → active。
- agent 工作区持 letter，headless 连 serve。

## 2. 用例清单

### 2.1 serve 自举/复用
| ID | 用例 | 期望 |
|---|---|---|
| S-01 | 无 TEAMX_SERVER_URL、无运行 serve | up 启动本 ws serve；server.pid 存在；`/health` ok |
| S-02 | 端口 5781 被占 | 自动找空闲端口；记录到 server.url |
| S-03 | TEAMX_SERVER_URL 指向运行中 serve | 复用（不新起），server.pid 不存在 |
| S-04 | stop 后 | 本 ws serve 被停；外部 serve 不停 |

### 2.2 建队与成员激活
| ID | 用例 | 期望 |
|---|---|---|
| M-01 | 首次 up | 自动 team create（owner local-owner），TEAM.md bootstrap，非 owner letter 落盘 |
| M-02 | 每成员 import 到其工作区 `.teamx` | `.teamx/letters/…` 出现证书 |
| M-03 | auto-approve | 每成员在 server 侧 active（owner 查询） |
| M-04 | 已 active 成员再次 up | 不重复 create/import/approve（幂等），仅补起进程 |
| M-05 | TEAM.md 无 owner | 复用现有 team；缺 owner 仅警告 |
| M-06 | owner 不在可用会话 | 报清晰错误 |

### 2.3 进程与开箱（复用 51）
| ID | 用例 | 期望 |
|---|---|---|
| P-01 | up 后每非 owner 有 headless opencode + ttyd | ps 显示 running，ttyd URL 可访问 |
| P-02 | 成员进程连上 serve | serve 端成员在线/收到连接 |
| P-03 | stop --all | 进程 + 本 ws serve 停 |

## 3. 测试策略
| 层 | 手段 |
|---|---|
| 单元 | 端口选择、URL 复用判定、幂等判断（纯逻辑） |
| 集成脚本 | `tests/local-team-server-test.sh`：临时 workspace + TEAM.md → up（自举）→ 断言 serve/health/成员 active/进程/ttyd → stop |
| 回归 | `cargo test` + 既有 local-team-test.sh / agents 测试 |

## 4. 入口/出口
- `cargo test -p teamx` 全绿。
- `tests/local-team-server-test.sh` PASSED。
- 手工：浏览器 ttyd URL 打开 agent，能收到/上报团队任务。
