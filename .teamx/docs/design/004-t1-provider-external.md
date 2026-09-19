# T1 设计：provider 落实 + 成本/合规/外部视角 + Console 开通页（#6 云主机 / #8 部署链路）

- author: xiaods（contributor）
- date: 2026-09-18（初稿）；2026-09-19（按评审 002 修正）
- task: t1-provider-external（high，owner 派发）→ fix-t1-provider-external（评审修正）
- scope: 本文仅覆盖「provider/成本/合规/外部视角 + Console 开通页」切片；IM 侧与客户端面归 t1-im-client（weiye-mini，评审中：owner 终审 PASS 含 2 必改，fix-t1-im-client 修正中）；总体实现切片与测试矩阵归 t1-impl-test（caicai，approved）；架构主笔归 weiye（co-lead）。
- 裁决基线：#6 owner 裁决 dsc_18d644d8ab1de0272222bc#4101（6 决策点收敛 + 结构裁决 A/B + 执行口径）；#8 owner 裁决 dsc_18d65716b4b73bb22222bc#4078（租户 1:1 / App-only / gh-intake 独立 / P0 先行 / M1a 基线）；owner 补充裁决 seq 4921（rqlite/OAuth 批准 + 开通页定位待澄清）/ seq 4934（拓扑澄清 + AWS 先行）；评审记录 `.teamx/docs/review/002-review-t1-provider-external.md`（CHANGES-REQUESTED，3 必改已落实）。本文引用的讨论贡献：#6 侧 #3766/#3770/#3774/#3782（xiaods）、#3802/#3803（weiye）、#3812（caicai）；#8 侧 #3998/#4011/#4038。
- 用户需求输入（2026-09-18，owner 侧新增，按默认设计落实）：R1 Console 开通页（GitHub 凭证登录验证 → 安装 Teamx APP → 拉起服务器装 k8e-sandbox + teamx server → 初始化 team_id/member_id）【修正注记：经评审必改 3 + owner 补充裁决 4934，“装 teamx server”改为 member node（worker/agent），serve 只在中心一台；provider 首批改为 AWS CLI 先行，k8e 后续再议。见 §1/§2】；R2 每用户一台 microvm（预装 opencode 即开即用的远程代理，计费与资源另算）；R3 其余按默认设计。
- 单件细化：invite-provision AWS 主路径详见 `design/006-invite-provision-node.md`（server 侧 AWS CLI 执行，含 D–V 裁决、§3.1 隔离模型、§5.0/§5.1）；本文 §1 只定接口/幂等/状态机通用面，不重复 006。
- 标准目录位置：`.teamx/docs/design/004-t1-provider-external.md`（与本文件内容同源；评审以标准目录为准）。

## 0. 交付物总览（P0/P1 分列）

| 切片 | 优先级 | 内容 | 依赖 | 代码量预估 |
|---|---|---|---|---|
| P0-Provider 接口 + AWS 首实现 | P0 | `CloudProvider` 多后端接口；**AWS CLI 首个实现**（主路径，详见 006）；幂等（C4 复用）；destroy 干净清单 | 无（本地 fixture 可验） | 中 |
| P0-Console 开通页 | P0 | user-console 新增 `/onboard`：GitHub 登录 → 装 APP → 开通 → bootstrap 状态机可视（承载位置待澄清，见 §2.1） | P0-Provider（mock 可先行） | 中 |
| P0-成本闸门数据面 | P0 | 配额/TTL/回收字段 + `budget.alert` T3 接线 + 计量落台账 | 无 | 小 |
| P1-k8e-sandbox 后续 | P1 | k8e 作为第二 substrate（等 AWS CLI 落地后再讨论，不在首批） | P0 接口冻结 | 待定 |
| P1-microvm 镜像 + 按需池 | P1 | 预装 opencode 的 microvm 镜像；ephemeral warm pool；启动预算 SLO | P0-Provider | 中 |
| P1-合规与结算 | P1 | at-rest 加密断言；读取全审计；计量→结算（计费另算，本文只定计量口径） | P0 成本面 | 小 |

## 1. P0：Provider 接口 + AWS CLI 首实现（#6 T3）

**设计要点**：

