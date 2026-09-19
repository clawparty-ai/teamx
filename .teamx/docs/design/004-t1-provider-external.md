# T1 设计：provider 落实 + 成本/合规/外部视角 + Console 开通页（#6 云主机 / #8 部署链路）

- author: xiaods（contributor）
- date: 2026-09-18
- task: t1-provider-external（high，owner 派发）
- scope: 本文仅覆盖「provider/成本/合规/外部视角 + Console 开通页」切片；IM 侧与客户端面归 t1-im-client（weiye-mini，eb68543，已评审 PASS）；总体实现切片与测试矩阵归 t1-impl-test（caicai）；架构主笔归 weiye（co-lead）。
- 裁决基线：#6 owner 裁决 dsc_18d644d8ab1de0272222bc#4101（6 决策点收敛 + 结构裁决 A/B + 执行口径）；#8 owner 裁决 dsc_18d65716b4b73bb22222bc#4078（租户 1:1 / App-only / gh-intake 独立 / P0 先行 / M1a 基线）。本文引用的讨论贡献：#6 侧 #3766/#3770/#3774/#3782（xiaods）、#3802/#3803（weiye）、#3812（caicai）；#8 侧 #3998/#4011/#4038。
- 用户需求输入（2026-09-18，owner 侧新增，按默认设计落实）：R1 Console 开通页（GitHub 凭证登录验证 → 安装 Teamx APP → 拉起服务器装 k8e-sandbox + teamx server → 初始化 team_id/member_id）；R2 每用户一台 microvm（预装 opencode 即开即用的远程代理，计费与资源另算）；R3 其余按默认设计。

## 0. 交付物总览（P0/P1 分列）

| 切片 | 优先级 | 内容 | 依赖 | 代码量预估 |
|---|---|---|---|---|
| P0-Provider 接口 + k8e 首实现 | P0 | `CloudProvider` 多后端接口；k8e-sandbox 首个实现；幂等（C4 复用）；destroy 干净清单 | 无（本地 fixture 可验） | 中 |
| P0-Console 开通页 | P0 | user-console 新增 `/onboard`：GitHub 登录 → 装 APP → 开通 → bootstrap 状态机可视 | P0-Provider（mock 可先行） | 中 |
| P0-成本闸门数据面 | P0 | 配额/TTL/回收字段 + `budget.alert` T3 接线 + 计量落台账 | 无 | 小 |
| P1-AWS CLI 第二实现 | P1 | 同一接口第二后端；region 白名单 enforcement | P0-Provider | 小 |
| P1-microvm 镜像 + 按需池 | P1 | 预装 opencode 的 microvm 镜像；ephemeral warm pool；启动预算 SLO | P0-Provider | 中 |
| P1-合规与结算 | P1 | at-rest 加密断言；读取全审计；计量→结算（计费另算，本文只定计量口径） | P0 成本面 | 小 |

## 1. P0：Provider 接口 + k8e-sandbox 首实现（#6 T3）

**设计要点**：

1. **接口先行、单实现落地**：`CloudProvider { create / status / destroy / list }` 首批即按多后端签名设计（region/image/size/ttl 参数位预留），但只实现 `k8e-sandbox`（单二进制 <100MB，gVisor 默认/Kata/Firecracker 可插拔，E2B SDK 兼容，warm pool 快启——能力以 k8e.sh 仓 README 为准，不在本设计中重述）。
2. **幂等键 `(team_id, member_id)`**：`provision_jobs` 表对 idem_key 建部分唯一索引 + `INSERT OR IGNORE` + 冲突回读胜者——直接复用 C4（`im_card_tokens`）已验证模式（caicai #3812），并发双 invite 只出一台。
3. **destroy 干净 = 可验证清单**（T3 验收逐项断言）：实例 terminated、快照按策略、公网 IP 释放、DNS 清除、secret 清理、member 证书吊销、invitation revoke；外加“吊销演练”：destroy 后旧证书打任意 RPC 必须立刻被拒（#62 式 E2E）。
4. **状态机**：`requested → provisioning → bootstrapping → ready → degraded → stopped → terminated`，每次迁移落台账；非法迁移拒绝（T2 单测覆盖）。
5. **invite/provision 解耦**（裁决 A）：`--provision-node` 只入队异步 job；invite 成功不以 job 成功为前提；console 开通页轮询 job 状态而非阻塞等。

**验收锚点**：重复 invite 单台；destroy 两次第二次为无操作成功；destroy 不存在报明确错；云 API 限流注入可重试且不 double-provision；kill 恢复（create 与写库之间 kill → 重放收敛同一台）。

**K8E 架构输入（控制面 / 业务数据面分离）**：

```text
K8E
 ├─ kube-apiserver ───────────→ etcd
 │    Pods / CRDs / Secrets / Leases
 │
 └─ 业务服务 ────HTTP SQL─────→ rqlite
      任务历史 / 计费记录 / 业务元数据
```

