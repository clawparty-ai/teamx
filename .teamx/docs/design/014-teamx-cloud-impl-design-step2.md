# 014 — Teamx Cloud 第二步实现设计：每租户 server 容器化 + 独立 CA + 平台控制台（v2 · 已按评审修订）

> 状态：**draft（实现级设计，待评审）** · 所有者：caishu · 日期：2026-09-20
> 范围：**第二步里程碑**（`design/011` D13）：把 teamx server 变成「每租户一个容器实例 + 独立 CA」，
> 落地 `installation_id ↔ team_id ↔ namespace` 三元组，并交付**平台控制台**（GitHub 登录 → 建 project → 邀请 → provision → 回收）。
> 输入：`design/010`（原语）· `011`（交付）· `012`（身份/成本）· `013`（第一步实现）· `requirements/002` v3
> 实现落点：`crates/teamx/src/k8s.rs`（扩展）、`crates/teamx/src/console*.rs`（平台面）、`crates/teamx/src/pki.rs`（租户 CA）

---

## 1. 范围与不变量

**第二步做**：每租户一个 teamx server 容器（含独立 CA、SQLite+PVC）· 三元组 1:1:1 · 平台控制台 · 平台 bootstrap。

**不做**（仍后置）：计费/计量（D14/D15）· gVisor/独立节点池（D17）· 多集群联邦 · im-gateway 节点侧。

**不变量**
1. **`team_id ↔ namespace` 恒 1:1**（C11 硬约束）——任何一侧缺失/重复都是非法状态；
   `installation_id` **可空**（CLI 创建的团队、IM 来源团队没有 GitHub installation），
   **有值时才参与 1:1 断言**（一个 installation 不得对应多个 team）。
   *（v2 → v2.1 修订：weiye task-92 必改③——原文"三层 1:1:1"会让非 GitHub 来源的合法租户全部非法。）*
2. **每租户独立 CA**（D2）；CA 私钥与租户 server 同生命周期、**必须进灾备**。
3. 平台管理员**默认仅见元数据**（fail-closed 封闭枚举）+ 授权式 break-glass（D10）。
4. 第二步**不破坏第一步**：member Pod 从"直连旧 server"平滑切到"连本租户 server"（同 `nodes`/job 抽象）。

## 2. 租户模型（三元组）

| 概念 | 值 | 说明 |
|---|---|---|
| `installation_id` | GitHub App 安装 id（D8 合一后） | 租户键的源头 |
| `team_id` | teamx team id | 与 installation 1:1 |
| `namespace` | `tenant-<team_id 全长或 ≥16 字符 hash>` | K8s 隔离与配额的作用域。**M5 裁决（013 v3）：namespace 是安全边界，禁止 8 字符截断**（32bit 熵碰撞=跨租户混住）；namespace 内 Pod/PVC/Secret 同口径 16 字符，防漂移 |
| 租户 server | Deployment `teamx-serve-<team8>`（namespace 内） | 单副本 + PVC |
| 租户 CA | Secret `teamx-ca-<team8>`（namespace 内） | 私钥加密存储 + 进灾备 |
| 成员 | Pod `teamx-m-<member8>`（同 namespace） | 第一步已实现，改指本租户 server |

**生命周期**：创建租户 = 建 namespace + CA + server（Deployment/PVC）+ 初始 seat；删除租户 = 逆序清空
（server → PVC/Secret → CA → namespace）+ 吊销（废 CA 即一次动作）。

## 3. 每租户 teamx server 容器化

