# T1 设计：IM 侧与客户端面（#6 T5 拆层 / #8 插件面）

- author: weiye-mini（role-ef4e5f1a）
- date: 2026-09-18
- task: t1-im-client（high，owner 派发）
- scope: 本文仅覆盖「IM 侧（#6 T5 拆两层）+ 客户端/插件面（#8）」切片；provider/成本/合规归 t1-provider-external（xiaods）；总体实现切片与测试矩阵归 t1-impl-test（caicai）；架构主笔归 weiye（co-lead）。
- 裁决基线: #6 owner 裁决 dsc_18d644d8ab1de0272222bc#4101（6 决策点收敛 + 结构裁决 A/B + 执行口径）；#8 owner 裁决 dsc_18d65716b4b73bb22222bc#4078（租户 1:1 / App-only / gh-intake 独立 / P0 先行 / M1a 基线）。本文引用的讨论贡献：#6 侧 #3776/#3784（weiye-mini），#8 侧 #3998/#4011/#4038（weiye-mini，已被 owner 拍板纳为 T1 验收基线）。

## 0. 交付物总览（P0/P1 分列）

| 切片 | 优先级 | 内容 | 依赖 | 代码量预估 |
|---|---|---|---|---|
| P0-IM 绑定层产品化 | P0 | #6 T5 第一层：bind-code→im_bindings 验收补强 | 无（不依赖云主机/T4） | 零/极小（复用现成门禁） |
| P0-二维服务白名单 | P0 | #8 P0：2-D 白名单纯重构（与 weiye #4034 的 P0 同一 PR） | 无 | 小（重构 + 负例） |
| P1-IM 通道层 | P1 | #6 T5 第二层：im-gateway 随节点起 + 租约互斥 + secret 凭据存储 | T4 bootstrap | 中 |
| P1-gh-intake 客户端面 | P1 | #8：gh-intake 二进制落地时的插件/文档面 + 调用点清单 | P0 白名单 | 小（插件面基本不动） |

## 1. P0：IM 绑定层产品化（#6 T5 第一层）

**范围**：bind-code → im_bindings 的绑定流产品化验收。**实现策略：零/极小新代码**——绑定门禁已在 #63 方案 A（im.bind code）与 #62 身份闸门中落地，本切片只做验收补强与文档化。

**设计要点**：
1. 绑定流 = `bind-code 签发（member，一次性/TTL/限速）→ 用户飞书发 bot → im.bind（code 门禁）→ im_bindings 落行（最低角色）`，全程审计（`im.bind` 成功 + 拒绝路径 `im.rejected`）。
2. **不依赖云主机**：绑定层在现有 sidecar 模式（方案 1）即可全量验证——等价于把方案 1 的绑定面产品化，与 T4/provision 完全解耦（owner 裁决：T5 拆两层，绑定层先行）。
3. 与单绑定裁决（B 项选 a）一致：`im_bindings` PK=(platform, im_user_id, team_id) 保持不动；云主机作为该用户唯一常驻 IM 响应端（笔记本侧不绑或只读）。

**验收锚点**：
- 正例：bind-code 签发 → im.bind 成功 → 绑定落行 + 最低角色 + 审计事件断言。
- 负例 ×3：code 错误 / code 过期 / code 重用（单次性）——均拒 + `im.rejected` 审计。
- 回归锚：现有 im-binding-rpc / im-gateway-b E2E 逐字不变（绑定路径是既有安全路径，改任何一行都要过这组锚）。

**风险**：验收补强若暴露现有门禁缺口（如 code 枚举面/限速不足），升级为真问题单独修，不塞回本切片。

## 2. P0：#8 二维服务白名单纯重构

**与 weiye #4034 的 P0 为同一 PR**（不另立）。要点复述 + 对任务问句「P0 回归锚是否足够」的回答：

1. 2-D `service_kind → allowed_methods`，两维 fail-closed（未知 kind 拒；该 kind 未列方法拒）；im-gateway 段 = 现表原样迁移（im.* 逐字不变）。
2. **回归锚是否足够：足够**。im-gateway-b（委派读）/ im-cards（写+双确认）/ im-binding-rpc（绑定）/ im-fault-drill（三层拦截）四条 E2E 已覆盖 gateway 委派全路径，加白名单拒绝侧单测 + 「未知 service kind 拒」负例即为完整回归面；无需新写 E2E。
3. reviewer≠author；本 PR 必须先于 #8 一切 gh-intake 工作合入（P1 车道的前提）。

## 3. P1：IM 通道层（#6 T5 第二层，依赖 T4）

**范围**：im-gateway 随节点常驻（依赖 T4 bootstrap），把 BYO app 的通道能力落到节点。