1. **控制面走 etcd**：Pods/CRDs/Secrets/Leases 等 k8s 状态由 kube-apiserver 落 etcd（k8e 系 k3s 派生结构，仓内 etcdsnapshot/controllers 可证）。网关租约互斥（t1-im-client §3 的 serve 侧注册租约）优先用 k8s Lease 语义实现，不另造租约表。
2. **业务数据走 rqlite**：任务历史、计费记录、业务元数据经 HTTP SQL 进 rqlite（新增主张；k8e 仓内暂无 rqlite 用法，属本 T1 引入的新依赖）。理由：业务数据不污染 k8s 控制面；rqlite 提供 SQLite 兼容 + Raft 复制的查询面，serve 保持 dumb（只写事件、不算钱）口径不变。
3. **边界**：teamx 台账 events 表仍为审计唯一真相源（安全/拒绝/绑定事件）；rqlite 只存可查询业务数据（§6 `cost_events`、任务历史、节点元数据副本）。两者分工在实现 PR 描述中显式声明，审计口径不漂移。

## 2. P0：Console 开通页（R1，用户需求）

**落点**：user-console（成员 console）新增公开 onboarding 面，与现有 `console.rs` 路由体系同构（`/`、`/login`、`/api/me`、`/api/cloud` 已有；本页为其前端延伸，不另起服务）。

**流程（5 步，状态机可视）**：

```text
① GitHub 登录（OAuth，仅身份环节，App-only 主流程之外）
  → ② 安装 Teamx APP（跳 GitHub App 安装页，回跳带 installation_id）
  → ③ 开通（选 BYO cloud / 平台账号（默认关）；region 白名单校验；进 provision_jobs 异步队列）
  → ④ Bootstrap（cloud-init 只带一次性 token → 节点 `teamx node bootstrap --token` 换 letter → 装 k8e-sandbox + teamx server → 初始化 team_id/member_id → 拉 teamx-ent 仓 → 起 worker）
  → ⑤ 就绪（presence 上线；可选 im-gateway；console 展示 ready + ssh/ tunnel 入口）
```

**设计约束**：

1. **OAuth 只作身份环节**（#4078：OAuth 不作里程碑第一步；登录通零团队能力）。token 短期、只存 session，不进 DB；`state/nonce` 防 CSRF/重放。
2. **App 安装即租户绑定**：回跳的 `installation_id ↔ team` 1:1 映射校验（未知即拒+审计），复用 #8 serve 侧映射口径，不另造第二套。
3. **密钥铁律**（weiye #3803 + #4101-5）：cloud-init 只带一次性 token（单次、TTL ≤15min、绑定 `(team_id, user_id, device_member_id)` 三元组，校验失败不消费）；letter 私钥落盘 0600；app_secret 只落节点 OS 凭据存储；serve/控制台不持有 node 私钥与 app_secret 明文。
4. **版本兼容门**（caicai #3812）：bootstrap 第一步校验节点 teamx 二进制与 serve semver 兼容，不兼容 fail-fast；`--opencode-url` 由 bootstrap 显式写入本节点端口（#70 无默认值教训，E2E 锁死）。
5. **新 CLI 面过全调用点清单**（#85 教训，t1-im-client §4.3 同款）：`teamx node {list,status,ssh,stop,start,destroy,bootstrap}` 落地即过“双插件 + docs/skill + E2E”清单，本页为验收项。

**验收锚点**：mock provider 下开通页 5 步全绿（含 token 四负例：重放/过期/换错设备/已消费全部留审计）；node 重启自愈；destroy 后旧证书立刻不可用；版本不兼容 fail-fast。

## 3. P0：成本闸门数据面（R2 的硬约束部分）

**默认成本模型**（#4101-4 + xiaods #3782，R3 按默认）：默认 S 规格；TTL 默认 30d 可续；CPU<5% 持续 30min 自动 stop；每 user 常驻上限 1 台（超限走审批）；欠费/到期自动 terminate（快照保留 7d 后删）。

**设计要点**：

1. **三级拓扑**（#3782）：① 本地 device 零成本默认路径（邀请即引导，先交付）；② 按需 ephemeral（warm pool，用完即毁）；③ 常驻 node 仅 7x24 重度用户。R2 的“每用户 microvm”默认落在②/③之间：**默认 stopped（按任务唤醒），常驻需显式 opt-in**——与“默认成本为零”裁决一致。
2. **`budget.alert` 熔断前移 T3**（#4101 执行口径）：超限自动 stop 与首台开通同批落地；serve 保持 dumb，只收计量事件不算钱；`cost_snapshot` 同时落台账事件（可审计可对账，caicai #3812）。
3. **计费与资源另算**（R2）：本文只定**计量口径**（CPU/内存/磁盘增量/egress/时长事件），结算、对账单、支付流程明确为后续项，不在本 T1 实现。

## 4. P1：AWS CLI / microvm 镜像 / 合规结算

