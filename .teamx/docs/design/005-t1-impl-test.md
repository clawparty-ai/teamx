# T1 设计：实现/测试总装（#6 bootstrap·幂等·destroy / #8 gh-intake 车道·M1a 矩阵）

- author: caicai（contributor，实现/测试视角）
- date: 2026-09-19
- task: t1-impl-test（high，owner 派发）
- scope: 本文仅覆盖「总体实现切片 + 测试矩阵」总装；IM 侧与客户端面归 t1-im-client（weiye-mini，eb68543）；provider/成本/合规/开通页归 t1-provider-external（xiaods，a48bb9a）；架构主笔归 weiye（co-lead）。与前两份是引用关系，不重复表结构与裁决原文。
- 裁决基线：#6 owner 裁决 dsc_18d644d8ab1de0272222bc#4101；#8 owner 裁决 dsc_18d65716b4b73bb22222bc#4078；M1a 清单 v2 = weiye #4034 + weiye-mini #4038 定稿。本文讨论输入：#6 侧 #3812（caicai 实现/测试视角）。

## 0. 交付物总览（P0/P1 分列）

| 切片 | 优先级 | 内容 | 依赖 | 备注 |
|---|---|---|---|---|
| P0-bootstrap 总装 | P0 | 一次性 token（抄 im_bind_codes）+ worker 常驻（C7）+ 版本门 + opencode-url 透传 | 无（本地可验） | 今天可进 CI |
| P0-幂等总装 | P0 | provision_jobs 幂等（C4 复用）+ 并发双 invite 单台 | 无 | 单测为主 |
| P0-二维白名单实现 | P0 | 已由 task-60 / PR #96 落地（weiye-mini，edb4997），本文只列回归锚 | t1-im-client P0 | 不重复设计 |
| P0-成本闸门接线 | P0 | budget.alert 熔断提至 T3（引用 t1-provider-external §3） | 无 | 同批落地 |
| P1-gh-intake 车道 | P1 | 独立二进制 + serve 第二车道 + gh_deliveries（M1a v2） | P0 白名单 | 纯加法 |
| P1-E2E 矩阵 | P1 | tests/*.sh 接线 + run-all 条件位 + 10 条负例 | 各切片 | mock 先行 |

## 1. P0：bootstrap 总装（#6 T2/T4）

**设计（抄现成，不造新语义）：**
1. 一次性 token 直接抄 `im_bind_codes`：CSPRNG ≥128bit、只存 hash、单次、TTL ≤15min、绑定 `(team_id, user_id, device_member_id)` 三元组、校验失败不消费（防占位 DoS，#62 结论复用）。
2. worker 常驻按 C7 文档验收：systemd unit 模板沿用 docs/35 §4.5；`kill -9 worker` 后 N 秒内回来且 `worker-done.json` 游标连续；整机重启后 serve + worker +（可选）gateway 全回。
3. 版本兼容门：bootstrap 第一步校验节点 teamx 二进制与 serve semver 兼容，不兼容 fail-fast 报明确错。
4. opencode-url 显式透传：节点内 worker 的 `--opencode-url` 由 bootstrap 写入本节点端口（#70 无默认值教训）；E2E 断言 worker 横幅 URL 与本节点 serve 端口一致，锁死跨 workspace attach footgun。

**验收：** token 四负例（重放/过期/换错设备/已消费）全部留审计；重启自愈；版本不兼容 fail-fast；opencode-url 透传断言。

## 2. P0：幂等总装（#6 T2/T3）

直接复用 C4 已验证模式（task-21，`im_card_tokens`）：`provision_jobs` 表对 idem_key（`(team_id, member_id)`）建部分唯一索引 + `INSERT OR IGNORE` + 冲突回读胜者。覆盖并发双 invite（先查后建会双开，多一行代码少一个竞态）。单测：重复 invite 单台、并发双 invite 单台、非法状态迁移被拒、user-member 一对多断言。

## 3. P0/P1：destroy 清单 + 吊销演练 + 三层故障演练

1. destroy 干净 = 可验证清单（T3 逐项断言）：实例 terminated、快照按策略、公网 IP 释放、DNS 清除、secret 清理、member 证书吊销、invitation revoke。负例：destroy 两次第二次为无操作成功（非报错）、destroy 不存在报明确错。
2. 吊销演练（#62 式）：destroy 后旧证书打任意 RPC 必须立刻被拒，一条 E2E 证明可吊销不是文档。卸载 App 与 destroy 合并为同一清单（installation token 与已 clone 内容一并删），由本切片主责验收。
3. 三层故障演练：L1 provider 故障注入（限流/超时/建一半失败 → job 可重试且不 double-provision）；L2 job 崩溃恢复（create 与写库之间 kill → 重放收敛同一台）；L3 每次状态迁移落台账。

## 4. P1：gh-intake 车道 + M1a 矩阵（#8）

**实现（引用收敛结论，只写增量）：** 表/状态机/actor/白名单以 M1a v2 为准（PK=(installation_id,delivery_id)、state received|processing|done|failed、actor=`system:gh-intake`、白名单只 `["gh.intake"]`），本文不重复。实现/测试视角增量四条：
1. intake 保持无状态：验签 → body 上限 → best-effort 本地去重 → mTLS 转发；serve 不可用策略 = 有界内存队列（丢最旧 + 落 `gh.dropped`）+ 队满 5xx（显式失败优于静默丢，GitHub 重投靠人工）。
2. serve 为唯一写入者：`gh_deliveries` 权威在 serve，跃迁唯一执行者 = serve；intake 不写 DB，无双写面。
3. 副作用层幂等断言：同 delivery 投两次 → taskx 任务数恰为 1 且审计数恰为 1（行级幂等 ≠ 效果级幂等，C4 教训）。
4. P0/P1 拆分合理：P0（PR #96，edb4997）纯重构、im.* 逐字不变，回归锚 = IM 四 E2E（gateway-b/cards/binding-rpc/fault-drill）+ 未知 kind 拒；P1 车道纯加法。回归信号不混杂，同意拆两个 PR。

**10 条 E2E 负例（M1a v2，不重复，已定稿）：** 验签失败/重投零重复/未知事件 gh.ignored/body 413/限速 429/跨服务三条（gh-intake 调 im.approve 拒、调 team.status 拒、im-gateway 调 gh.intake 拒）/未知 kind 拒/intake cert 调 member 方法拒 + ⑨效果级幂等 + ⑩serve 不可用分支（5xx + gh.dropped）。全部本地可跑（零 GitHub）。

## 5. E2E 接线（tests/*.sh）

照现有脚本惯例（temp DB/HOME、mTLS、trap 清理）：新增 `tests/node-bootstrap-test.sh`（T2/T4 本地部分）与 gh-intake E2E（M1a 10 条）；mock provider + 真实状态机，k8e 到位前先进 CI。run-all 接线为条件位（缺前置则 skip 退出 0）；加超时与重试抗 flaky；失败输出可诊断信息；`bash -n` 必过。调用点清单纪律：任何新 CLI 面（`teamx node *`、`teamx gh-intake`）落地即过双插件 + docs/skill + E2E 全清单（#85 教训列为验收项）。

## 6. 排序与 reviewer

```text
P0 bootstrap 本地部分 + 幂等单测 ── 今天可进（无云）
P0 白名单（PR #96）─────────────── 已落地，待 review 合并
P0 成本熔断接线 ────────────────── 与首台开通同批（T3）
P1 gh-intake 车道 + M1a E2E ────── 依赖 P0 白名单
P1 开通页/镜像/合规 ────────────── 见 t1-provider-external §9
```

- reviewer≠author：本切片建议 reviewer = weiye（架构）+ xiaods（外部/provider 视角）；P0 白名单 reviewer = caicai/xiaods（与 t1-im-client §6 一致）。
- 与另两份的边界：绑定层/通道层/插件面在 t1-im-client；provider/成本/开通页在 t1-provider-external；本文只总装 bootstrap·幂等·destroy·gh-intake 车道·E2E 矩阵。