1. **接口先行、AWS 首落**（owner 补充裁决 4934）：`CloudProvider { create / status / destroy / list }` 首批即按多后端签名设计（region/image/size/ttl 参数位预留），**首个实现为 AWS CLI**（invite-provision 主路径 = server 侧 aws cli 创建/初始化/import，单件细节见 006）；**k8e-sandbox 等 AWS CLI 落地后再讨论，不在首批**（原初稿“k8e 首实现”已按裁决调整）。
2. **幂等键 `(team_id, member_id)`**：`provision_jobs` 表对 idem_key 建部分唯一索引 + `INSERT OR IGNORE` + 冲突回读胜者——直接复用 C4（`im_card_tokens`）已验证模式（caicai #3812），并发双 invite 只出一台；同 user 复用语义见 006 §3.1（复用优先于新建）。
3. **destroy 干净 = 可验证清单**（T3 验收逐项断言，清单口径与 006 §6 同一）：实例 terminated、EIP/公网 IP 释放、安全组规则清理、DNS 清除、secret 清理、member 证书吊销、invitation revoke、OS 用户删除；外加“吊销演练”：destroy 后旧证书打任意 RPC 必须立刻被拒（#62 式 E2E）。
4. **状态机**：`requested → provisioning → bootstrapping → ready → degraded → stopped → terminated`，每次迁移落台账；非法迁移拒绝（T2 单测覆盖）。
5. **invite/provision 解耦**（裁决 A）：`--provision-node` 只入队异步 job；invite 成功不以 job 成功为前提；console 开通页轮询 job 状态而非阻塞等。

**验收锚点**：重复 invite 单台；destroy 两次第二次为无操作成功；destroy 不存在报明确错；云 API 限流注入可重试且不 double-provision；kill 恢复（create 与写库之间 kill → 重放收敛同一台）。

**K8E 架构输入（控制面 / 业务数据面分离；k8e 事实见证：https://github.com/xiaods/k8e.git @ e8da043d）**：

```text
K8E
 ├─ kube-apiserver ───────────→ etcd
 │    Pods / CRDs / Secrets / Leases
 │
 └─ 业务服务 ────HTTP SQL─────→ rqlite【已批准（owner 裁决 4921-①），见下】
      任务历史 / 计费记录 / 业务元数据
```

1. **控制面走 etcd**：Pods/CRDs/Secrets/Leases 等 k8s 状态由 kube-apiserver 落 etcd（k8e 系 k3s 派生，仓内 `cmd/etcdsnapshot`、`pkg/generated` controllers 可证）。网关租约互斥（t1-im-client §3 的 serve 侧注册租约）优先用 k8s Lease 语义实现，不另造租约表。
2. **业务数据走 rqlite（owner 已批准）**：任务历史、计费记录、业务元数据经 HTTP SQL 进 rqlite。**为何不用现有存储**：`teamx.db`（serve 内嵌 SQLite）是单节点控制面库（团队状态 + 台账），把跨节点可查询业务数据塞进去会让业务增长耦合控制面 DB、且无复制；台账 events 是 append-only 审计轨迹（不可变语义），不适合做计费聚合与历史查询。rqlite（SQLite 兼容 + Raft 复制 + HTTP SQL）补的正是“可查询 + 可复制的业务数据面”，serve 保持 dumb（只写事件、不算钱）口径不变。
3. **rqlite 运维/备份口径**（裁决 4921-① 要求）：与 k8e 集群同生命周期部署（sidecar 或同 VPC 独立组）；备份 = rqlite 自带快照 + 台账 events 重放双保险（events 为审计真相源，可重建业务聚合）；保留期与幂等键存续期对齐（≥30d）；凭据走节点 OS 凭据存储（与 app_secret 同口径）。
4. **边界**：teamx 台账 events 表仍为审计唯一真相源（安全/拒绝/绑定事件）；rqlite 只存可查询业务数据（§6 `cost_events`、任务历史、节点元数据副本）。两者分工在实现 PR 描述中显式声明，审计口径不漂移。

## 2. P0：Console 开通页（R1，用户需求）

**拓扑澄清（评审必改 3 + owner 补充裁决 4934-1）**：**serve 只在中心一台（teamx server，现网 `120.27.222.33`）**；被开通的机器是 **member node（worker/agent），不是 serve**。节点持有的是该 team 的 **device member 证书**（user+device 模型），台账唯一写入口仍是中心 serve（mTLS RPC），租户边界由 serve 侧 installation↔team 映射执行。任何“每节点一个 serve”的读法均不成立。

