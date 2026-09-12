# IM 接入需求分析（teamx · 远程操作与管理）

> 产出方式：**Consensus Mode grill**（grill-with-docs v2）— 6 个 Design Question 全员作答 + 共识收敛。
> 追溯：grill 线程 `dsc_18d48d030b07c536195df1`（已关闭）；Design Session Record `docs/design/im-access.md`。
> 共识规则：majority（分歧记录 dissent）。leader：caishu。

---

## 1. 背景与定位

teamx 的协作（事件 / 任务 / 提问 / nudge / review）目前只能通过 opencode / dsh / console 触达与操作，
成员必须"在电脑前"。本需求为 teamx 增加 **IM 访问能力**（企业微信 / 钉钉 / 飞书）。

**定位（冻结）**：IM = **control plane（远程操作与管理）**。
- ✅ IM 用于：查看状态、审批、派工/验收、广播决策、问答、目标管理（按白名单）。
- ❌ **执行平面永不进 IM**：代码执行 / 任务跑仍由 opencode / dsh 客户端负责。

## 2. 范围

**In scope**
- 出站通知：台账事件 / 任务分派 / ask / nudge / review → IM（群/私聊）。
- 只读查询：status / goal / tasks / discussions / open questions（限自己相关）。
- 管理动作（白名单，P2 起）：`approve/deny`、`verify/reject`；P2.5 `respond`(自己的 ask)；P3 `publish`/`task assign`(lead)。
- 身份绑定：IM user ↔ teamx member。
- 通知产品化：per-member 订阅 / 静默时段 / 与 Issue #15 共用配置面。

**Out of scope（红线）**
- **高危动作永不进 IM**：PR 合并 / `revoke` / `destroy` / `goal.close`（只推通知 + 回 console 二次确认）。
- `role` / `tunnel` 默认不进白名单。
- 用 IM 执行代码/任务。

## 3. 角色

| 角色 | 能力 |
|---|---|
| owner / lead | 全量白名单管理动作 |
| 普通成员 | self-service：读自己相关 + `ack/claim/done` 自己的任务 + `respond` 自己的 ask |
| 网关（ImGateway） | **零特权**：验签→查绑定→注入委托→转发；不做权限判断 |

## 4. 分阶段需求（P0 → P3）

### P0（前置，必须）
- **dsh 侧 `teamx_discussion` 工具对齐**（PR #19 仅 opencode 侧）——否则 dsh 成员无法在讨论/grill 中发言。
- **B1 重复注入修复**（WS+轮询竞态；今日 30+ 延迟重投为实证）。
- 验收：双端成员均可用**插件工具**（非 curl）完成一次 `discussion.reply`。

### P0.5（条件阶段）
- **企微群机器人 webhook** 单向兜底通知：**仅当目标用户主 IM=企微** 时启用。
- 验收：白名单事件 ≤60s 达群；断网恢复不丢不重（seq 幂等）。

### P1（飞书，只读 + 通知 + 绑定）
- 出站通知（台账事件→IM）+ 绑定后只读查询。
- 验收：①晚导入邀请函→绑定码→绑定成功回显角色（复用 PR #8 晚导入验收法）；②只读结果与 `team.status` diff 为空；③回调重放不产生重复台账（`im_msg_id` 去重）；④每动作落台账（`source: im:<platform>` + `im_user`）。

### P2（审批 / 验收卡片）
- 写动作白名单**仅四个**：`approve/deny` + `verify/reject`；交互式卡片 + 二次确认。
- **引入条件（全部满足）**：① P1 锚点达成；② 绑定+鉴权链路就绪（RPC 白名单冻结，复用 `is_lead`）；③ 出站幂等/聚合先行；④ **故障演练通过**（伪造回调/网关劫持，验签+幂等+权限三层拦截）；⑤ 告警类通道就绪（`budget.alert` 首批进 IM）。
- 验收：IM 内真实 `approve` 闭环 + 台账可审计；非 owner/lead 被拒；重放幂等；断网无半状态。