| 项 | 设计 | 依据 |
|---|---|---|
| 工作负载 | **Deployment**（单副本，`strategy: Recreate`）+ PVC | SQLite 单写者；Recreate 避免双写 |
| 数据 | **SQLite + PVC**（`/data/teamx.db`） | A3；迁移预案见 §8 |
| 镜像 | `teamx-server:<版本>`（**与 member 镜像同源**：同一 base + 同一 `TEAMX_BIN`，独立 tag；server 更轻——不需要 opencode/ttyd/bun）。统一构建入口 `docker/build.sh`（含同源校验：两镜像的 teamx 版本必须一致） | 版本 pin |
| 配置 | env：`TEAMX_HOME=/data`、`TEAMX_DB=/data/teamx.db`、`TEAMX_TENANT_TEAM_ID=<team_id>` | |
| 端口 | `5783`（mTLS RPC）+ 健康检查 `/health` | 与现网一致 |
| CA | 启动时读 Secret `teamx-ca-<team8>`；缺失则**生成并写回**（首次） | D2 |
| 证书 | 租户 CA 签发本 server 的服务端证书 + 成员证书 | 沿用 pki |
| 调度保护（建议②） | 租户 server Pod（单副本、扛全租户控制面）使用 **server 档** `PriorityClass`（013 M7 两档）+ **PDB `maxUnavailable=0`**；member Pod 用 member 档。 |
| 升级/回滚 | **重建式升级**（`Recreate`，短暂下线 + 恢复断言，非零停机滚动）；**保留上一版 image tag 供回滚**；`TEAMX_VERSION_PIN` 可锁定 | C14 + 评审措辞修正 |
| 灾备 | 备份 = SQLite 快照 + **CA Secret** + 关键 Secret；恢复演练断言"恢复后旧成员证书仍有效" | D2/NFR-4 |

**与第一步的差异**：第一步 member Pod 连的是"平台那台 server"（`nodes.server_url`）；第二步把
`server_url` 指向本租户 server 的 Service DNS（`teamx-serve-<team8>.<ns>.svc:5783`），其余不变（同 provider/job/清单）。

## 4. 独立 CA（D2）

| 环节 | 设计 |
|---|---|
| 生成 | **裁决（weiye 必改①）：平台在建租户时生成**（`create_tenant` 一次完成 CA + server 证书 + 成员/服务证书的签发基础），私钥只写进租户 namespace 的 Secret（0600）。**平台"写完即忘"，不保留 CA 私钥副本**——因此平台后续要签 service 证书时，必须经 kubeconfig **读回**该 Secret（见 §6.5），而不是持有副本。EC2/BYO 自建路径（无平台参与）才走"server 首启缺失则自生成"。<br>*合规口径：平台在写入 Secret 的那一刻短暂经手 CA 私钥（同一次 kubeconfig 操作内），不落平台库、不进日志、不返给调用方；这条须在 §7 边界声明中显式写出。* |
| 存储 | K8s Secret（`teamx-ca-<team16>`）；**不进镜像、不落节点磁盘**。<br>**口径修正（weiye 建议①）**：K8s Secret 默认只是 base64（etcd 明文）——本设计**不承诺"加密存储"**，改为三项可验证约束：① **集群前提**：启用 etcd encryption at rest（与 013 的 CNI 前置同款清单，部署检查项）；② **RBAC + namespace 隔离**：仅租户 namespace 内的 workload 与该 namespace 的管理者可达；③ **平台不保留副本**（见上"生成"行）。 |
| 轮换 | 支持新旧 CA 并存期（双 CA 信任窗口，**实现 = server TLS 的 trust bundle 拼接新旧 CA**）；轮换动作 = 生成新 CA → server 重载 → 成员证书逐步换发 → 废旧 CA。<br>**换发触发器（weiye 必改②）**：member 证书是**静态 letter**（Pod 创建时 import，无在线续期），所以"逐步换发"**唯一现实路径 = 逐个滚动重建 member Pod**（PVC 保留、重走 setup）——由 `teamx tenant reissue-members`（**已实现**，见 §9.6）一次性为全租户成员签新证书 + 投递 Secret + 同批切 `server_url`，随后 Pod 重建时自动 import。触发方式：**operator 驱动**（显式命令/平台控制台入口），不做"成员下次连接时下发"的在线续期（v1 无该机制，故也不承诺）。<br>**重建语义**：换发期间**成员会话会中断一次**（Pod 重建），需在迁移 runbook 中写明，不当作故障。<br>**"废旧 CA"前置校验（未实现，开放项）**：必须能断言"旧 CA 签发的**有效证书数 = 0**"才可废；当前**没有**按 CA 分代的签发台账（server 无法枚举自己签过的证书）→ 列为 §11 开放项，在轮换实现时一并补（否则"逐步换发"无法证明完成）。 |
| 吊销 | **吊销租户 = 废该 CA**（一次动作）；吊销单个成员 = seat 置 `left`（第一步已实现，serve 派发前拦截） |
| 灾备 | CA Secret **必须**在备份清单内；恢复后旧成员证书仍可验证（验收断言） |
| 边界 | 平台 console 作为客户端时**按租户存放成员证书**，不得做成"信任所有租户 CA"的角色（D2 边界提醒） |

