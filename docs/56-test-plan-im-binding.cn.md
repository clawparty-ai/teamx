# 56 · IM 身份绑定（P1）测试计划

> 被测设计：`docs/55-design-im-binding.cn.md`
> 被测代码：`crates/teamx/src/im/binding.rs`、`crates/teamx/src/im/mod.rs`、`crates/teamx/src/db.rs`（v17）、`serve.rs`（`im.*`）、`cli.rs`/`commands.rs`（`teamx im ...`）

## 1. 覆盖层次

| 层 | 载体 | 覆盖内容 |
|---|---|---|
| 单元 | `im::binding::tests`（16 个用例） | 码生命周期、限速、换绑双向唯一、解绑授权、级联失效、审计事件、存储不落明文、尝试表剪除 |
| CLI 验收 | `tests/im-binding-test.sh` | 本地团队的完整闭环 + 负例（单次、跨队、限速、越权），含台账断言 |
| RPC 验收 | `tests/im-binding-rpc-test.sh` | 真实 `teamx serve` + mTLS：邀请函导入 → 审批 → 绑定码 → 绑定 → 重放拒绝 → 列表/解析/解绑 → 台账 |

运行方式：

```bash
cargo test -p teamx                                   # 单元（含 16 个绑定用例）
TEAMX=target/debug/teamx bash tests/im-binding-test.sh       # CLI 验收
TEAMX=target/debug/teamx bash tests/im-binding-rpc-test.sh   # RPC 验收
```

## 2. 单元用例

| 用例 | 断言要点 |
|---|---|
| `code_create_requires_active_membership` | 非本队成员不能签发；TTL 默认 300 |
| `redeem_binds_and_echoes_role` | 绑定成功、角色/显示名回显、`resolve_member` 反查命中且刷新 `last_seen_at`、`im.binding(action=bind)` |
| `code_is_single_use` | 二次兑换 `code_used`；绑定数不增加；`im.rejected` 计数 1 |
| `expired_code_is_rejected` | 过期码 `code_expired`；不产生绑定 |
| `unknown_codes_are_rate_limited_after_five_failures` | 5 次失败后第 6 次即使持有效码仍 `rate_limited`；限速按 IM 身份隔离，他人不受影响 |
| `short_or_oversized_code_input_is_rejected_without_a_lookup` | 长度越界走 `code_format`，不做哈希查询 |
| `rebinding_an_im_user_moves_the_binding_to_the_new_member` | `action=rebind`、`previous.member_id` 正确、旧成员零绑定、解析改为新成员 |
| `a_member_keeps_one_im_identity_per_platform_and_team` | 成员换 IM 账号后仅保留一个身份，旧身份不再解析 |
| `unbind_is_self_service_or_lead_only` | 同队非 lead 同伴被拒（`forbidden`）、本人自助成功、二次解绑报错 |
| `a_lead_may_unbind_another_member` | owner 可解绑他人，团队有效绑定归零 |
| `binding_goes_invalid_when_the_member_leaves` | 成员 `left` 后解析返回 `None`；其为 `left` 时签发的码兑换被拒（`member_inactive`） |
| `listing_is_self_scoped_unless_the_caller_is_a_lead` | 默认自查；非 lead `--all` 被拒；lead 看到全队 |
| `codes_are_never_stored_in_plaintext` | 库中仅 64 位十六进制哈希，不含明文码 |
| `attempt_rows_are_pruned` | 24h 前的尝试行被剪除，不参与限速窗口 |
| `rejections_are_audited_against_the_actors_team` | 有团队上下文时未知码也落 `im.rejected`（含 `source`）；无团队上下文时不写任何团队账本 |

## 3. CLI 验收步骤（`tests/im-binding-test.sh`）

1. 建队 + owner + 成员（成员角色设为 `reviewer` 以便校验角色回显）。
2. 签发绑定码：长度 10、预关联创建者、TTL 上下钳位（5→60、100000→900）、非成员签发被拒。
3. 兑换：`action=bind`、成员与角色回显正确。
4. 重放：`code_used` 拒绝。
5. 列表：本人 1 条（`scope=self`）；非 lead `--all` 被拒；owner `--all` 看到 `scope=team`。
6. 解析：非 lead 被拒；有效身份命中、未知身份 `bound=false`。
7. 换绑：同一 IM 身份换到 owner → `action=rebind`、`previous.member_id` 正确、原成员 0 条、解析指向 owner。
8. 跨队：A 队签发的码在 B 队兑换 → `team_mismatch`。
9. 限速：同 IM 身份连续 5 次失败后，第 6 次持有效码仍 `rate_limited`。
10. 解绑：同队同伴被拒、原成员被拒、本人自助成功、解析失效、二次解绑报错。
11. 台账：`im.binding` 动作为 `{bind, rebind, unbind}`；`im.rejected` 明细含 `code_unknown / code_used / rate_limited / team_mismatch / unbind_not_self_or_lead`；全部带 `source: im:feishu`。

## 4. RPC 验收步骤（`tests/im-binding-rpc-test.sh`）

1. 起 `teamx serve`（mTLS）；成员用邀请函 `team.import` 并经 owner `team.approve` 激活。
2. `im.bind_code.create`：码与调用者成员 id 一致、长度 10。
3. `im.bind`：`action=bind`、成员 id 与角色回显。
4. `im.bind` 重放：`code_used` 拒绝。
5. `im.bindings.list`：本人 1 条；非 lead `all=true` 被拒；owner 看到全队 1 条。
6. `im.resolve`：非 lead 被拒；命中返回成员、未知身份 `bound=false`。
7. `im.unbind`：owner 解绑后解析 `bound=false`。
8. `im.nope`：未知方法被拒（`unknown im method`）。
9. 台账：`im.binding` 动作 `{bind, unbind}`、`im.rejected` 含 `code_used`，且均带 `source: im:feishu`。

## 5. 回归与已知基线

- `cargo test -p teamx`：本 PR 后 **235 passed / 1 failed**。
- 唯一失败为 `server_tasks::tests::task_event_payload_carries_both_field_spellings`，属 **issue-29（`task.log` LIKE 模式带空格）存量失败**，在 `origin/enterprise` 基线上同样失败，由未合并的 PR #30 修复；与本任务无关，不影响绑定路径。
- 本 PR 触及 `db.rs` 迁移，故同步更新了两处 schema 版本断言（16 → 17）；`migrate` 幂等性与旧库收敛测试保持通过。
