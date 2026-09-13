# 55 · IM 身份绑定（P1）设计说明

> 任务：`im-p1-feishu-binding`（IM P1 关键路径第一项）
> 关联：`docs/design/im-access-requirements.md` §5.2（DQ-0003 身份绑定）、§5.3（DQ-0004 白名单与鉴权）、§5.4（DQ-0005 幂等/审计/安全）、§6（数据模型）、§7（台账事件）
> 代码：`crates/teamx/src/im/binding.rs`、`crates/teamx/src/im/mod.rs`、`crates/teamx/src/db.rs`（v18）、`crates/teamx/src/serve.rs`（`im.*` 路由）、`crates/teamx/src/{cli.rs,commands.rs}`（`teamx im ...`）

## 1. 范围

**本次实现（P1 的身份半场）**

- 一次性绑定码：生成（自助、免审批）、兑换、单次消费、TTL 与限速。
- 绑定表：绑定 / 解绑 / 换绑，一个 IM 身份在一个团队内只对应一个成员。
- 身份解析：入站动作由 `platform + im_user_id + team_id` 反查成员（网关不携带 `member_id`）。
- 审计：绑定类与拒绝类动作同等落台账。
- 双通道：CLI（本地/workspace 团队）与 RPC（网络团队），语义一致。

**不在本次范围**：飞书长连接入站、出站通知与 digest、只读查询、P2 审批卡片、网关独立进程（服务证书身份 B）。

## 2. 数据模型（schema v18，幂等迁移）

| 表 | 关键列 | 说明 |
|---|---|---|
| `im_bind_codes` | `code_hash` PK、`member_id`、`team_id`、`scope`、`created_by`、`created_at`、`expires_at`、`used_at` | **只存 SHA-256**，明文码仅在创建响应中出现一次；`used_at` 即单次消费标记 |
| `im_bindings` | PK(`platform`,`im_user_id`,`team_id`)、`member_id`、`bound_at`、`bound_by`、`revoked_at`、`last_seen_at` | 身份源；`revoked_at IS NULL` 表示有效；索引 `(member_id)` |
| `im_bind_attempts` | `platform`、`im_user_id`、`attempted_at`(epoch 秒)、`ok` | 限速依据；写入时顺带剪除 24h 前的行（表有界） |

设计说明：

- `attempted_at` 用 **epoch 秒**而非 RFC3339 文本：限速窗口过滤不依赖文本格式（RFC3339 的 `T`/偏移量与 SQLite `datetime()` 输出不可直接比较）。
- `im_bindings` 的主键就是「一个 IM 身份在一个团队」的自然身份，因此换绑用 `ON CONFLICT ... DO UPDATE` 替换该行，而不是插入新行后留悬挂旧行。

## 3. 流程

### 3.1 生成（已认证通道 → 未认证身份的单向授权）

```
member --(CLI/RPC)--> create_code(member_id, team_id, ttl?)
  校验：调用者是该团队的活动成员（state ∉ {left, denied}）
  ttl  = clamp(入参 ?? 300, 60, 900)
  code = 10 位 CSPRNG（30 字符无歧义表，拒绝采样，无模偏）
  写入：code_hash = SHA-256(code)，member_id 预关联
  返回：明文 code（仅此一次）+ expires_at
```

### 3.2 兑换（网关路径，P1 进程内直调）

校验顺序（先便宜后昂贵，任何失败都计数并落 `im.rejected`）：

1. `platform` / `im_user_id` 非空；码长度合法（8..=64，避免无意义哈希）。
2. **限速**：同一 `(platform, im_user_id)` 15 分钟内失败 ≥5 次 → 即使拿出有效码也拒绝（`rate_limited`）。
3. 按哈希查码：不存在 → `code_unknown`；已用 → `code_used`；过期 → `code_expired`。
4. 队匹配：调用方所在团队 ≠ 码所属团队 → `team_mismatch`（跨队兑换被拒，且计为失败）。
5. 成员在职：`left/denied` → `member_inactive`。
6. **单事务**：条件消费码（`UPDATE ... WHERE used_at IS NULL`，`changes==1` 才算赢）→ 撤销该成员在本平台+团队的其它有效绑定 → upsert 该 IM 身份绑定 → 写 `im.binding`（`action = bind|rebind`）。

返回体回显成员 `id / display_name / role`，以及 `previous.member_id` / `previous.im_user_id`（换绑时告知被替换的一方）。

### 3.3 换绑语义（双向唯一）

- 一个 `(platform, im_user_id, team_id)` 只对应一个成员：同一 IM 身份用新码兑换后，旧成员即刻失去身份（旧行被替换，历史留在 `im.binding` 事件里）。
- 一个成员在一个 `platform + team` 只保留一个 IM 身份：成员用新 IM 账号兑换后，旧 IM 身份被撤销（`revoked_at` 置位）。
- 解绑后立即生效：`resolve_member` 不再解析（"解绑即在途卡片作废"在 P1 无卡片，语义由身份解析体现）。

### 3.4 解绑

