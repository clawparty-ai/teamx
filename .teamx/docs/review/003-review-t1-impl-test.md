# 评审记录：t1-impl-test（实现/测试总装）

- 被评审对象：`design/005-t1-impl-test.md`（author: caicai，源台账仓 `teamx-ent` @ `ad77556`）
- 评审人：caishu（owner 代 weiye 做架构视角评审，reviewer≠author）
- 评审日期：2026-09-19
- 结论：**PASS（含 2 项必改）**

## 结论

总装结构清晰、复用得当、负例可证伪。**通过**，但 2 项必改需修正。

## 评审点

1. **总装清晰** ✅ bootstrap / 幂等 / destroy + 吊销演练 / gh-intake 车道 / M1a 矩阵 / E2E 接线，P0/P1 分列，交付物总览表可用。
2. **复用现成、不造新语义** ✅ 一次性 token 抄 `im_bind_codes`；幂等抄 C4（部分唯一索引 + `INSERT OR IGNORE` + 冲突回读）；worker 常驻按 C7（docs/35 §4.5）；白名单引用 PR #96 不重复设计。
3. **负例可证伪** ✅ token 四负例（重放/过期/换错设备/已消费）；destroy 两次第二次无操作成功；**吊销演练**（destroy 后旧证书立刻被拒）；三层故障演练（L1 provider 注入 / L2 job 崩溃恢复 / L3 状态迁移落台账）；M1a 10 条负例（零 GitHub 本地可跑）。
4. **忠实裁决** ✅ token 唯一路径（禁 cloud-init 直塞）；版本兼容门 fail-fast；`--opencode-url` 显式透传（#70 教训）；熔断前移 T3。
5. **纪律条目** ✅ 新 CLI 面过「双插件 + docs/skill + E2E」全调用点清单（#85 教训列为验收项）。

## 问题（必改）

1. **与 `t1-im-client` 状态冲突**：本文 §0/§4.4 称二维白名单「**已由 task-60 / PR #96 落地**（edb4997）」；而 `t1-im-client` §2 称其「与 weiye #4034 的 P0 为**同一 PR**（不另立）」＝**待做**。两份文档对同一事项状态不一致，必须统一（并核实远端 PR #96 是否已合并）。
2. **切片编号与里程碑未对齐**：本文用 P0/P1，而任务/裁决用 T2/T3/T4/T5。建议补一张「P0/P1 ↔ T2–T5」映射表，便于与另两份对齐排期与验收。

## 建议（可采纳）

1. §3.1 destroy 清单建议直接落成 **E2E 断言文件路径**（可执行清单，而非散文），使「可验证」落到代码。
2. §5 建议补「条件位 skip 的判定输入」清单（哪些前置缺失时 skip、退出码语义）。

## 风险

- 无阻塞风险。

## 后续

- 作者（caicai）修正 2 项必改后，本文档维持 `approved`，修正项随实现落地并回填 `.meta.json` history。
- 与被评审文档的关系：`design/005-t1-impl-test.meta.json` 的 `state` 由 `review` 推进为 `approved`，并在 `history` 追加本评审结论。
