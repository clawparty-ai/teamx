# 009 — Teamx Cloud 第一轮脑爆 harvest：共识结论与开放问题（第二轮输入）

> 状态：**round-1 harvest（只整理，不筛不砍、不排序）**
> 来源：讨论 `dsc_18d7008f8c851cbb26a0d3`（33 帖：idea 18 / question 5 / concern 3 / reference 3 + owner 裁决 1）
> 投稿：weiye 13、weiye-mini 13、xiaods-mac 3、caicai 1（由 weiye-mini 代贴）、caishu 3
> 日期：2026-09-20 · 用途：**第二轮方案讨论的输入与参考**

---

## A. 硬结论（owner 已裁决，第二轮不再讨论）

| # | 结论 | 来源 |
|---|---|---|
| A1 | 租户隔离粒度：**共享集群**（不做每租户独立集群） | #6186 |
| A2 | teamx server：**每个租户一个实例** | #6186 |
| A3 | 数据库：**先用 SQLite**（PVC 持久化） | #6186 |
| A4 | member：**容器化与 EC2+systemd 并存**；teamx cloud **只支持 k8e** | #6186 |
| A5 | 主机来源：**BYO**（平台不代开） | #6186 |
| A6 | **第一版不做**计费/配额 | #6186 |
| A7 | k8e 定位：**基础底座**（不作为对外产品形态） | #6186 |

## B. 方法论共识

| # | 结论 | 来源 |
|---|---|---|
| B1 | 脑爆只做**发散**，取舍/排序一律留到第二轮 | #6135（owner 澄清） |
| B2 | 第一轮产物 = **可能性清单**（不筛不砍），每条带来源与"下一步能验证什么" | #6135 |

## C. 方向性共识（多人独立提出 / 无人反对，第二轮需确认并落设计）

| # | 结论 | 提出 | 来源 |
|---|---|---|---|
| C1 | **演进替代而非重写**：新增 `provider=k8e` 作为 design/006 的第三后端，复用 `nodes`/`provision_jobs`/幂等键 `(team_id,member_id)`/复用语义/destroy 清单/状态机 | weiye-mini、weiye | #6037 #5965 #6093 |
| C2 | **v1 从单节点 all-in-one k8e 起步**，3 控制面 HA 后置（HA 的运维复杂度是固定成本最大头） | weiye、weiye-mini | #6090 #6039 |
| C3 | **member workspace 必须持久化**：PVC per member（`emptyDir` 丢未提交不可接受） | weiye-mini、caicai | #6038 #5985 |
| C4 | **邀请函/证书走 K8s Secret 注入 → 容器内 `teamx team import`**（不进镜像、不进 cloud-init，与 006 裁决 O 一致） | weiye-mini | #6038 |
| C5 | **镜像内容 = 006 §5.1 清单 1:1 平移**；但与 EC2 相反：**镜像预装 teamx/opencode**（容器要快启动） | weiye-mini、caicai | #6038 #5985 |
| C6 | **默认禁出网**（deny-all egress + 白名单：serve/registry/git），并**显式封 IMDS `169.254.169.254`** | weiye、weiye-mini | #6068 #5970 |
| C7 | **v1 就上 PSA(restricted)+seccomp+只读根文件系统** | weiye、weiye-mini | #6071 #6041 |
| C8 | **平台管理员默认无 exec 权限**，需 break-glass（显式申请+留痕+事后告警）；审计流独立于租户可控台账 | weiye | #6072 |
| C9 | **冷启动拆三段分别埋点**：①调度+拉镜像 ②workspace 恢复 ③agent 热身（先测出瓶颈再投钱） | weiye、weiye-mini | #6069 #5970 |
| C10 | **member 先建（秒级可达）、provision 异步**，冷启动不进关键路径；预热池降级为体验优化 | weiye | #6069 |
| C11 | **#8 与 #9 共用租户键**：`installation_id ↔ team_id ↔ namespace` 1:1:1（避免两套租户体系并行生长） | weiye | #6091 |
| C12 | **验收复用现有 E2E**：把 xiaods-ec2 全链原样跑一遍 `provider=k8e`，加两类负例（跨租户不可达、拔节点演练） | weiye | #6093 |
| C13 | **成本：平台侧是唯一计费权威**，台账只引用平台产出（`cost_snapshot` 先例）；**存储/镜像是被低估的长尾** | weiye | #6070 |
| C14 | **升级/回滚/pin 必须进设计**（每租户一 server 的滚动升级成本随租户数放大） | caicai | #5985 |
| C15 | **平台自身 bootstrap 复用 006 的一次性 token**（不另造引导协议） | weiye | #6070 |
| C16 | **第一步可先做「server 不动、只容器化 member」**（member Pod 直连现有 server，风险最小；server 容器化/Postgres 为第二步） | weiye-mini | #5965 |

## D. 开放问题（第二轮需要讨论或 owner 拍板）

### D-1 架构原语（最硬，先答）