本人自助；团队 owner / co-lead 可解绑任意成员。非本人且非 lead → `forbidden/unbind_not_self_or_lead`。二次解绑返回「无有效绑定」错误而不是静默成功。

### 3.5 身份解析（入站网关的身份源）

`resolve_member(platform, im_user_id, team_id)` → `None` 或成员信息，并刷新 `last_seen_at`。绑定有效但成员已 `left/denied` 时同样返回 `None`：**级联失效按成员状态判定，不改绑定行**（成员被重新批准后身份自然恢复，无需重新绑定）。

## 4. 权限与安全

| 议题 | 结论 |
|---|---|
| 身份 vs 权限 | **绑定表给身份、`members` 表给权限**：角色实时读取，不缓存进绑定 |
| 自助 vs 审批 | 自助码免审批（DQ-0003）；码生成即预关联 `member_id`，单向授权 |
| 码强度 | ≥8 位（实现 10 位 / 30 字符表 ≈ 49 bit），TTL 5 分钟，单次，**只存哈希** |
| 限速 | 同 IM 身份 15 分钟 5 次失败后拦截（含第 6 次有效码）；失败计数随成功清零窗口自然过期 |
| 审计 | 成功与拒绝同等落台账；拒绝事件带冻结 `reason_code` + 细分 `detail`，并带 `source: im:<platform>` |
| 跨队隔离 | 兑换必须与调用方团队一致；RPC 侧团队由**证书身份**解析，成员只能在自己团队内动作 |
| 网关特权 | 网关零特权：只传 `platform / im_user_id / im_team`（+签名），**永不传 `member_id`**；P1 采用 DQ-0004 方案 A（进程内直调），P2 写动作上线前切服务证书身份 B |
| 不可归属的拒绝 | 码未知且调用方团队未知（进程内网关未传团队）时，拒绝只作为返回值，不写任何团队台账——避免把噪音写进无关团队的账本 |

## 5. 接口

**CLI**（`teamx im ...`，作用于本地 workspace DB）

| 命令 | 作用 |
|---|---|
| `im bind-code [--ttl-secs n] --session --team` | 生成一次性绑定码（自助） |
| `im bind <CODE> --im-user --platform --session --team` | 兑换（P1 进程内网关路径 / 演示与验收） |
| `im unbind --im-user --platform --session --team` | 解绑（本人或 lead） |
| `im bindings [--all] --session --team` | 查看绑定（默认自己；`--all` 仅 lead） |
| `im resolve --im-user --platform --session --team` | 反查身份（仅 lead） |

**RPC**（网络团队，`im.*`）：`im.bind_code.create` / `im.bind` / `im.unbind` / `im.bindings.list` / `im.resolve`。
团队解析与 `task.*` 一致：未传 `team` 时取调用者的唯一团队，且始终校验证书身份属于该团队。

## 6. 台账事件

| 事件 | payload 要点 |
|---|---|
| `im.binding` | `action` = `bind` / `rebind` / `unbind`；`platform`、`im_user`、`member_id`、`team_id`、`bound_at`、`source: im:<platform>`；换绑带 `previous_member_id` / `previous_im_user_id` |
| `im.rejected` | `reason_code` ∈ `unbound` / `forbidden` / `duplicate` / `invalid_signature`（冻结枚举）；`detail` ∈ `code_format` / `code_unknown` / `code_used` / `code_expired` / `rate_limited` / `team_mismatch` / `member_inactive` / `unbind_not_self_or_lead`；`source` 同上 |

## 7. 开放问题与后续

1. **码创建未落台账**：§7 的事件枚举未含「码已签发」，故本次不新造事件类型。若需要签发审计，建议单独提案 `im.code_issued`（补进枚举后再实现）。
2. **`last_seen_at` 写放大**：每次入站解析都会写一次绑定表。入站网关落地（`im-p1-inbound-gateway-security`）时应加节流或按连接合并，避免每条 IM 消息一次写。
3. **P2 前置**：网关独立进程 + `provider.verify()` 验签 + 委托短时凭证；本次的 `redeem()` 不做平台 I/O，正好留给网关层处理。
4. **多团队切换**：需求为「一行一 team，默认最近活跃 + `/team` 切换」，服务端无状态（回调带签名 `team_id`）。解析接口已按 `team_id` 显式入参，切换逻辑属入站网关范围。

## 8. 验收锚点（对应需求 §4 P1 四条）

1. 晚导入邀请函 → 绑定码 → 绑定成功回显角色：`tests/im-binding-rpc-test.sh`（真实 `serve` + mTLS，成员经邀请函导入并获批后完成绑定）。
2. 只读结果与 `team.status` diff 为空：属 `im-p1-readonly-query`；本次提供其身份前提（`im.resolve`）。
3. 回调重放不产生重复台账：码单次消费 + `code_used` 拒绝且落 `im.rejected`（单元 + CLI + RPC 三层覆盖）。
4. 每动作落台账（`source: im:<platform>` + `im_user`）：`im.binding` / `im.rejected` 双事件断言。