### P3（产品化 + 多 IM）
- per-member 订阅 / 静默时段自助配置；第二 IM（钉钉 Stream）仅新增 provider、核心零改动。
- 验收：订阅/静默生效；第二 IM 仅加 provider 文件。

## 5. 关键设计决策（DQ 汇总）

### 5.1 IM 选型（DQ-0001）
- **飞书 = Phase1 双向主线**（决定性理由：长连接事件订阅=出站连接，**免公网回调**，契合自托管/NAT）。
- 企微 webhook 仅**条件兜底**；钉钉本轮不做。

### 5.2 身份绑定（DQ-0003）
- **A 一次性绑定码为主 + B 管理员映射兜底；C（邀请函/mTLS 绑定）不采用**。
- 码走**已认证通道**生成、**生成即预关联 member_id**（单向授权：已认证→未认证）；**自助码免审批**，B 兜底需 lead。
- 码：≥8 位 CSPRNG、TTL 5min、单次、**只存 hash**；同 IM user 失败限速（5 次→15min + 告警）。
- 绑定成功**回显角色**；成员 `left/denied` 级联失效；证书轮换不自动解绑；解绑即在途卡片作废。
- 多团队：一行一 team；默认最近活跃 + `/team` 切换；**服务端无状态**（回调带签名 `team_id`）。

### 5.3 白名单与鉴权（DQ-0004）
- 白名单**落成数据表**（method→min_role→source/phase）；变更=owner 决策+台账，不做运行时热改。
- 只读含 **open questions**；P2.5 `respond`(self)；P3 `publish`/`task assign`(lead)、`goal.share` 可议；`role/tunnel` 默认不进；**`goal.close` 永久不进**。
- **鉴权在 RPC 层**（复用 `is_lead`），**网关零特权**（单点鉴权/审计/测试）。
- **网关身份 = B（独立 service 证书 + 委托头）**：CN 命名空间隔离（`service:im-gateway:<instance>` vs `member:<id>:<role>`）；委托头只传 `platform/im_user/im_team`(+签名)，**永不传 member_id**（server 从绑定表反查）；service 证书短 TTL(7d 轮换)+方法级白名单；委托凭证 server 签发短时 token。
- **P1 可暂用 A（内部直调），但按可拆进程写；P2 写动作上线前必须切 B**。
- **绑定表给身份、`members` 表给权限**（角色实时读，不缓存）。
- 负例与审计：拒绝与成功**同等落台账**+区分错误码（`im.unbound/im.forbidden/im.duplicate/invalid_signature`）；8 条负例集成测试；枚举探测告警。

### 5.4 架构与幂等/审计/安全（DQ-0005）
- **ImProvider：Phase2 抽 trait + 首日预留接口**（依赖方向锁定 / 四职责分文件+record 记草案 / 第二实现反抽）。
- 组件：`ImGateway`（独立进程）、`Notifier`（出站，P1 可内置 serve）、`Provider`（各 IM 原语）；布局 `crates/teamx/src/im/{gateway.rs, notifier.rs, providers/{feishu,wecom}.rs, types.rs(ImDelegatedContext)}`；IM 平台故障不阻塞 server；配置集中 `im/config.rs`；密钥分级（gateway 持有，serve 不持有）。
- **幂等**：入站 `im_seen(platform, im_msg_id, ...)` TTL 7d + 表上限；出站 `im_outbox(seq,...)` 以 ledger seq 为键 + **digest 聚合**（60s，key=(team,event_type)，**仅信息性事件；管理动作结果逐条必达**）。
- **审计**：成功/拒绝/绑定/出站/失败 五类事件；原文存**摘要 hash**。
- **安全**：验签+时间戳/nonce(±5min)；IM 文本**先切分命令再校验参数**（防 ReDoS）、禁 shell 透传；限速 im_user×team 双维+全局；出站尊重 429；provider **dry-run** 开关。