## 5. 平台控制台（platform console）

| 维度 | 设计 |
|---|---|
| 定位 | **平台面**：登录 / 租户 / 集群状态 / 用量元数据；**不承载**租户内的团队协作（那是 user-console） |
| 登录 | GitHub OAuth（D8 合一：同一 GitHub App；user token 只读身份/仓库列表）→ 绑 `users.id`（人的锚点） |
| 功能面 | ① 登录/登出 ② 创建 project（= 建租户：namespace+CA+server+初始 seat） ③ 邀请成员 ④ 一键 provision member ⑤ 查看资源/用量元数据 ⑥ 回收租户 |
| 与既有 console 的分层 | **platform console**（新建，平台面）/ **admin-console**（`teamx ui`，租户内 lead 看板）/ **user-console**（`teamx console`，成员自助）—— 三者数据源分层：平台面读平台 API，租户面读租户 server RPC |
| API | 平台 API（REST + SSE）：`/api/tenants`、`/api/tenants/{id}`、`/api/provision`、`/api/quota`（v1 只读）；租户内操作仍走 teamx RPC（mTLS） |
| 承载 | 平台 console 自身也容器化（namespace `teamx-platform`），Ingress + TLS |

## 6. 平台自身 bootstrap（C15）

- **第一个租户与平台管理员凭据**：复用 `design/006` 的**一次性 token** 语义（已有兑现与吊销），
  不另造引导协议：
  1. 平台部署时生成一次性 bootstrap token（打印到日志/环境变量）；
  2. 持有者用它完成平台管理员绑定（GitHub 登录 + token 校验）；
  3. token 兑现后即失效，操作落审计（NFR-9）。
- 平台 console 的 GitHub App 凭据：serve 持有 + 不落盘 + 可轮换（#8 裁决），平台面复用同一条口径。

## 6.5 平台 console ↔ 租户 server 的认证通道（评审补：重大缺口）

平台 console 的"邀请成员 / 一键 provision / 查看用量"要**调租户 server 的 RPC（mTLS）**，而平台不是成员、没有成员证书。

**签发路径（weiye 必改①，已实现 `98f8059`）**：平台**不持有**租户 CA 私钥副本，签 service 证书时
经 kubeconfig **读回**租户 namespace 的 CA Secret → 用该 CA 现场签一张
`CN=service:platform-console:<instance>`（EKU=ClientAuth）→ 立即用于本次 mTLS 调用。
私钥只在内存中短暂存在，不落平台库/日志。**证书短 TTL + 每次调用重签**（不做长期证书缓存，
避免"平台持有一张永久租户身份"的边界模糊）；吊销/轮换路径见 §11 开放项。

**设计（复用既有二维服务白名单）**：
- 平台 console 以 **service 车道**身份接入：新增 `service_kind = platform-console`，证书**由该租户 CA 签发**
  （即平台作为租户的一个受限服务主体，而非"信任所有租户 CA"的角色 —— 与 §4 边界不冲突：方向是"租户信任平台这一张证书"，不是"平台信任所有租户"）。
- **方法白名单 fail-closed**：该 service_kind 仅允许 `tenant.` 元数据读写 + 成员邀请/provision 触发；
  其余方法一律拒绝（沿用 t1-im-client 的 `service_kind → allowed_methods` 机制，`crates/teamx/src/serve.rs` 已实现）。
- 配额与审计在 serve 侧（NFR-9 审计独立）；证书短 TTL + 可轮换 + 可吊销。
- 平台库只保存"租户 → 平台服务证书"的引用（不落明文私钥）。

## 7. 安全与可见边界（D10）