1. **AWS CLI 第二后端**：同一 `CloudProvider` 接口；region 白名单在开通时强制校验（企业合规，#3766）；BYO cloud 先行，平台统一账号默认关闭（#4101-2）。
2. **microvm 镜像（R2）**：预装 opencode + teamx worker 的基础镜像 + 共享快照 registry/构建缓存降冷启动；ephemeral 启动预算 SLO：bootstrap 端到就绪 ≤X 分钟断言（X 由 T3 实测定，caicai #3812 建议采纳）。
3. **合规**：workspace at-rest 加密；SSH/secret 读取全审计；tenant 隔离复用 k8e per-session 边界（gVisor 默认）+ 单网关审计出口；`node_secrets` 只存引用。
4. **卸载即同一张清单**（weiye #4001-4，#8 侧）：卸载 APP = 吊销证书 + destroy + 删 installation token 与已 clone 内容——与 §1 destroy 清单合并为同一验收口径，由 t1-impl-test 主责，本文引用。

## 5. 外部视角（#8 对标，R3 默认）

orbi.build 对标结论：本 T1 的“Console 开通页 5 步”即对标面的可交付形态（一键部署 = 开通页 + token bootstrap + destroy 清单三件套）；部署链路**复用 #6**（token bootstrap、destroy 清单、region、预算），不另造第二套（#4078 T1 硬要求）。GitHub 侧差异点（installation 模型、delivery 语义）归 gh-intake 车道（t1-impl-test），本文不重复。

## 6. 接口与数据（采纳 #3753 §10 + 增量）

```text
nodes(id, team_id, user_id, member_id, provider, region, instance_id,
      ip, state, im_enabled, created_at, ready_at, terminated_at, cost_snapshot,
      compliance_tags)                                   --增量：region/合规标签（#3766）
provision_jobs(id, node_id, state, attempts, last_error, idem_key, created_at)
node_secrets(node_id, kind, ref, created_at)             --只存引用，不存明文
cost_events(ts, node_id, meter, value, unit)             --增量：计量事件（落台账；结算另算）
```

```text
teamx team invite <role> --user-name <人> --provision-node [--provider k8e|aws --region .. --size .. --ttl 30d] [--im feishu]
teamx node list | status <id> | ssh <id> | stop <id> | start <id> | destroy <id>
teamx node bootstrap --token <t> [--im feishu]
```

Console 面（user-console 新增）：`GET /onboard`（5 步向导页）+ `POST /api/onboard/{login,install-callback,provision,bootstrap-token,status}`；状态订阅复用 `/api/stream`（SSE 台账流，不新开通道）。

## 7. 风险与开放项

1. 平台统一账号的熔断 E2E 未落地前只走 BYO（#4101-2），开通页默认隐藏平台账号选项。
2. GitHub OAuth 应用凭据（client secret）由 serve 持有——它是本设计唯一例外于“serve 不持 secret”的点：限定短期兑换 code 用途，不落盘，轮换流程与 App webhook secret 轮换同文档（#8 T1 引用）。
3. 无 systemd 容器场景的常驻方案（worker/gateway 自启替代）→ T4 与 provider 切片对齐定（t1-im-client §7 同款开放项）。
4. ephemeral warm pool 的容量与驱逐策略 → P1 实测定，不阻塞 P0。

## 8. 测试设计汇总

| 层 | 测试 | 类型 |
|---|---|---|
| T2 状态机 | 重复 invite 单台 / 非法迁移拒 / user-member 一对多断言 | 单测 |
| T3 开通回收 | 幂等开通 / destroy 干净清单 / 吊销演练 / 限流注入重试 | E2E（mock provider 先行，k8e 实机后补） |
| T4 bootstrap | token 四负例+审计 / 重启自愈 / 版本门 / opencode-url 透传断言 | E2E |
| 开通页 | 5 步端到端（mock provider + fixture installation）/ region 非法拒 | E2E |
| 成本 | 超限 auto-stop / TTL terminate / cost 事件落台账 | E2E（T3 同批） |
| 调用点清单 | 新 node CLI 面过双插件+docs/skill | 流程验收 |

## 9. 排序与依赖

```text
P0 provider 接口 + k8e 实现（T2 本地状态机先行，今天可进）
P0 成本闸门数据面 ───────────── 与上并行（字段 + budget.alert 接线）
P0 Console 开通页 ───────────── 依赖 provider job 状态机（mock 可先行）
P1 AWS CLI ─────────────────── 依赖 P0 接口冻结
P1 microvm 镜像 + 按需池 ────── 依赖 P0 k8e + T4 bootstrap
P1 合规结算 ────────────────── 依赖 P0 计量口径
```

- 与 t1-im-client 的边界：T5 绑定层/通道层在彼；本切片的 bootstrap 与 im-gateway 随节点起（P1a）在 T4 联调会对接。
- 与 t1-impl-test 的边界：`--provision-node` 异步 job 总装、M1a E2E 矩阵、destroy/卸载同一清单验收在彼；本文只定 provider/成本/开通页三块。
- reviewer≠author：建议 reviewer=weiye（架构）+ caicai（实现/测试）。
