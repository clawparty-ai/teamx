# 评审记录：t1-provider-external（provider / 成本 / 合规 / 外部 + Console 开通页）

- 被评审对象：`design/004-t1-provider-external.md`（author: xiaods，源台账仓 `teamx-ent` @ `ad77556`）
- 评审人：caishu（owner 代 weiye 做架构视角评审，reviewer≠author）
- 评审日期：2026-09-19
- 结论：**CHANGES-REQUESTED（3 项必改 + 3 项待 owner 裁决）**

## 结论

覆盖面最完整的一份（provider / 成本 / 合规 / 开通页 / 外部对标），但存在 1 处事实性错误、1 处未获批新依赖、1 处**重大拓扑歧义**。**退回修正**（`.meta.json` state → `draft`）。

## 评审点

1. **覆盖完整** ✅ provider 接口 + k8e 首实现、Console 开通页（R1）、成本闸门数据面、P1 AWS CLI / microvm 镜像 + 按需池 / 合规结算、外部对标。
2. **需求记录** ✅ R1–R3（2026-09-18 owner 侧新增）显式落文档，便于追溯。
3. **架构输入** ✅ K8E 控制面（etcd：Pods/CRDs/Secrets/Leases）与业务数据面（rqlite）分离显式声明，边界（台账 events 仍是审计唯一真相源）写清。
4. **忠实裁决** ✅ BYO 先行、平台统一账号默认关、熔断前移 T3、destroy 清单、密钥铁律、invite/provision 异步。
5. **诚实自陈** ✅ §7-2 主动标注「OAuth client secret 由 serve 持有」是本设计唯一例外于「serve 不持 secret」。

## 问题（必改）

1. **事实性错误**：头部写 t1-im-client「**已评审 PASS**」——**实际当时未评审**（weiye 离线，评审在本次才进行）。须改为「待评审 / 评审中」，避免下游误信。
2. **新依赖未获批**：§1 引入 **rqlite**（文档自注「k8e 仓内暂无 rqlite 用法，属本 T1 引入的新依赖」）。须将其显式列为**待 owner 批准项**，并补充论证：为何不复用现有 SQLite（`teamx.db`）或台账 events。
3. **拓扑歧义（重大）**：§2 步骤④ 写「装 k8e-sandbox + **teamx server**」、R1 写「拉起服务器装 k8e-sandbox + teamx server」。若每个节点都跑一个 `teamx serve`，则与现有模型（一队一个 serve `120.27.222.33` + node 为 member）冲突。须明确：节点上跑的是 **serve** 还是 **member node（worker/agent）**；若确为多 serve，须说明台账/证书/租户边界与数据同步方式。

## 建议（可采纳）

1. §1 K8E 事实（k3s 派生结构、`etcdsnapshot`/`controllers`）建议附 k8e 仓链接 + commit，避免不可核验断言。
2. **公网面边界**：§2 的 `/onboard` + `POST /api/onboard/*` 是新公网入口，但 user-console 默认绑 `127.0.0.1`。须说明外部 GitHub 用户如何到达（部署位置/暴露方式），并与 #8「gh-intake 是唯一公网面」的结论对齐。
3. §8 测试表建议补「责任切片」列，便于与另两份去重。

## 风险 / 待 owner 裁决

1. **rqlite 新依赖** —— 批准 / 否决（若批准，需补运维与备份口径）。
2. **GitHub OAuth client secret 由 serve 持有** —— 批准 / 否决；若批准，须限定为「短期 code 兑换、不落盘、可轮换」并写入安全约束。
3. **公网面边界** —— 开通页是否引入第二个公网入口（与 #8 结论对齐）。

## 后续

- 作者（xiaods）按 3 项必改修正；owner 对 3 项待裁决项给出结论后**复审**。
- 与被评审文档的关系：`design/004-t1-provider-external.meta.json` 的 `state` 由 `review` 退回 `draft`，并在 `history` 追加本评审结论。
