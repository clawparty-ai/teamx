# 测试方案：console Discussion / teamx agents / kanban 明细 / discussion 事件（batch 2026-09-07）

> 对应设计：`docs/41-design-kanban-detail.cn.md`、`docs/42-design-teamx-agents.cn.md`、`docs/43-design-discussion.cn.md`、`docs/44-design-console-discussion.cn.md`。
> 状态：**方案定稿**。本文件是"开发前置"缺失后补的补救文档——以下功能在开发时未先有本测试方案，现统一补订并据此执行/验收。
> 更新：2026-09-07

---

## 0. 诚实声明（追溯范围）

以下功能属于本测试方案覆盖范围，但**是在没有测试方案的情况下先行开发**的（违反"先测试方案后开发"纪律）：
1. `teamx agents`（spawn/list/stop 多实例管理器）
2. `teamx discussion`（CLI + serve RPC 事件模型 + 引用）
3. kanban T1–T3（api_kanban 全 kind/排序 + 前端明细卡）
4. user-console Discussions 页面（BBS）
5. opencode-plugin activity 上报修复（headless 免 session）
6. user-console Events 修复（最新在前 + 分页）

本文档为它们建立**正式测试方案**，作为当前/回归测试的基准。方案中"当前状态"如实标注已实现与未实现。

## 1. 范围与目标

| 目标 | 说明 |
|---|---|
| G1 discussion 事件模型 | `discussion.created/message/closed` 写入 ledger、seq 单调、引用(坐标+快照)不悬空、close 后禁言、权限（owner/lead 才能 close） |
| G2 discussion RPC 映射 | serve `discussion.*` → 正确 Command；网络成员可跨机使用 |
| G3 console Discussions | panel(list/open) + POST action(create/say/reply/close) 数据通道正确 |
| G4 agents 进程管理 | spawn/list/stop/remove、registry 持久化、端口避让、并发锁、非法输入拒绝 |
| G5 kanban 数据逻辑 | api_kanban 列排序（活跃在前/空沉底）、events 全 kind 聚合、summary |
| G6 UI 前端 | console Discussion/Events 页面 JS 行为（人工/浏览器级，自动化为可选） |
| G7 插件 activity | headless `opencode run` 下 activity 能入库（不依赖 session） |

## 2. 测试策略（分层）

| 层 | 手段 | 位置 | 运行 |
|---|---|---|---|
| 单元（Rust） | `#[cfg(test)]` 直测函数/状态机 | `discussion.rs`、`agents.rs`、`ui.rs`、`console_data.rs` | `cargo test` |
| RPC 映射 | serve 分派表 → Command 结构断言 | `serve.rs` tests | `cargo test` |
| CLI 集成 | 真实二进制 + 临时 SQLite + 临时 TEAMX_HOME | `tests/` shell / rust-集成 | `./tests/run-all.sh` |
| HTTP（console） | 起 `teamx console` → 带 cookie 调 panel/action API | 脚本 | 手动/可选脚本 |
| 前端 JS | 提取 JS `node --check`（语法）；行为走浏览器/console API | CI 可做语法层 | 手动 E2E |
| 手工 E2E | 浏览器 Discussions 页：建话题/发言/引用/关闭/agent 参与 | 人工 | 见 §7 |

## 3. 测试环境与隔离

- Rust 工具链；需要真实 `opencode` 二进制的 agents spawn 用例用 `--opencode` 指向测试桩（可用 `/bin/cat` 之类? 不——需要能 serve 的二进制；改用「spawn 后立即退出则报错」路径 + registry 逻辑测试，真实 spawn 仅手工）。实际：`agents` 单测聚焦 registry/校验/端口/stop 逻辑，**不 spawn 真 opencode**（进程管理真跑用脚本手工验证）。
- 一律 `TEAMX_HOME`/`TEAMX_DB` 指向 `mktemp` 临时目录；需要改全局 env 的测试用全局 `Mutex` 串行（同 commands.rs 既有模式）。
- 不触碰 `~/.teamx/teamx.db`。

## 4. 用例清单

### 4.1 discussion.rs（当前 3 通过；需补 4.1.x）
| ID | 用例 | 期望 | 状态 |
|---|---|---|---|
| D-01 | create→say→reply(引用)→open→close 全流程 | 消息楼层正确；reply_to 含原文快照 | ✅ 已实现（test 3） |
| D-02 | reply 引用不存在的消息 | 拒绝 | ✅ 已实现 |
| D-03 | 非本队/未知 session | 拒绝 | ✅ 已实现 |
| D-04 | close 后 say | 拒绝 | ✅ 已实现（并入 D-01） |
| D-05 | 非 lead 成员 close | 拒绝 | ✅ 已实现 |
| **D-06** | **quotes[] 多引用 + 服务端快照生成** | quotes 落事件，text 由服务端生成非客户端伪造 | ❌ 待补 |
| **D-07** | **跨队引用**（引用他队 message/event） | 拒绝 | ❌ 待补 |
| **D-08** | **消息正文/引用长度上限**（≤4KB / quotes≤8 / 快照≤500） | 超限拒绝或截断 | ❌ 待补 |

