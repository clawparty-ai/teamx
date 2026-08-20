# teamx V1 测试案例（Test Cases）

约定：
- 每条用例给出自动化落点（`cargo test` 的测试名 / `tests/*.sh` 的步骤 / 手工），无自动化落点的为手工用例。
- CLI 用例以 `teamx <cmd>` 表示，运行前 `export TEAMX_DB=$(mktemp ...).db`。

## A. 单元测试（`cargo test`）

| ID | 名称 | 步骤 | 预期 | 落点 |
|---|---|---|---|---|
| TC-001 | Team 正常路径 | forming→share_goal→active→blocked→resumed→close_goal→completed→archive | 各步状态正确 | `state::tests::team_happy_path` |
| TC-002 | Team 非法转换拒绝 | forming 下 close_goal / archived 下再 archive 等 | 返回 Err | `state::tests::team_illegal_transitions_rejected` |
| TC-003 | Team 中性动作 | active 下 publish start/progress/decision/refine/achieved | 状态保持 active；blocked 下这些动作不解除阻塞 | `team_neutral_actions_keep_active` |
| TC-004 | Member 正常路径 | pending→approve→active→ask→waiting→respond→active→idle→active | 状态正确 | `member_happy_path` |
| TC-005 | Member 离开 | 从 pending/active/waiting/idle 均可 leave→left；left/denied 后不可再 approve/ask | 正确 + 拒绝 | `member_leave_from_any_state` |
| TC-006 | Member 角色切换 | set_role 对 active/idle 中性；pending 选角色即激活 | 状态正确 | `member_role_set_is_neutral` |
| TC-007 | Goal 正常路径 | proposed→shared→in_progress→blocked→in_progress→achieved→closed | 状态正确 | `goal_happy_path` |
| TC-008 | Goal refine 流程 | shared/in_progress→refine→refining→start→in_progress | 状态正确 | `goal_refine_flow` |
| TC-009 | Goal 非法转换拒绝 | closed 后一切动作、跳过中间态等 | 返回 Err | `goal_illegal_transitions_rejected` |
| TC-010 | 状态字符串回环 | 各状态 from_str/as_str 互逆；未知串为 None | 通过 | `state_string_roundtrip` |
| TC-011 | 每 team seq 独立单调 | 两个 team 交错写，各得 1..3 | 各自单调、互不影响 | `events::tests::seq_is_monotonic_and_independent_per_team` |
| TC-012 | 游标排他读取 | list(after=N) 只返回 seq>N | 正确 | `list_after_cursor_is_exclusive` |
| TC-013 | 游标推进 | cursor_for 默认 0；set_cursor 后读取正确；可覆盖 | 正确 | `cursor_advance_is_idempotent` |
| TC-014 | payload JSON 回环 | 中文字符串/数字/布尔对象经账本存取 | 相等 | `payload_roundtrips_through_json` |

## B. CLI 冒烟·正常路径（`tests/smoke.sh`）

| ID | 名称 | 步骤 | 预期 |
|---|---|---|---|
| TC-101 | 初始化 | `teamx init`（两次） | 幂等，ok:true |
| TC-102 | 建队 | `teamx team create "Test Team" --session inst:alice` | 返回 team id + invite_token，state=forming |
| TC-103 | 加入 | `teamx team join <token> --name Bob --session inst:bob` | pending，提示等待审批 |
| TC-104 | 审批 | owner `teamx team approve <member_id>` | 成员 active |
| TC-105 | 选角色 | `teamx role set contributor --session inst:bob` | role=contributor |
| TC-106 | 设目标 | owner `teamx goal set "Ship the MVP" --body ...` | goal proposed |
| TC-107 | 分享目标 | owner `teamx goal share` | goal shared，team active |
| TC-108 | 汇报进展 | `teamx publish progress --data '{"message":"..."}'` | goal in_progress，事件落账 |
| TC-109 | 提问/应答 | owner `ask` → Bob `respond` | Bob waiting→active，question answered |
| TC-110 | 报告完成 | `teamx publish achieved` | goal achieved |
| TC-111 | 关闭目标 | owner `teamx goal close` | goal closed，team completed |
| TC-112 | 终态同步 | member `teamx sync` | 看到 team.completed 等全部事件 |
| TC-113 | 账本有序 | `teamx events --team <id>` | seq 严格递增 |

## C. CLI 边界 / 负面（`tests/cli-test.sh`）