- **默认仅元数据**：平台可见 = 成员/状态/成本/资源用量/租户健康；**不可见** = 任务正文、讨论正文、代码、密钥。
- **封闭枚举（fail-closed）**：字段白名单是代码里的**封闭集合**，不在集合内即不可见；新增字段必须显式加白。
- **break-glass**：租户显式授权 + 短 TTL + 单次 + 全程留痕 + 事后告警（通知租户**全部 lead**）。
- **审计独立**：平台侧审计存平台库，租户不可删改。
- 隔离：namespace + PSA(restricted) + NetworkPolicy（**CNI 必须支持策略执行**，见 `design/013` v2.1 前置条件）+ ResourceQuota/LimitRange。

## 8. 迁移与兼容

| 项 | 策略 |
|---|---|
| member 的 server 指向 + **证书换发** | ⚠️ **必须同批生效**（否则 member 直接失联）：<br>① 新租户 server 就绪后，用**租户 CA** 为该租户成员**签发新证书** → 经 Secret 注入 → member Pod 重建时 import（复用 013 §4 的 setup 机制）；<br>② `server_url` 切换与证书换发**同批**完成；<br>③ **旧 server 保留**至新链路验证通过（D11 源保留原则）。<br>理由：member 手里是**旧平台 CA** 签的证书，切到新独立 CA 的 server 后 mTLS 验证失败。 |
| 存量（第一步建的）member | **不强制迁移**（D11）；保留在旧 server 直到用户显式迁移 |
| SQLite → Postgres | v1 不迁（A3）；迁移判据：QPS / 租户数 / PVC IOPS 出现瓶颈（`requirements/002` §9 开放项） |
| 双 provider | EC2（BYO/自建）与 k8e（云上）继续并存，同 `nodes`/job 抽象 |

## 9. 验收（第二步）

1. **租户全链**：创建 project → namespace+CA+server 就位 → 邀请 → provision member → 成员可用 → 删除租户 → 资源清空（可验证清单）。
2. **三元组一致性**：断言 `team_id ↔ namespace` 1:1（负例：重复/缺失即拒绝）；`installation_id` **仅对 GitHub 来源租户**断言（有值时不得一对多）。
3. **CA 轮换**：双 CA 窗口内新旧证书都能验证；轮换完成后旧 CA 失效。
4. **CA 吊销**：废 CA 后该租户所有成员 RPC 被拒（一次动作）。
5. **灾备恢复**：备份含 CA；恢复后**旧成员证书仍有效**（断言）。
6. **平台可见边界负例**：平台无授权读任务/讨论正文 → 拒 + 审计；授权过期后不可再读。
7. **升级/回滚**：Deployment 滚动升级；回滚到上一版 image tag 成功（断言 `/health` + 成员不受影响）。
8. **bootstrap**：一次性 token 兑现后失效；重复使用被拒 + 审计。

## 9.5 评审修订记录（v1 → v2）

| 来源 | 意见 | 落点 |
|---|---|---|
| weiye-mini（task-93） | 必改 1：namespace 命名违反 M5（8 字符截断） | §2（改全长/≥16 hash）+ **代码 `short_id` 8→16** |
| | 必改 2：迁移缺**成员证书换发**（上线即断链） | §8（三步同批生效） |
| | 重大缺口：平台 console ↔ 租户 server 认证通道未设计 | 新增 §6.5（service 车道 + 二维白名单 fail-closed） |
| | 建议 1：M11 kubeconfig 信任域收敛声明 | §10 前段 |
| | 建议 2：`Recreate` 措辞 | §3（改"重建式升级"） |
| | 建议 3：users 双层权威 | §10 前段（平台 users.id 为权威源） |
| | 建议 4：双 CA 并存实现 | §4（trust bundle 拼接） |
| | 开放项 6：平台 console HA/灾备 | §11.5 补充 |

## 9.6 实现状态（截至 2026-09-22）

代码仓 `teamx-enterprise`（分支 `main`，逐步单独提交）：