**落点**：user-console（成员 console）新增公开 onboarding 面，与现有 `console.rs` 路由体系同构（`/`、`/login`、`/api/me`、`/api/cloud` 已有；本页为其前端延伸，不另起服务）。

**流程（5 步，状态机可视）**：

```text
① GitHub 登录（OAuth，仅身份环节，App-only 主流程之外）
  → ② 安装 Teamx APP（跳 GitHub App 安装页，回跳带 installation_id）
  → ③ 开通（选 BYO cloud / 平台账号（默认关）；region 白名单校验；进 provision_jobs 异步队列）
  → ④ Bootstrap（cloud-init 只带一次性 token → 节点 `teamx node bootstrap --token` 换 letter → 装 teamx node（worker/agent）→ 初始化 team_id/member_id → 拉团队 repo / 准备 workspace → 起 worker）
  → ⑤ 就绪（presence 上线；可选 im-gateway；console 展示 ready + ssh/ tunnel 入口）
```

（注：AWS 主路径下步骤④的“装机”实为 server 侧 SSH 直推初始化，cloud-init 仅为用户自带机器路径保留；单件细节见 006 §3。）

**设计约束**：

1. **OAuth 只作身份环节**（#4078：OAuth 不作里程碑第一步；登录通零团队能力）。token 短期、只存 session，不进 DB；`state/nonce` 防 CSRF/重放。
2. **App 安装即租户绑定**：回跳的 `installation_id ↔ team` 1:1 映射校验（未知即拒+审计），复用 #8 serve 侧映射口径，不另造第二套。
3. **密钥铁律**（weiye #3803 + #4101-5）：cloud-init 只带一次性 token（单次、TTL ≤15min、绑定 `(team_id, user_id, device_member_id)` 三元组，校验失败不消费）；letter 私钥落盘 0600；app_secret 只落节点 OS 凭据存储；serve/控制台不持有 node 私钥与 app_secret 明文。**例外（owner 已批准，裁决 4921-②）**：GitHub OAuth client secret 由 serve 持有——限定短期 code 兑换、不落盘、可轮换、最小权限；轮换流程与 App webhook secret 同文档。
4. **版本兼容门**（caicai #3812）：bootstrap 第一步校验节点 teamx 二进制与 serve semver 兼容，不兼容 fail-fast；`--opencode-url` 由 bootstrap 显式写入本节点端口（#70 无默认值教训，E2E 锁死）。
5. **新 CLI 面过全调用点清单**（#85 教训，t1-im-client §4.3 同款）：`teamx node {list,status,ssh,stop,start,destroy,bootstrap,provision}` 落地即过“双插件 + docs/skill + E2E”清单，本页为验收项。

### 2.1 开通页承载位置澄清（待 owner 裁决③，不擅断）

评审与 owner 要求先说清：`GET /onboard` + `POST /api/onboard/*` 部署在哪个进程/主机、外部 GitHub 用户如何到达、与 #8「gh-intake 是唯一公网面」的关系。现状与选项（中立陈述，供裁决）：

- **现状**：user-console 默认绑 `127.0.0.1`（本地成员自用）；外部 GitHub 用户无法直接到达。
- **选项 A（serve 侧托管）**：开通页由中心 serve 的公网面承载（与 gh-intake 同进程不同路由，鉴权走 OAuth session + installation 映射）。代价：serve 新增公网路由面，需与 #8 结论对齐（gh-intake 唯一公网面是否扩为“gh-intake + onboard”双路由）。
- **选项 B（teamx cloud 域名）**：经 teamx cloud 统一域名 + cert-manager 签名暴露（与 R2 域名需求同源），后端反代到 serve/onboard。代价：引入域名与证书运维（k8e cert-manager 承接）。
- **选项 C（lead 定向隧道）**：owner/lead 按需经 `teamx tunnel` 暴露本地 console（一次性、可审计）。代价：不可规模化，仅过渡。
- 本文不选边；若与 #8 相关，标注为「待 #8 讨论」延续（owner 补充裁决 4934-4）。
- **owner 输入（2026-09-19，待正式裁决）**：teamx cloud 需要一个**登录门户控制台**（用户远端管理自身资源/状态；市场传播与后续增值服务载体）。按“每用户只装 opencode + teamx skill”的现状无管理端，门户缺失会卡死运营。含义：选项 C（定向隧道过渡）不满足门户要求；A/B 需按“门户”标准补登录态、资源视图与多租户隔离。正式裁决前 §2 实现按 mock 可先行，公网定位不动。