### 4.2 serve.rs discussion RPC 映射（当前 0）
| ID | 用例 | 期望 | 状态 |
|---|---|---|---|
| R-01 | `discussion.*` 经 /rpc 全链路（create/say/reply/open/list/close + 权限/close后禁言） | 见 tests/discussion-rpc-test.sh 全过 | ✅ tests/discussion-rpc-test.sh |
| R-02 | 未知 action | 报 unknown | ✅ tests/discussion-rpc-test.sh |

### 4.3 console_data discussion（当前 0）
| ID | 用例 | 期望 | 状态 |
|---|---|---|---|
| C-01 | fetch_panel("discussions") 网络模式 | 返回 discussion.list 结果（console 走同一 net_rpc） | 🟡 由 RPC 集成覆盖 + console HTTP 冒烟 |
| C-02 | fetch_panel("discussion?id=…") | 返回 discussion.open 结果 | 🟡 同上 + 手工冒烟（已验证楼层/引用） |
| C-03 | discussion_action create/say/reply/close 本地模式参数拼装 | 生成的 CLI args 正确 | ❌ 待补 |
| C-04 | discussion_action 未知 action | Err | ❌ 待补 |

### 4.4 agents.rs（当前 0）
| ID | 用例 | 期望 | 状态 |
|---|---|---|---|
| A-01 | registry 空 load | 默认空 registry | ❌ 待补 |
| A-02 | spawn 重名拒绝 / 空名拒绝 / 含空格斜杠名拒绝 | Err | ❌ 待补 |
| A-03 | spawn daemon 后 registry 含该实例；list 显示 running | 状态正确 | ⚠️ 依赖真 opencode，脚本级；单测做 registry 往返 |
| A-04 | pick_port 避让已占用端口 | 返回空闲端口 | ❌ 待补 |
| A-05 | stop 不存在实例 | Err not found | ❌ 待补 |
| A-06 | stop --remove 后 registry 移除 | 记录删除 | ❌ 待补 |
| A-07 | 并发 spawn 同端口（flock） | 串行、端口不重复、registry 不丢 | ⚠️ 集成级 |

### 4.5 ui.rs api_kanban（当前 0 新增）
| ID | 用例 | 期望 | 状态 |
|---|---|---|---|
| K-01 | 两 user：一有近期 activity、一无 → 列排序活跃在前 | order 正确 | ✅ tests/ui-api-test.sh |
| K-02 | 无 activity user 沉底 | 空 tasks/events 置尾 | ✅ tests/ui-api-test.sh |
| K-03 | tool_call 进 events 而非 tasks | 断言 events 含 tool_call、tasks 均为 work_session | ✅ tests/ui-api-test.sh |

### 4.6 前端 JS（console_html/ui）
| ID | 用例 | 期望 | 状态 |
|---|---|---|---|
| J-01 | 内嵌 JS 语法 | `node --check` 通过 | ✅ 已实现（手动步骤） |
| J-02 | console Discussions E2E | 浏览器建/回/引/关/轮询 | ⚠️ 手工 |
| J-03 | console Events 分页 E2E | 最新在前、翻页更早 | ⚠️ 手工 |

### 4.7 插件 activity（opencode-plugin）
| ID | 用例 | 期望 | 状态 |
|---|---|---|---|
| P-01 | tsc/build | 无类型错、构建成功 | ✅（AGENTS 默认） |
| P-02 | headless run 后 activity 入库（network 无 session） | DB activity 有新行 | ✅ 已在真实节点验证（zhongm 35 行） |
| P-03 | 交互式 session activity 仍正常 | 不回归 | ⚠️ 回归时验证 |

## 5. 入口 / 出口准则

- `cargo test -p teamx` 全绿是**最低入口**。
- agents/discussion/kanban 变更：`cargo test` + 本方案 4.x 用例通过。
- console UI 变更：J-01 语法 + 手工 E2E（J-02/J-03）。
- 插件变更：`bun run build` + P-02 场景复验。
- 发布到云端前：跑 `./tests/run-all.sh`（或至少 cargo test + 关键脚本）。

## 6. 执行记录（2026-09-07）

1. ✅ agents.rs 单测 7 项（registry/重名/非法名/pick_port/stop 不存在/pid 缺失）。
2. ✅ discussion.rs D-06/07/08（quotes 服务端快照、跨队引用、长度上限，含补实现）。
3. ✅ R-01/02 → `tests/discussion-rpc-test.sh`（serve /rpc 全链路，REST 黑盒，不经重构）。
4. ✅ K-01..03 → `tests/ui-api-test.sh`（/api/kanban REST 断言）。
5. 🟡 C-01..04：console 数据通道与 serve RPC 同走 `net_rpc`，网络分支已由 R-01 覆盖；console HTTP 层（/api/panels/discussions、/api/discussion/{action}）经手工冒烟验证（建/说/引用/开/关全链路）。若需自动化可后续加 console local-mode HTTP 脚本。
6. 保留 §7 手工 E2E 清单（agent 真实参与、浏览器翻页/引用交互）。

## 7. 手工 E2E 清单（验收用，非自动化）
- [ ] console Discussions：列表→建话题→两条消息→引用回复显示引用块→刷新/轮询可见 agent 回复→owner close→列表 closed 灰显→再发言被拒
- [ ] console Events：最新在上、翻页到最旧、按钮消失
- [ ] agents：两台真 opencode 实例端口错开、stop/--remove

## 8. 相关文档
- `docs/41/42/43/44-design-*.cn.md`
- `docs/08-test-plan.cn.md`（仓库总测试方案风格参照）
