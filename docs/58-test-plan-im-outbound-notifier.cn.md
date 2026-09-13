# 测试计划 · IM 出站通知器（im-p1-outbound-notifier）

> 设计：`docs/57-design-im-outbound-notifier.cn.md`
> 需求锚点：`docs/design/im-access-requirements.md` §5.4（幂等/聚合/429/dry-run）、§7（审计事件）、§8（刷屏治理）

## 1. 单元测试（cargo test -p teamx，`im::notifier::tests`）

| # | 用例 | 断言要点 |
|---|---|---|
| U1 | collect_enqueues_whitelisted_skips_heartbeat_and_unknown | 心跳（`kind:session.idle`）与白名单外（`team.created`）不入 outbox；digest 行 `pending`；direct 行同 pass 即 `pushed` 且恰好 1 条 `im.push` |
| U2 | cursor_persists_and_recollection_is_idempotent | 二次 pass collected=0；PK 去重不重复；`im.push` 审计事件不再被收集（无回环） |
| U3 | direct_rows_push_immediately_with_audit | 逐条必达；`im.push` payload `seqs=[seq]`、`digest=false` |
| U4 | digest_defers_within_window_then_aggregates | 窗口内 deferred 不发送；窗口过 → 同 (team,event_type) 聚合 1 条 `im.push`，`seqs` 覆盖 [1,2] |
| U5 | digest_flushes_early_on_batch_cap | 满 20 条立即 flush，不等窗口 |
| U6 | failure_fails_permanently_after_cap_and_audits | 3 次失败后行 `failed` + 1 条 `im.push_failed`（reason/retry_count）；期间行保持 `direct` |
| U7 | rate_limit_backs_off_and_does_not_count_toward_failure_cap | 429 后行永不 `failed`；退避期内 provider 零调用（gate 生效）；退避到期恢复发送 |
| U8 | no_target_groups_means_collect_only | 未配群：只收集不发送，无审计事件 |
| U9 | whitelist_rendering_is_closed_and_digest_body_mentions_range | 枚举封闭性（`im.push`/`team.destroy` 不可入）；心跳判定；digest 文案含 `× N` 与 seq 区间 |

## 2. 验收脚本（tests/im-notifier-test.sh，真实 serve + dry-run）

前置：`cargo build -p teamx`、sqlite3、python3；离线安全（`TEAMX_IM_DRY_RUN=1` 永不出网）。

| # | 步骤 | 断言 |
|---|---|---|
| T-1 | 种子 5 条事件（1 心跳 + 1 白名单外 + 2 digest + 1 direct）→ 启动 serve（interval 1s、digest 3s） | outbox 恰 3 行：心跳/白名单外被跳过 |
| T-2 | direct 行 | `doc.help_requested` 行 `pushed`；`im.push`（digest=false）存在 |
| T-3 | digest 窗口过期 | 两行按 (team,event_type) 各聚合 1 条 `im.push`（digest=true，共 2 条） |
| T-4 | 再等一个周期 | outbox 稳定 3 行（审计不回环）；`im.push_failed` = 0 |

稳定化：脚本内 sqlite 读带 `.timeout 5000`（避开 serve WAL 写锁）；连续 3 次运行全过。

## 3. 边界与负例（设计对齐）

- **未配置群**：serve 不启动通知任务（无 outbox 副作用之外的行为）。
- **dry-run 默认开**：不设 `TEAMX_IM_FEISHU_APP_ID/_SECRET` 时 live 发送在取 token 步即失败并计入重试，永不出网。
- **429 语义**：错误串含 `429` 才触发退避；其它错误走失败计数，两种路径互不污染。
- **无禁词 / 红线**：事件只推白名单；执行平面内容（任务体/命令）不进 IM 文案（仅标题/摘要字段）。

## 4. 手工验收（可选，真实飞书）

1. 建飞书自建应用，取 app_id/app_secret，加机器人进群，取群 `chat_id`。
2. `TEAMX_IM_NOTIFY_GROUPS=<chat_id> TEAMX_IM_DRY_RUN=0 TEAMX_IM_FEISHU_APP_ID=... TEAMX_IM_FEISHU_APP_SECRET=... teamx serve ...`
3. 触发 `publish`/任务求助 → 群内 ≤15s+60s 收到 digest；触发 `task.nudge` → 秒级收到单条。
4. 台账核对：`im.push` 引用 seq 与群消息内容一致。