**验收锚点**：mock provider 下开通页 5 步全绿（含 token 四负例：重放/过期/换错设备/已消费全部留审计）；node 重启自愈；destroy 后旧证书立刻不可用；版本不兼容 fail-fast。

## 3. P0：成本闸门数据面（R2 的硬约束部分）

**默认成本模型**（#4101-4 + xiaods #3782 + 006 D/Q（最便宜机型·下限 1C2G），R3 按默认）：默认该 region 最便宜机型、下限 1 vCPU / 2 GiB（可 `--size` 覆盖）；TTL 默认 30d 可续；CPU<5% 持续 30min 自动 stop（粒度：实例级停服务组，整机级仅全实例 idle 才停，见 006 P）；每 user 常驻上限 1 台（复用优先于新建，见 006 §3.1；超限走审批）；欠费/到期自动 terminate（快照保留 7d 后删）。

**设计要点**：

1. **三级拓扑**（#3782）：① 本地 device 零成本默认路径（邀请即引导，先交付）；② 按需 ephemeral（warm pool，用完即毁）；③ 常驻 node 仅 7x24 重度用户。R2 的“每用户 microvm”默认落在②/③之间：**默认 stopped（按任务唤醒），常驻需显式 opt-in**——与“默认成本为零”裁决一致。
2. **`budget.alert` 熔断前移 T3**（#4101 执行口径）：超限自动 stop 与首台开通同批落地；serve 保持 dumb，只收计量事件不算钱；`cost_snapshot` 同时落台账事件（可审计可对账，caicai #3812）；计量事件同时进 rqlite `cost_events`（可查询面）。
3. **计费与资源另算**（R2）：本文只定**计量口径**（CPU/内存/磁盘增量/egress/时长事件），结算、对账单、支付流程明确为后续项，不在本 T1 实现。

## 4. P1：k8e 后续 / microvm 镜像 / 合规结算

1. **k8e-sandbox（后续再议）**：等 AWS CLI 落地后讨论，不在首批；届时同一 `CloudProvider` 接口扩展，不改上层时序与身份模型。
2. **microvm 镜像（R2）**：预装 opencode + teamx worker 的基础镜像 + 共享快照 registry/构建缓存降冷启动；ephemeral 启动预算 SLO：bootstrap 端到就绪 ≤X 分钟断言（X 由 T3 实测定，caicai #3812 建议采纳）。
3. **合规**：workspace at-rest 加密；SSH/secret 读取全审计；tenant 隔离复用 k8e per-session 边界（gVisor 默认）+ 单网关审计出口；`node_secrets` 只存引用。
4. **卸载即同一张清单**（weiye #4001-4，#8 侧）：卸载 APP = 吊销证书 + destroy + 删 installation token 与已 clone 内容——与 §1 destroy 清单合并为同一验收口径，由 t1-impl-test 主责，本文引用。

## 5. 外部视角（#8 对标，R3 默认）

orbi.build 对标结论：本 T1 的“Console 开通页 5 步”即对标面的可交付形态（一键部署 = 开通页 + token bootstrap + destroy 清单三件套）；部署链路**复用 #6**（token bootstrap、destroy 清单、region、预算），不另造第二套（#4078 T1 硬要求）。GitHub 侧差异点（installation 模型、delivery 语义）归 gh-intake 车道（t1-impl-test），本文不重复。

## 6. 接口与数据（采纳 #3753 §10 + 006 §4 增量）

```text
nodes(id, team_id, user_id, member_id, provider, region, instance_id,
      ip, state, im_enabled, os_user, keypair_ref, created_at, ready_at, terminated_at,
      cost_snapshot, compliance_tags)              --os_user/keypair_ref 必填（引 006 §4）
provision_jobs(id, node_id, state, attempts, last_error, idem_key, created_at)
node_secrets(node_id, kind, ref, created_at)       --只存引用，不存明文
cost_events(ts, node_id, meter, value, unit)       --计量事件（台账 + rqlite 双写；结算另算）
```

