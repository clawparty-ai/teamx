# 评审记录：t1-im-client（IM 侧与客户端面）

- 被评审对象：`design/003-t1-im-client.md`（author: weiye-mini，源台账仓 `teamx-ent` @ `ad77556`）
- 评审人：caishu（owner 代 weiye 做架构视角评审，reviewer≠author）
- 评审日期：2026-09-19
- 结论：**PASS（含 2 项必改）**

## 结论

结构清晰、边界明确、忠实于 owner 裁决（#6 #4101 / #8 #4078）。**通过**，但 2 项必改需随实现修正。

## 评审点

1. **边界声明** ✅ 明确只覆盖 IM 侧 + 客户端面，与 `t1-provider-external` / `t1-impl-test` 互引，无重复设计。
2. **忠实裁决** ✅ T5 拆两层（P0b 绑定层先行、P1a 通道层依赖 T4）；IM 单绑定选 (a)；B 协议不实现（留设计输入）。
3. **可执行性** ✅ P0a「零/极小新代码」——复用 #63 方案 A（im.bind code）与 #62 身份闸门，只做验收补强（一正例三负例 + 审计断言），不依赖云主机/T4，今天可做。
4. **机制化落地** ✅ 单连接硬规则（本地锁 + serve 侧注册租约双保险 + 拒启审计）；app_secret 用 OS 凭据存储、不进 workspace；重连六边界逐条。
5. **回答任务问句** ✅ 明确回答「P0 回归锚是否足够 = 足够」，并给出理由（IM 四条 E2E 覆盖委派全路径 + 拒绝侧单测 + 未知 kind 负例）。

## 问题（必改）

1. **与 `t1-impl-test` 状态冲突**：本文 §2 称二维服务白名单「与 weiye #4034 的 P0 为**同一 PR**（不另立）」＝**待做**；而 `t1-impl-test` §0/§4.4 称其「**已由 task-60 / PR #96 落地**（edb4997）」＝**已完成**。两份文档对同一事项状态不一致，必须统一（并核实远端 PR #96 是否真的已合并）。
2. **接口未定义**：§3.1 写 `im-gateway run --mode self`，但未说明 `--mode self` 是新参数还是新语义（现 CLI 为 `im-gateway run --cert-dir/--server/--team/...`）。须给出确切参数面，或改用现有调用形态。

## 建议（可采纳）

1. P1a 的 serve 侧注册租约 TTL/抢占语义，建议与 `t1-provider-external` §1「k8s Lease」口径对齐并交叉引用。
2. §5 测试表建议补一列「归属切片」，便于与 `t1-impl-test` 的 E2E 矩阵对齐去重。

## 风险

- 无阻塞风险。P0a 若暴露现有门禁缺口（枚举面/限速不足）已声明升级为真问题单独修，不塞回本切片 ✅。

## 后续

- 作者（weiye-mini）修正 2 项必改后，本文档维持 `approved`，修正项随实现落地并回填 `.meta.json` history。
- 与被评审文档的关系：`design/003-t1-im-client.meta.json` 的 `state` 由 `review` 推进为 `approved`，并在 `history` 追加本评审结论。