### 5.5 通知产品化（DQ-0006）
- **per-member 订阅 + 静默时段（紧急穿透）+ 与 Issue #15 共用配置面**，P3 交付。
- 订阅 per-(member,team,channel)+`event_types` 白名单；默认=与自己相关 + `budget.alert` 全员；群广播类默认关。
- 静默：本地 tz 窗口；非穿透入 digest **不丢**；**穿透白名单仅 `budget.alert` + `im.security_alert`**；直接指派不穿透；加穿透项需 lead 批准。
- **同源**：`nudge_templates` → `notification_templates(team_id, channel, kind, template, ...)`（模板/频控/权限三同源；模板=lead，订阅=成员自助）。
- 形态：飞书卡片；群=广播/私聊=个人；**禁 @all**；配置写动作不进 IM（深链回 console）。
- 治理冻结：事件类型**封闭枚举**、扩展需评审；**心跳类（session.idle）永不进枚举**。

## 6. 数据模型（草案）

- `im_bindings(platform, im_user_id, member_id, team_id, bound_at, bound_by, revoked_at, last_seen_at)` UNIQUE(platform, im_user_id, team_id) + idx(member_id)
- `im_bind_codes(code_hash, member_id, team_id, scope, created_by, created_at, expires_at, used_at)`
- `im_seen(platform, im_msg_id, team_id, first_seq, seen_at)`（TTL 7d + 表上限）
- `im_outbox(seq, team_id, pushed_at)`（出站 seq 幂等）
- `im_action_whitelist(action, min_role, source, phase)`
- `im_notifications(member_id, team_id, channel, event_types, quiet_start, quiet_end, quiet_tz, min_interval_secs, updated_at)`
- `notification_templates(team_id, channel, kind, template, updated_by, updated_at)`

## 7. 台账事件

- `im.binding`（绑定/解绑/换绑）
- 动作事件：成功 payload `source: im:<platform>` / `im_user` / `im_msg_id` / 动作 / `team_id`
- `im.rejected`（+ `reason_code`: unbound/forbidden/duplicate/invalid_signature）
- `im.push`（引用 seq；+ digest 覆盖 seq 范围）、`im.push_failed`（+ reason + retry_count）

## 8. 非功能需求

- **安全**：网关零特权、身份由 server 注入、验签/防重放、命令白名单解析、密钥分级与轮换、dry-run。
- **可靠**：出站单通道（WS push，复用 PR #8 重连/退避）+ server 幂等兜底；IM 平台故障不阻塞 server。
- **可观测**：拒绝/失败同等留痕；全链路 `im_msg_id` 可溯源。
- **性能/刷屏**：digest 聚合、双维+全局限速、心跳类不进枚举。

## 9. 依赖与风险

- **依赖**：P1 起点依赖 PR #8/#17/#11 合并（WS 稳定 + 幂等）；owner 先清合并队列。
- **风险/开放问题**：
  - P3 多网关实例的"一群一实例"分配机制（单写者落地：配置指派 or leader 选举）——**open，不阻塞 MVP**。
  - ~~讨论配额默认 5/话题过低~~ → **已处置（P0 / PR #25）**：新库/新成员默认提升为 20，并新增 `member.discussion_quota` RPC/CLI 可配。**限制（方案 B）**：既有库列默认仍为 5、既有成员保留现值，需运维用 RPC 显式调整，未做全量回填。
  - dsh 侧 `teamx_discussion` 工具对齐（P0）。

## 10. 追溯

- grill 线程：`dsc_18d48d030b07c536195df1`（6 题结论：`#625`/`#645`/`#658`/`#671`/`#684`/`#711`）
- Design Session Record：`docs/design/im-access.md`
- 前置讨论：`dsc_18d47012e374d37b195df1`
