# 下一迭代候选盘点：goal 能力现状矩阵 + 缺口 + 候选任务清单

> 任务 task-15 产出（caicai，2026-09-16）。基线：enterprise @ `fc8da32`（0.2.4）。
> 供 owner 在合并队列清空后选定下一迭代方向。
> 状态图例：✅ 已合 / 🟡 PR待合 / ⬜ 未启动。

## 一、goal 能力现状矩阵

| goal 能力 | 现状 | 证据 |
|---|---|---|
| mTLS 网络模式（serve/console/tunnel） | ✅ 可用 | serve/console/tunnel 全链路 mTLS；smart-HTTP git 512MiB（#18） |
| Member Console（Discussions/Documents/Events） | ✅ 可用 | master-detail、mdRender 白名单、goal 回退显示 |
| Console「File」文件浏览+预览 | ✅ 已合（grill 6 题 unanimous 收敛） | 15f9fb4（panel+折叠/敏感名/上限/mdRender 接线）；grill 已 close |
| File 隐私收紧（全局隐藏+PDF 内联） | 🟡 待合 | #68（`is_private()` 全局规则 + PDF `inline`）；依赖先合 #67 |
| File `.git/config` 折叠+双策略测试 | 🟡 待合 | #67（caicai review PASS） |
| 抓包/分析/回放（capture/analysis/replay） | ✅ 可用 | capture-test/analysis-test/replay-test；E2E 脚本齐 |
| 团队文档与任务契约（teamfile/docs/taskx/grill） | ✅ 可用 | TEAM.md/_spec、task-15 本盘点亦属此能力 |
| IM P1（绑定/通知/只读查询/网关A） | ✅ 已合 | #43/#44/#50/#51 + 锚点验收 |
| IM P2 网关B+白名单+执行体+卡片 | ✅/🟡 大部已合 | #57/#58/#59/#60 已合；#54 白名单已合 |
| IM 故障演练+L2 越权修复 | 🟡 待合 | #62（caicai review PASS，先于 #63） |
| IM budget.alert 直推 | 🟡 待合 | #61（DQ-0006 全员默认+穿透） |
| IM 持久连接网关（ws-gateway） | ✅ 已合 | #63 已合（GitHub 上若仍 open 请销账） |
| 成员直连 P0/P0.5/P1/P2 | ✅ 已合 | #46/#47/#48/#49 |
| 插件双端对齐（65 工具） | 🟡 待合 | #71（caicai 首轮 CR→作者修 4f1fd0f→复核 PASS）；argv 加固 #56 已合；lead 唤醒 #52 已合 |
| worker 运维（4199 footgun 修复） | 🟡 待合 | #70（caicai 实现；weiye-mini review 首轮 CR→作者修 2d206c1→复核 PASS，324P/0F） |
| nudge 可定制模板 | ✅ 已合 | #53（set/get/reset） |
| 质量门禁（clippy 基线） | 🟡 待合 | #72（deny 根修 + 82→0；caicai review PASS） |
| CI gate | ⬜ 未生效 | `.github/` 被忽略；clippy/test 靠人工报告 |

## 二、缺口（按严重度）

### G1 本轮必须收口（否则前功尽弃）
1. **合并队列执行**：#72→#71→#70→#67→#68（#68 需rebase）→#62→#61；关闭 #63/#64/#66（已被取代）。owner 手工动作，无 agent 可代劳。
2. **CI 门禁生效**：`.github/` 当前被忽略，82 warnings 即如此腐烂。#72 合并时同步打开 CI（至少 clippy `-D warnings` + cargo test），否则基线会再次腐烂。

### G2 写动作上线前必须收口（安全债，reviews 已挂账）
3. **`im.query` 网关证书专用化**：member-cert 路径仍可用任意已绑定身份读其自我范围（#50/#58/#62 三份 review 一致挂账，只读、team 可见面内）。
4. **卡片 request 拒绝审计对称**：未知动作/非 lead/未绑定的 request 拒绝未落 `im.rejected`（pr-60 建议2）。
5. **卡片首击幂等**：`im_msg_id` 被 request 忽略，双击签发双 token（pr-60 建议1）。

### G3 功能完整性缺口
6. **tunnel 真机 E2E**：expose→forward 回环仅录制/单测，#71 声明两台节点间实测一次（非本 PR）。
7. **File 后续片③④**：前端 File 菜单抽取 + repo 发现/排序（收敛口径③④；①②已由 owner 实现替代，#64/#66 已关）。
8. **worker 部署 rollout**：#70 合并后，systemd units 与云节点手册（docs/35）须把 `--opencode-url`/TEAMX_OPENCODE_URL 补上，否则存量单机 worker 启动即报错。
9. **parity 测试对称性**：list/download 接线无 parity 锁死（pr-64 建议1，#67 延续）。

### G4 流程债
10. **discussion 配额与 grill 豁免产品化**：6 题全员 grill 结构性配额不足（历史教训 seq 690），leader @ 解锁流程产品化或共识会话豁免。
11. **goal 治理已立规**（#69，AGENTS.md 硬约束）：AI 不得开关目标；本次误关 closed 的返工不再犯——属已闭环，列此备忘。

## 三、候选任务清单（供 owner 勾选）

| # | 候选任务 | 依据 | 建议优先级 |
|---|---|---|---|
| C1 | 执行合并队列（#72/#71/#70/#67/#68/#62/#61，销账 #63） | G1-1 | P0（owner） |
| C2 | 打开 CI 门禁（clippy `-D warnings` + cargo test） | G1-2 | P0 |
| C3 | `im.*` 网关证书专用化（写动作上线门禁） | G2-3 | P1 |
| C4 | 卡片 request 拒绝审计 + 首击幂等 | G2-4/5 | P1 |
| C5 | tunnel 双节点真机 E2E | G3-6 | P1 |
| C6 | File 前端抽取③ + repo 发现④ | G3-7 | P1 |
| C7 | worker 部署文档 rollout（units/手册补 URL） | G3-8 | P2 |
| C8 | parity 测试补 list/download | G3-9 | P2 |
| C9 | discussion 配额/grill 豁免产品化 | G4-10 | P2 |

> 注：本盘点只列“候选”，不分派；owner 选定方向后由 lead 拆解为 taskx 任务。
> 并行建议：C1/C2 先行（owner）；C3/C4 可与 C5/C6 并行（不同成员）。