| # | 问题 | 选项 | 来源 |
|---|---|---|---|
| D1 | **「OS 用户」在 k8s 的对应物** | A 一成员一 Pod、每 team 一容器（同 Pod 多容器，隔离弱于现状）／ B 一 team-member 一 Pod（隔离最接近现状，成本与冷启动 ×N）／ C 一成员一 Pod、多 team 共用容器靠目录隔离（最省、隔离最弱） | weiye #6092（倾向 A + 文档如实声明） |
| D2 | **CA 归属** | 每租户独立 CA（密码学边界，吊销=废 CA）／ 共享 CA + CN 编租户（便宜，但边界靠代码不出错） | weiye #6067（主张独立）**vs** caicai #5985（主张共享+CN）—— **有分歧** |
| D3 | **member workspace 根路径** | A 路径稳定（容器内挂出相同绝对路径）／ B 动态发现（只保证内容一致）／ C 双轨（EC2 老成员保路径，k8e 新规范） | xiaods-mac #6408 |
| D4 | **forward 的"本地"锚点** | A 本地=Pod 内 ／ B 本地=用户设备（保留今天语义）／ C 本地=server 侧（不 forward） | xiaods-mac #6406 |
| D5 | **隧道（member 数据面）形态** | A sidecar expose（随 Pod 生死）／ B 节点级多路 expose（开销小、爆炸半径大）／ C 用户态自愈（退避+探针+断线计数） | xiaods-mac #6405（现实证据：EC2 隧道重启后 1h 再次僵死；ws 读写无超时、4.5h 零重连日志） |
| D6 | **BYO 主机接入** | SSH 一键 / cloud-init / join token 三选一；**key 归属**（server 持钥 vs 用户持钥）、**单元命名碰撞**、**join token 防重放** | xiaods-mac #6407 |
| D7 | **共享底座的爆炸半径** | 节点池分层 / ≥2 worker + 滚动窗口 / 网络附着存储或显式快照策略（把"节点死"从丢数据降级为可恢复停机） | weiye #6089 |

### D-2 身份 / 存储 / 交付

| # | 问题 | 选项 | 来源 |
|---|---|---|---|
| D8 | **GitHub 身份面是否与 #8 合一** | 合一（一套 client secret，Cloud 管登录租户、App 管 repo 事件）／ 分开（两套集成，边界要写清） | weiye-mini #5966 #6042 |
| D9 | **SQLite→Postgres 的时机判据** | 现在拍死 / 挂量化信号（QPS、租户数、PVC IOPS）后置 / 写迁移预案成文 | weiye-mini #5966 |
| D10 | **平台管理员的数据可见边界** | 能否 exec、是否需要租户同意、台账哪些字段可见（任务契约/讨论/成本/成员 IP） | weiye #6072 |
| D11 | **存量迁移路径** | EC2 workspace → PVC 一次性 rsync ／ 快照转储 ／ 双 provider 长期并存不迁 | weiye #6093 |
| D12 | **平台自身 bootstrap** | 第一个租户/集群从哪来、平台管理员凭据怎么发（建议复用 006 一次性 token） | weiye #6070 |
| D13 | **v1 里程碑切分** | 一步到位（server+member 都容器化）／ 两步（先只容器化 member，server 不动） | weiye-mini #5965 |

### D-3 成本 / 体验 / 过程

| # | 问题 | 选项 | 来源 |
|---|---|---|---|
| D14 | **平台固定开销如何兜底**（无计费/配额前提下） | 最小套餐地板 ／ 资源用量封顶免费额度 ／ 默认规格+超卖 | weiye #6090 |
| D15 | **计量口径** | 活跃秒（技术成本）／ member-小时（定价口径）／ 双轨（v1 不做计费，但埋点口径要定） | weiye #6070 |
| D16 | **冷启动交互期待** | 保持"创建即同步 provision" ／ 改异步 + UI 显示"准备中" ／ 仅首次邀请同步 | weiye #6069 |
| D17 | **强隔离档是否进 v1 定价分档** | 默认档（共享内核+PSA）／ gVisor(runsc) ／ 每租户节点池（企业档） | weiye #6071、weiye-mini #6041 |
| D18 | **收窗/中期 harvest 节奏** | 中期 harvest（不关窗）／ 等 48h 自然到期 ／ 等外部视角（addozhang 等）补齐 | weiye #6100 |

## E. 第二轮建议议程（供 owner 参考，不预设结论）

1. **先拍 4 个架构原语**：D1（隔离对应物）、D2（CA 归属）、D3（workspace 路径）、D5（隧道形态）
   —— 这四条不定，4.x/5.x/6.x 的讨论都是空转。
2. **再拍交付与里程碑**：D13（一步/两步）、D7（爆炸半径与存储）、D11（存量迁移）、D6（BYO 接入）
3. **再拍身份与安全**：D8（GitHub 身份面）、D10（平台可见边界）、D4（forward 锚点）、D17（隔离分档）
4. **最后拍成本/体验与过程**：D14–D16、D18

---

## 附：本轮的"属性轴"（讨论中被反复使用的评价维度，第二轮直接复用）

隔离强度 · 爆炸半径 · 成本（固定/长尾）· 冷启动 · 运维负担 · 可逆性 · 可观测性（重连/断线计数）· 数据主权 · 体验（离线可用/链路跳数）· 承诺与实现的落差（诚实声明优先）
