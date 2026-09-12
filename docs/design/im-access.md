---
status: completed
protocol_version: 2
owner: caishu
created_at: 2026-09-12
updated_at: 2026-09-12
---

# IM 接入方案（远程操作与管理）

## Context
给 teamx 增加 IM 访问能力（企业微信/钉钉/飞书）。定位：IM = **control plane（远程管理与操作）**，
执行仍归 opencode/dsh。Consensus Mode grill：1 session = 1 discussion，逐题串行。

- grill discussion: `dsc_18d48d030b07c536195df1`
- 共识规则: majority（分歧记录 dissent）
- leader: caishu

## Settled Decisions
<!-- Consensus Mode: one entry per DQ, e.g.
- DQ-0001 <title>: <consensus> — rationale: …; dissent: …; unanimity: unanimous|majority|leader-decides; discussion: <thread id>#<message id> -->

- **DQ-0001 目标范围与首个接入的 IM**: 范围 = control plane（远程操作与管理），**执行平面永不进 IM**；**首个 IM = 飞书**（Phase1 双向主线）；企微群机器人 webhook 仅作**条件兜底**（仅当目标用户主 IM=企微 时）；钉钉本轮不做；**高危动作（合并/revoke/destroy/goal.close）永不进 IM**（只通知+回 console）。 — rationale: 飞书长连接事件订阅=出站连接，免公网回调，契合自托管/NAT；dissent: 无实质反对（weiye 的「D 条件化」由 leader 采纳）；unanimity: majority；discussion: `dsc_18d48d030b07c536195df1#603`（结论 `#625`）。
  - 遗留：ImProvider 抽象时机 → DQ-0005；dsh 侧 `teamx_discussion` 工具对齐（P0）→ 任务；出站幂等/聚合 → DQ-0005/DQ-0006。

- **DQ-0002 分阶段边界与验收锚点**: 采用 A（P1 只读+通知 → P2 审批/验收卡片），否决 B/C；阶段 **P0→P0.5(条件)→P1→P2→P3**；**P0 = dsh `teamx_discussion` 工具对齐 + B1 重复注入修复**；审批引入条件（P1 锚点达成 / 绑定+鉴权就绪含 RPC 白名单冻结 / 出站幂等聚合先行 / 故障演练通过 / 告警通道就绪）；**P2 首批白名单仅 approve/deny + verify/reject**；P1 依赖 PR #8/#17/#11 合并。 — rationale: 触达未验证不上写动作、定位成本；dissent: 无实质反对；unanimity: majority（A unanimous）；discussion: `dsc_18d48d030b07c536195df1#626`（结论 `#645`）。

- **DQ-0003 身份绑定方案**: **A（一次性绑定码）为主 + B（管理员映射）兜底；C 不采用**。码走已认证通道生成、**预关联 member_id**（单向授权），自助码免审批；≥8 位 CSPRNG/TTL 5min/单次/hash 存储/失败限速；绑定成功回显角色；成员 left/denied 级联失效、证书轮换不自动解绑、解绑即在途卡片作废；一行一 team、服务端无状态、回调带签名 team_id；表 `im_bindings` + `im_bind_codes` + `im.binding` 台账。 — rationale: 单向授权+预关联防冒用；dissent: caicai 主张码需 lead 审批，weiye 反对 → leader 采纳 weiye（自助免审批）；unanimity: majority；discussion: `dsc_18d48d030b07c536195df1#646`（结论 `#658`）。

- **DQ-0004 管理动作白名单与鉴权（含网关 RPC 身份）**: 白名单**落成数据表**（method→min_role→source/phase）；P2 写动作仅 approve/deny+verify/reject，只读含 open questions，P2.5=respond(self)，P3=publish/task assign(lead)/goal.share 可议，role/tunnel 默认不进，**goal.close 永久不进**；鉴权在 **RPC 层**（复用 is_lead），**网关零特权**；网关身份 **B（service 证书 + 委托头，CN 命名空间隔离，永不传 member_id，server 反查绑定表；短 TTL+方法级白名单）**；P1 可暂用 A 但按可拆进程写、P2 前必切 B；**绑定表给身份、members 表给权限**；拒绝与成功同等落台账+区分错误码+8 条负例测试。 — rationale: 单点鉴权/审计/测试、blast radius 隔离；dissent: 无实质反对；unanimity: majority；discussion: `dsc_18d48d030b07c536195df1#659`（结论 `#671`）。

- **DQ-0005 架构与幂等/审计/路由/安全**: **ImProvider = Phase2 抽 trait + 首日预留接口**（依赖方向锁定 / 四职责分文件+record 记草案 / 第二实现反抽）；ImGateway 独立进程(B)，Notifier 出站(P1 可内置 serve)，布局 `crates/teamx/src/im/{gateway,notifier,providers/<im>,types(ImDelegatedContext)}`；入站 `im_seen` TTL 7d+表上限、出站 `im_outbox` seq 幂等 + digest 聚合（仅信息性事件，**管理动作结果永不聚合**）；审计四类事件+摘要 hash+失败重试；安全（验签+nonce、命令切分防 ReDoS、双维+全局限速、密钥分级、dry-run）；路由无状态、P3 多网关分配记 open。 — rationale: 过早抽象更贵、单通道/单写者；dissent: caicai vs weiye 抽象时机分歧已消解；unanimity: majority；discussion: `dsc_18d48d030b07c536195df1#672`（结论 `#684`）。

- **DQ-0006 通知产品化**: **per-member 订阅 + 静默时段（紧急穿透）+ 与 Issue #15 共用配置面**，P3 交付。订阅 per-(member,team,channel)+event_types 白名单，默认=与自己相关+budget.alert 全员；静默本地 tz、非穿透入 digest 不丢、**穿透白名单仅 budget.alert + im.security_alert**、直接指派不穿透；`nudge_templates`→`notification_templates(team_id,channel,kind,template)`（模板/频控/权限三同源，模板=lead、订阅=成员自助）；群=广播/私聊=个人、禁 @all、配置写动作不进 IM；治理冻结：事件封闭枚举、心跳类永不进、穿透白名单硬编码。 — rationale: 防刷屏从枚举层杜绝、穿透是打扰特权；dissent: caicai 保留"P3 才产品化"（digest 与禁 @all 第一天即做，已采纳）；unanimity: majority；discussion: `dsc_18d48d030b07c536195df1#685`（结论 `#711`）。

## Current Frontier
（无。所有 DQ 已收敛。）

## Remaining Branches
（无）

## Outcome
- 全部 6 个 DQ 已达成共识，见 `docs/design/im-access-requirements.md`（需求分析文档）。
- grill 线程：`dsc_18d48d030b07c536195df1`（已关闭）。

## Fact Requests and Reports
（暂无）

## Related Artifacts
- grill discussion: `dsc_18d48d030b07c536195df1`
- 前置讨论（已完成）: `dsc_18d47012e374d37b195df1`