**设计要点**：
1. **常驻**：systemd（Linux）/ launchd（macOS）unit，模板沿用 C7 worker unit（docs/35 §4.5）；`im-gateway run --mode self`。
2. **单连接硬规则**：一个飞书 app_id 恰绑定一个 im-gateway 实例——**第二实例启动即拒**（本地锁 + serve 侧注册租约双保险），拒启落 `im.rejected` 审计。这是飞书平台行为的结构性约束（多客户端事件随机分发且不可重放），必须机制化而非约定。
3. **app_secret 存储**：节点 OS 凭据存储（macOS keychain / Linux secret-service / systemd-credentials）；**不进 workspace 任何位置**（现有 fs 校验拒 .teamx/.git，secret 连 workspace 都不碰）；`im-gateway run` 只从凭据存储读，启动时引导写入。
4. **重连边界六条**（引我 #3784，owner 裁决沿用）：
   - 事件丢失不可补：IM = best-effort 通道，重要通知走 serve 推送/任务台账双轨；
   - 重投去重：`im_msg_id` 首击幂等（C4 已有）；
   - 半开连接：心跳探活 + serve presence 刷新；
   - 租约互斥：崩溃重启不得双实例抢同一 app（注册租约 TTL 保证旧实例过期后新实例接管）；
   - 凭据热轮换：app_ticket/secret 刷新不掉线，必要时优雅重拨；
   - 分区降级：断网时降级 serve-push-only + gateway 标记 degraded，恢复自动重注册。
5. **B 协议（中央路由）不实现**：留作设计输入（我 #3784 的四原语草案），与 owner 裁决「B 不做首期」一致。

**验收锚点**：双实例拒启（同 app_id 第二 gateway 启动失败 + 审计）；secret 不落 workspace 断言（fs 扫描负例）；重连六边界各一条断言（部分可在本地 mock，飞书侧真实验证留 T4 联调）。

## 4. P1：#8 客户端/插件面

**结论先行**：M1a/M1b 阶段**双插件工具面基本不动**——gh-intake 是独立 server 侧进程（公网面），M1 产物是 unclaimed taskx 任务，走现有 worker 认领流；插件与 gh-intake 之间没有 RPC 关系。

**插件面改动点清单**：
1. 无新增工具调用（M1 无写动作、无回链）；
2. docs/skill 同步：gh-intake 落地（M1b）时在 README/skill 增补 gh-intake 说明段（部署、secret 轮换、审计查询）；
3. **调用点全清单纪律**（#85 三处遗漏教训）：gh-intake / `teamx node bootstrap` 等**任何新 CLI 面**落地时，必须过「双插件 + docs + E2E」全调用点清单——本切片把这条列为验收项而非建议；
4. M1a 阶段唯一的插件面动作：无（连 skill 都不动，等 M1b 实际可用再写文档，避免文档先行失真）。

**对任务问句「M1a 清单的客户端/插件面校对」的回答**：M1a 清单 v2（weiye #4034 + 我 #4038 定稿）在客户端面**无未覆盖改动面**——10 条 E2E 负例全部落在 gh-intake 进程与 serve 车道上，插件面零触碰；唯一确认项是「插件侧不得试图代理 gh-intake 公网面」（它是独立进程，不该被任何插件包一层）。

## 5. 测试设计汇总（本切片）

| 层 | 测试 | 类型 |
|---|---|---|
| P0 白名单 | 未知 service kind 拒 + im.* 拒绝侧单测 | 单测（新增） |
| P0 白名单 | IM 四 E2E 回归锚逐字不变 | E2E（既有） |
| P0 绑定层 | bind-code 一正例三负例 + 审计断言 | E2E（新增段） |
| P1 通道层 | 双实例拒启 / 重连六边界 / secret 不落 workspace | E2E + 本地 mock（T4 联调补真机段） |
| 全切片 | 调用点全清单（新 CLI 面过双插件+docs+E2E） | 流程验收 |
| M1a | 10 条负例（weiye #4034 + 我 #4038 定稿，不在此重复） | E2E（t1-impl-test 切片主责） |

## 6. 排序与依赖

```text
P0a 二维白名单重构（#8，weiye #4034 P0）
P0b 绑定层验收补强（#6 T5-1）        ← 与 P0a 并行（完全不同面）
P1  通道层（#6 T5-2）                 ← 依赖 T4 bootstrap + 凭据存储
P1  gh-intake 车道（#8 M1a）          ← 依赖 P0a 白名单
M1b 映射+建任务                       ← 依赖 P1 车道 + t1-impl-test 测试矩阵
```

- reviewer≠author：P0a 建议 reviewer=caicai/xiaods；P0b=weiye；P1 通道层=weiye（架构提出者）；gh-intake 车道=weiye+caicai 双审。
- 与 t1-impl-test（caicai）的边界：总体测试矩阵/M1a E2E 主责在彼；本切片只负责 IM 侧与客户端面的测试设计，M1a 10 条负例不在此重复。

## 7. 风险与开放项

1. 绑定层验收补强可能暴露现成门禁的真缺口 → 升级单独修，不塞本切片；
2. 通道层常驻在无 systemd 容器场景需替代方案 → T4 时与 provider 切片对齐定；
3. gh-intake 公网面加固清单归 t1-impl-test（xiaods #3986/3987 已给主体：HMAC fail-closed / body 上限 / 限速 / 错误不回内部细节 / 审计带实例标识 / secret 轮换）——本文不重复，仅引用；
4. `--provision-node` 参数与异步 job 归 t1-impl-test 总切片（owner 裁决 A 项），本文仅引用其「默认成本为零」结论作为通道层排期前提。