| §9 验收项 | 状态 | 证据 |
|---|---|---|
| 1 租户全链 | ✅ 已实现（stub 集群 E2E） | `teamx tenant create/destroy`（`09cb884`/`a71debb`）+ `tests/tenant-e2e-test.sh` N1–N7；平台控制台入口 `1a0e56f`/`33c5a45` |
| 2 三元组 1:1:1 | ✅ 已实现 | `provision::create_tenant` 幂等（重复 create 不换 CA，N3）；`short_id` 16 字符（M5，N7） |
| 3 CA 轮换（双 CA 窗口） | ⬜ 未实现 | 设计在 §4；需 server TLS trust bundle 拼接 + 轮换动作 |
| 4 CA 吊销 | ✅ 已实现（=废 CA Secret） | `destroy_tenant` 的 `revoke-tenant-ca` 步骤（`a71debb`，N5） |
| 5 灾备恢复（含 CA） | ⬜ 未实现 | 设计在 §4；需备份/恢复演练 + 旧成员证书仍有效断言 |
| 6 平台可见边界负例 | ✅ 部分 | 平台面只读元数据 + 审计独立（`4d192ff`）；service 车道方法白名单 fail-closed（`784ced3`）；**授权式 break-glass 读正文未实现** |
| 建议② 租户 server 调度保护 | ✅ 已实现 | 租户 server Pod 用 server 档 PriorityClass + PDB `maxUnavailable=0`；断言见 `tests/tenant-e2e-test.sh` N9 |
| 7 升级/回滚 | ⬜ 未实现 | 镜像同源构建已就绪（`75971a4`）；滚动升级/回滚断言未写 |
| 8 bootstrap 一次性 token | ✅ 已实现 | `platform bootstrap-token/redeem/revoke`（`4d192ff`）+ `tests/platform-test.sh` 7 断言 |
| §8 迁移 ①②③ 成员证书换发 | ✅ 已实现 | `teamx tenant reissue-members` + RPC + 平台控制台路由（`98f8059`）；E2E 断言换发信 CA **DER 指纹 == 租户 CA**（tenant N8）+ 控制台入口（platform-console C8） |
| 🖥️ 台式机真集群验证（2026-09-23，k3s v1.34 单节点） | ✅ 部分 | 单测 394 + stub E2E 47 全绿；member 真集群 `k8e-real-cluster-test.sh` **ALL PASSED**（R6b 走 flannel 显式例外）；**租户首次上真集群**：namespace/CA Secret/Deployment/PVC(Bound)/PDB/PriorityClass/Service 全就位，server Pod **1/1**，Service DNS 解析正常，destroy 清单完整。抓到并修了 4 个真机问题（见下） |

其它已实现但不在 §9 列表内的：平台控制台骨架（`1a0e56f`/`33c5a45`，含 launch token 登录 + GitHub OAuth 配置面）、租户全链 E2E 接入 `run-all.sh`（18c–18f）。

**台式机真集群抓到的 4 个问题（已修）**：
1. 测试 NS 取 8 位已过期（short_id 早改 16 位）→ R1 误报。
2. R6b 的 CNI 探测 `grep` 无匹配 + `set -e/pipefail` → 脚本静默死（无 FAIL 无 PASS）；两处加 `|| true`。
3. **租户 server 无 Service** → service 车道拨号目标不存在；补 ClusterIP Service。
4. **server Pod 永远 0/1**：mTLS 下 kubelet HTTPS 探针无客户端证书 → 改就绪文件 + exec 探针。
另：台式机 k3s 自身 overlay 曾因换网段（flannel public-ip 陈旧）瘫痪，表现为 PVC 永 Pending + 调度器对 PVC Pod 静默 —— 重启 k3s 自愈；与本项目无关，已记录。

**明确未做**（避免"看起来完成"）：GitHub OAuth 真实 token 交换（网络不可达，接口如实返回未完成）；平台服务证书吊销/轮换；CA 轮换双窗口与灾备演练；**站外控制台直连租户 ClusterIP（开放项 O-5）**。

## 9.7 评审处置（weiye task-92 · v2 评审 CHANGES-REQUESTED）

评审原文：`.teamx/docs/review/014-review-teamx-cloud-impl-step2-weiye.md`（co-lead weiye，reviewer≠author ✅）。