```text
teamx team invite <role> --user-name <人> --provision-node [--provider aws --region .. --size .. --ttl 30d] [--im feishu]
teamx node provision --member <member_id> [--provider aws --region .. --size .. --ttl ..]  --补开（引 006 §5.0）
teamx node list | status <id> | ssh <id> | stop <id> | start <id> | destroy <id>
teamx node bootstrap --token <t> [--im feishu]     --用户自带机器路径保留
```

Console 面（user-console 新增）：`GET /onboard`（5 步向导页）+ `POST /api/onboard/{login,install-callback,provision,bootstrap-token,status}`；状态订阅复用 `/api/stream`（SSE 台账流，不新开通道）；承载位置待澄清（§2.1）。

## 7. 风险与开放项

1. 平台统一账号的熔断 E2E 未落地前只走 BYO（#4101-2），开通页默认隐藏平台账号选项。
2. GitHub OAuth client secret 由 serve 持有（**owner 已批准**，限定短期 code 兑换、不落盘、可轮换、最小权限；轮换与 App webhook secret 同文档）——例外状态关闭，转为受约束允许。
3. 无 systemd 容器场景的常驻方案（worker/gateway 自启替代）→ T4 与 provider 切片对齐定（t1-im-client §7 同款开放项）。
4. ephemeral warm pool 的容量与驱逐策略 → P1 实测定，不阻塞 P0。
5. 开通页公网定位（§2.1）→ 待 owner 裁决（与 #8 对齐）。

## 8. 测试设计汇总

| 层 | 测试 | 类型 | 责任切片 |
|---|---|---|---|
| T2 状态机 | 重复 invite 单台 / 非法迁移拒 / user-member 一对多断言 | 单测 | 本文（provider） |
| T3 开通回收 | 幂等开通 / destroy 干净清单 / 吊销演练 / 限流注入重试 | E2E（mock 先行，AWS 实机后补） | 本文（provider） |
| T3 复用语义 | 同 user 两 team invite → 复用主机只加 OS 用户 | E2E | 006 详述，本文引用 |
| T4 bootstrap | token 四负例+审计 / 重启自愈 / 版本门 / opencode-url 透传断言 | E2E | t1-impl-test 总装，本文引用 |
| 开通页 | 5 步端到端（mock provider + fixture installation）/ region 非法拒 | E2E | 本文（provider） |
| 成本 | 超限 auto-stop（实例/整机两级）/ TTL terminate / cost 事件双写 | E2E（T3 同批） | 本文（provider） |
| 调用点清单 | 新 node CLI 面过双插件+docs/skill | 流程验收 | 本文（provider） |

## 9. 排序与依赖

```text
P0 provider 接口 + AWS 实现（T2 本地状态机先行；单件 006 同步推进）
P0 成本闸门数据面 ───────────── 与上并行（字段 + budget.alert 接线）
P0 Console 开通页 ───────────── 依赖 provider job 状态机（mock 可先行；承载位置待澄清）
P1 k8e-sandbox ──────────────── 等 AWS CLI 落地后再讨论
P1 microvm 镜像 + 按需池 ────── 依赖 P0 AWS + T4 bootstrap
P1 合规结算 ────────────────── 依赖 P0 计量口径
```

- 与 006 的关系：006 是 AWS 主路径单件细化（含 D–V 裁决、§3.1 隔离模型、§5.0/§5.1）；本文 §1/§6 与之同口径，细节不重复。
- 与 t1-im-client 的边界：T5 绑定层/通道层在彼；本切片的 bootstrap 与 im-gateway 随节点起（P1a）在 T4 联调会对接。
- 与 t1-impl-test 的边界：`--provision-node` 异步 job 总装、M1a E2E 矩阵、destroy/卸载同一清单验收在彼；本文只定 provider/成本/开通页三块。
- reviewer≠author：建议 reviewer=weiye（架构）+ caicai（实现/测试）。

## 10. 修订历史（评审必改落实）

- 2026-09-19 fix（fix-t1-provider-external）：落实评审 002 三必改——① scope 行 t1-im-client 改“评审中”；② rqlite 转已批准 + 运维/备份/分工论证；③ 拓扑澄清（serve 单中心 + member node）并改写 §2④/R1；provider 排序按补充裁决翻转为 AWS 先行/k8e 后续；§8 补责任切片列；K8E 事实见证 k8e 仓 commit；§2.1 开通页定位中立待裁决。
