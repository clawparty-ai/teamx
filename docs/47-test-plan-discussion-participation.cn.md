# 测试方案：discussion 参与机制（通知 / AI 唤醒 / per-topic 配额）

> 对应设计 `docs/46-design-discussion-participation.cn.md`。
> 状态：**方案（待执行）**；更新：2026-09-08
> 目标：在开发前锁定可验收用例，按 REST/RPC + 单测分层落地（同 docs/45 思路）。

## 1. 范围

| 目标 | 说明 |
|---|---|
| G1 配额字段 | `members.discussion_quota`（默认 5）存在、可读写、随成员走 |
| G2 主动发言配额 | say/reply 计主动发言；per-topic 独立；达上限服务端拒绝 |
| G3 被@响应免配额 | reply 响应"正文 @ 自己的消息"不计配额、不被上限拦截 |
| G4 quota CLI/RPC | get/set；本人改自己 / owner/lead 改任何人；越权拒绝 |
| G5 inbox discussion | 离线 AI inbox 出现被@/订阅/任务相关的 discussion 新消息 |
| G6 插件 toast | discussion.created/message/closed 专属文案；@我高亮 |

> 本方案 G6 为 opencode-plugin（TS），按 bun 单测/构建验证；其余 Rust/脚本。

## 2. 「主动发言 vs 被@响应」判定（实现准则，可客观执行）

- **计数与拦截只针对"主动发言"**。
- **被 @ 响应** = 本次是 `reply`，且**被回复的那条消息正文包含 `@<本成员 display_name>`**。视为响应他人召唤 → 不计配额、不受上限。
- **主动发言** = 其余一切 say/reply（含普通 reply 与他人、新开 say）。→ 计数。
- 边界：`@` 文本匹配用 `@` + display_name（词边界，避免误配前缀）；大小写不敏感匹配 display_name 但保留原文展示。

## 3. 用例清单

### 3.1 服务端配额（discussion.rs 单测 / RPC 集成）
| ID | 用例 | 期望 |
|---|---|---|
| Q-01 | 默认 quota=5；`quota get` 返回默认 | 5 |
| Q-02 | 成员在话题 T 主动 say/reply 累计到 quota 后再次主动 say | 服务端拒绝（含剩余 N/N） |
| Q-03 | 同成员在另一话题 U 主动发言 | 独立计数，U 不受 T 影响 |
| Q-04 | reply 响应"正文含 @self"的消息 | 不计配额，达上限后仍可发 |
| Q-05 | reply 响应"正文不含 @self"的消息 | 计主动发言，超限被拒 |
| Q-06 | quota 改大（5→8）后 | 可再主动发言至新上限 |
| Q-07 | quota 改小至低于已发言数 | 立即不能再主动发（直到提高） |
| Q-08 | 服务端拒绝只针对主动发言；被 @ 响应永不被拦 | 通过 |
| Q-09 | `@` 匹配词边界：成员 A 存在时 `@Alice2` 不应误命中 `@Alice` | 只精确命中 |

### 3.2 quota CLI/RPC 权限
| ID | 用例 | 期望 |
|---|---|---|
| QC-01 | 成员读自己的 quota | ok |
| QC-02 | 成员写自己的 quota | ok |
| QC-03 | 普通成员写他人 quota | 拒绝 |
| QC-04 | owner/lead 写任意成员 quota | ok |
| QC-05 | `--member` 指定他人 + owner | 目标成员生效 |

### 3.3 inbox discussion（集成/脚本）
| ID | 用例 | 期望 |
|---|---|---|
| I-01 | 讨论新消息含 `@worker`（worker 成员 display_name） | 该 worker inbox `discussions` 出现该条 |
| I-02 | 消息未 @ worker、worker 未订阅、无 task 关联 | inbox 不出现 |
| I-03 | worker 订阅该讨论后，后续消息 | inbox 出现（触发订阅） |
| I-04 | 讨论挂 task 且 worker 为该 task assignee | inbox 出现 |
| I-05 | 已处理水位：worker 处理过 seq N 后，同 seq 不再重复出现 | 不重复 |

### 3.4 插件 toast（opencode-plugin）
| ID | 用例 | 期望 |
|---|---|---|
| P-01 | discussion.created | 「{author} 创建讨论「{title}」」 |
| P-02 | discussion.message | 「{author} 在「{title}」回复：{正文前 80 字}」 |
| P-03 | discussion.message 含 @self | 文案含「(提及你)」/高亮标记 |
| P-04 | discussion.closed | 「讨论「{title}」已关闭」 |
| P-05 | 构建 | `bunx tsc --noEmit` + `bun run build` 通过 |

## 4. 测试策略（分层）

| 层 | 手段 | 位置 |
|---|---|---|
| 单元（Rust） | discussion.rs quota/mention 判定、db 迁移 | `cargo test` |
| CLI | 真实二进制 + 临时 DB：quota get/set 权限 | `tests/quota-test.sh`（新）或并入 cli |
| RPC/集成 | serve + mTLS：主动超限拒 / @响应免配额 / per-topic（仿 discussion-rpc-test.sh） | 扩展 `tests/discussion-rpc-test.sh` |
| inbox | 本地/网络模式断言 inbox.discussions | 脚本 |
| 插件 | TS 单测 + 构建 | `bun` |

## 5. 入口 / 出口准则
- 服务端：`cargo test` 全绿；新增配额单测 + 扩展 RPC 集成脚本通过。
- 插件：构建通过 + toast 单测通过。
- 发布前：`tests/discussion-rpc-test.sh` 含配额场景通过。