| # | 意见 | 处置 | 落点 |
|---|---|---|---|
| 🔴 重大缺口 | 迁移只设计了 member 侧；**team 服务端状态**（ledger/goal/tasks/discussions/members/tunnels/IM 绑定）在旧库，新租户 server 是空库——"用户显式迁移"后成员签到的是一个"失忆"的租户；全仓无 team 状态导出/导入能力 | ✅ **已裁决（owner @ 2026-09-22）**：采纳 weiye 倾向——**(c) v1 默认**（第二步只服务新建租户，存量 step-1 永久留旧 server，与 D11 一致）+ **(a) 作为显式选项**（控制台显式「全新开始」，标注空账本）；**(b) 导出/导入列后续**，v1 不做 | §11 开放项 O-1 |
| 必改① | platform-console service 证书**签发路径**未定义（鸡生蛋：CA 私钥在租户 server 侧，平台签不了） | ✅ **已裁决并已实现**：平台建租户时生成 CA；平台不保留副本，签 service 证书时经 kubeconfig **读回** Secret 现场签、短 TTL、每次重签 | §4 生成行 + §6.5 签发路径 |
| 必改② | CA 轮换的**成员侧换发触发器**未定义；"废旧 CA"缺前置校验 | ✅ 触发器已实现（`tenant reissue-members`，operator 驱动 + Pod 重建语义 + 会话中断一次）；⚠️ **"旧 CA 有效证书数=0"前置校验未实现**（无按 CA 分代签发台账）→ 开放项 O-2 | §4 轮换行 + §9.6 |
| 必改③ | 不变量 1 对**非 GitHub 来源租户**不成立（CLI/IM 团队无 installation） | ✅ 已改：`team_id ↔ namespace` 恒 1:1；`installation_id` 可空、有值才断言；§9.2 负例同步限定 | §1 不变量 1 + §9.2 |
| 建议① | "私钥加密存储"落机制（Secret 默认 base64） | ✅ 已改为三项可验证约束：etcd encryption at rest（集群前提）+ RBAC/namespace 隔离 + 平台不保留副本；**不再承诺"加密存储"** | §4 存储行 |
| 建议② | 租户 server Pod 的 PriorityClass/PDB | ✅ 已实现：server 档 PriorityClass + PDB `maxUnavailable=0` | §3 + §9.6 |

### 待 owner 决定：team 服务端状态迁移（O-1）

weiye 给出的三选一（其倾向：**(c) 为 v1 默认 + (a) 作为显式选项**，(b) 列后续）：

- **(a) 全新开始**：显式迁移 = 在新租户**重建 team（空账本）**，旧 server 数据只读归档——必须作为**产品决定显式声明**，不能默认。
- **(b) 设计 team 状态导出/导入**（ledger + taskx + goal + members 一次性打包）——约一个 PR 量级，涉及 id/seq 连续性与引用完整性。
- **(c) v1 不迁移存量**：第二步只服务**新建租户**（console 创建即新世界），存量 step-1 member 永远留在旧 server（与 D11"不强制迁移"最一致，代价是长期双轨）。

**现状（代码事实，供 owner 决策参考）**：`tenant reissue-members` 已能完成 member 侧切换，但**不搬 team 状态**——即选 (a)/(c) 都不会因此断链（(a) 需要在控制台显式提供"新建团队"入口并标注空账本；(c) 则无需改动）。若选 (b)，需新增 `team export/import` RPC 与一致性校验。

## 10. 需求映射与开放项

| 需求 | 落点 |
|---|---|
| FR-2 每租户 server（容器/SQLite/PVC/独立 CA） | §3/§4 |
| FR-3 租户内建 project/team | §2/§5 |
| FR-13 回收（含 CA） | §2/§4/§9.1 |
| FR-16 平台 bootstrap 复用 006 token | §6 |
| FR-17 升级/回滚/版本 pin | §3 |
| NFR-2 身份边界（每租户 CA） | §4 |
| NFR-4 持久化（CA 入灾备） | §3/§4/§9.5 |
| NFR-9 审计独立 + NFR-13 平台可见边界 | §7/§9.6 |
| NFR-14 凭据分离 | §5/§6 |