| ID | 名称 | 步骤 | 预期 |
|---|---|---|---|
| TC-201 | 坏 token | `team join bogus-token` | 拒绝 |
| TC-202 | 重复加入 | 同 session 二次 join 同一团队 | 拒绝 |
| TC-203 | 越权审批 | 非 owner approve/deny | 拒绝 |
| TC-204 | 越权目标操作 | 非 owner share/close goal、代指定角色 | 拒绝 |
| TC-205 | 审批非 pending | 对已 active 成员 deny | 拒绝 |
| TC-206 | 拒绝流 | 第二成员 join 后 owner deny | 成员 denied，sync 被拒 |
| TC-207 | 未知角色 | `role set wizard` | 拒绝并提示目录 |
| TC-208 | 非法 publish 类型 | `publish teleport` | 拒绝并列出合法类型 |
| TC-209 | 未设目标即 publish | 新团队直接 `publish progress` | 拒绝，提示先设目标 |
| TC-210 | 自问自答 | owner ask 自己 | 拒绝 |
| TC-211 | 非目标应答 | 非 target 者 respond | 拒绝 |
| TC-212 | 重复应答 / 未知 id | 已 answered 再 respond / respond 不存在 id | 拒绝 |
| TC-213 | 多团队消歧 | 同一 session 加入两队；不带 --team 的 status/publish | 拒绝并列出 team 列表；带 --team 成功 |
| TC-214 | 游标语义 | sync 推进→空；--no-advance 不推进（两次都能看到新事件）；正常 sync 推进后为空 | 见用例 |
| TC-215 | events 需 --team | `events` 不带 --team | 拒绝 |
| TC-216 | 离开 | leave 后再次 leave；成员 state=left | 第一次成功，二次拒绝 |
| TC-217 | 完成后禁止加入 | completed 团队新 join | 拒绝 |
| TC-218 | loopx 未绑定/不可用 | 非成员 loopx report；不存在的项目目录 | 明确提示，不影响闭环 |

## D. 并发（部分并入 C）

| ID | 名称 | 步骤 | 预期 |
|---|---|---|---|
| TC-301 | 并发写 seq 单调 | 5 会话 × 3 并行 publish（`tests/concurrency.sh`） | 15 条事件 seq 严格递增且唯一 |

## D2. 三人协作（`tests/three-member.sh`，等价于 demo-team）

| ID | 名称 | 步骤 | 预期 |
|---|---|---|---|
| TC-401 | 三人闭环 | owner 建队+目标；contributor/reviewer 各自 join+申请角色（保持 pending）；owner 审批两人+分享目标；contributor 产设计 progress；reviewer sync 看到并产评审 progress；owner ask→contributor respond；owner 广播决策；contributor achieved；owner close+archive | 角色=contributor/owner/reviewer 全 active；终态 archived/closed；事件链含全部关键类型 |

## E. 插件注册（构建期探测）

| ID | 名称 | 步骤 | 预期 |
|---|---|---|---|
| TC-401 | 插件类型检查 | `bunx tsc --noEmit` | 0 错误 |
| TC-402 | 插件打包 | `bun run build` | dist/teamx.js 生成 |
| TC-403 | agent 注册 | `opencode agent list` | 出现 `teamx (all)` |
| TC-404 | 命令注册 | opencode serve 后 GET `/command` | 列表含 `Team` |
| TC-405 | 工具注册 | GET `/experimental/tool/ids` | 含全部 17 个 `teamx_*` |

## F. 手工验收（真实 opencode 双窗口）

| ID | 名称 | 步骤 | 预期 |
|---|---|---|---|
| TM-01 | 完整闭环 | 窗口 A `/Team 创建团队 "Demo" ...`；窗口 B `/Team 加入 <token> ...`；A 审批并分享目标；B 选角色、汇报进展、问澄清、报告完成；A 验证关闭 | 双窗口状态一致，事件可追溯，team completed |
| TM-02 | 多成员角色 | 3 窗口：owner + contributor + observer | observer 只读观察；owner 广播被所有成员 sync 到 |
| TM-03 | loopx 联动 | member 绑定 loopx 项目，`teamx_loopx_report` 发布，owner sync 可见 loopx.progress | 事件含 goal_state/gate/next_todo/quota |
| TM-04 | 三人协作 | 3 窗口：owner + contributor + reviewer，走 `docs/13-demo-team.md` 全流程 | 终态 archived/closed；design-plan.md + review-plan.md 产出 |
| TM-05 | 模型级验收（headless） | `tests/acceptance.sh`：`opencode run --agent teamx` 让真实模型建队 | 账本出现 team.created/goal.set，团队名正确 |