**平台与租户的权威关系（评审建议）**：**平台 `users.id` 为权威源**（GitHub identity ↔ users.id 在平台库），
租户 server 只**引用**（member ↔ user 的绑定），避免双权威漂移 —— 这是 D8「三处锚点统一」的前提。
**M11 闭环声明**：第二步 server 入集群后，平台建租户所用的 kubeconfig 权限须收敛
（namespace 模板化 + 最小 RBAC，或 operator 模式），并声明**成员数据面不再依赖集群级凭据**。

**开放项（评审时定）** —— 以下 1–5 已由 owner 裁决（2026-09-22，v1 口径，可后续修订）：

1. ~~租户 server 的**端口/Ingress 暴露方式**~~ **裁决**：**每租户独立端点**（独立子域或独立端口），**TLS 透传（不终止）**，保留端到端 mTLS（每租户 CA，NFR-2）；平台统一入口仅服务平台面 API/console，**不代理成员 mTLS 流量**。
2. ~~平台 console 的技术栈~~ **裁决**：**复用现有 console（静态 + API 模式）**，v1 不新建 Next.js。
3. ~~租户 CA 的算法与有效期口径~~ **裁决**：**沿用现有 pki 默认**（v1 不做 per-tenant 配置）。
4. ~~双 CA 并存期的**最长窗口**（轮换 SLA）~~ **裁决**：**≤ 30 天**；窗口内完成成员证书换发（`tenant reissue-members` + Pod 重建）；超期废旧 CA 前必须满足 O-2 前置（旧 CA 有效证书数=0）。
5. ~~平台库（租户元数据/审计）自身的高可用方案~~ **裁决**：**v1 单副本 + PVC**，**纳入灾备清单**并补 console 重建 runbook（平台挂了全租户无法新建/回收）；HA（多副本/外部 DB）列后续。

**评审新开开放项（weiye task-92）**

6. **O-1 🔴 team 服务端状态的迁移** —— **已裁决（owner @ 2026-09-22）**：**v1 默认 (c)**（第二步只服务新建租户，存量 step-1 member 永久留旧 server，与 D11「不强制迁移」最一致，接受长期双轨）+ **(a) 作为显式选项**（控制台提供「全新开始/新建团队」入口，显式标注空账本；旧 server 数据只读归档）；**(b) team 状态导出/导入列后续**（约一个 PR，含 id/seq 与引用完整性），v1 不做。实现要求：(a) 入口须显式、不设默认迁移路径。
7. **O-2 CA 轮换的签发台账**：需 server 能按 CA 分代枚举自己签发的有效证书（否则"废旧 CA"无法证明
   前置条件"旧 CA 有效证书数=0"）→ 与轮换实现一并做。**owner 附注**：**轮换 v1 默认不启用**，直到 O-2 台账落地（否则无法证明废旧 CA 前置）。
8. **O-3 平台 service 证书的吊销/轮换**：当前为短 TTL + 每次重签（见 §6.5），未做显式吊销；
   若后续改为长期证书，须补吊销路径与审计。**owner 附注**：v1 维持短 TTL + 每次重签，不做显式吊销。
9. **O-4 GitHub OAuth 真实 token 交换**：接口与配置面已就位，callback 如实返回未完成（受限网络）；
   落地需可达 github.com 的环境。
10. **O-5 🔴 站外平台控制台 → 租户 server 的拨号路径**（真集群验证新发现）：Service 补上后，
    集群内 DNS/拨号已通，但 operator 本机跑的 `platform console` 直连不到 ClusterIP。
    候选：(a) 控制台进集群跑（Deployment）；(b) 经 kube-apiserver proxy 拨号
    （`/api/v1/namespaces/<ns>/services/<svc>:<port>/proxy/`，复用 kubeconfig 凭据）；
    (c) 每租户 NodePort/LB（暴露面，需安全评审）；
    **(d)（与开放项 1 裁决衔接）拨租户独立端点**：开放项 1 已裁决"每租户独立端点 + TLS 透传、
    保留端到端 mTLS"——service 车道可直接拨该独立端点（平台持租户 CA 签的 service 证书，
    端到端 mTLS 不变），**无需 ClusterIP 可达**。这是目前最顺的路径，待 owner 确认是否采纳为 O-5 主选。
    **未裁决前 service 车道在真集群不可达**，stub E2E 的 C7 负例保持 fail-closed。
