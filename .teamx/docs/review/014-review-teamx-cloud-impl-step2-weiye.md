# Review: design/014 — Teamx Cloud 第二步实现设计（每租户 server + 独立 CA + 平台控制台）v2

- reviewer: weiye（co-lead，架构/评审；task-92）
- target: `.teamx/docs/design/014-teamx-cloud-impl-design-step2.md` v2（作者 caishu，reviewer≠author ✅）
- date: 2026-09-22
- verdict: **CHANGES-REQUESTED（1 重大缺口 + 3 必改 + 2 建议；均为局部，不动骨架）**
- 说明：文档 meta 已标 `approved`（owner 指示不等最后一位评审）——本评审**不改变状态**，但必改 1/2 直接影响实现顺序与数据安全，建议**在对应实现启动前**落进 design/012↔014 修订。

## 总评

v2 已把第一轮评审意见全部落实（M5 命名熵、§8 证书换发三步、§6.5 平台 console 认证通道（复用我的 P0b 二维白名单）、双 CA trust bundle、M11 kubeconfig 声明、users 双层权威、平台 HA）。三元组、CA 边界、平台可见性 fail-closed 都成立。以下是**对抗视角**下仍存在的缺口。

## 重大缺口（1）

### 🔴 迁移的另一半没有设计：team 的**服务端状态**怎么办

§8 只设计了「member 侧」的迁移（证书换发 + `server_url` 切换 + 工作区数据走 D11-A）。但一个 team 的**服务端状态**——ledger 全史、goal、tasks/taskx、discussions、members 表、tunnels 注册、IM 绑定——**全部在旧平台的 SQLite 里**，而新租户 server 是一个**全新空库**。

- 若「用户显式迁移」= member 切到新租户 server：**该 team 在新 server 上不存在**（team/members/goal/任务全空）——成员签到的是一个"失忆"的租户。这不是边缘 case，是迁移定义本身。
- 全仓**不存在** team 状态的导出/导入能力（CLI/RPC 均无；ledger 不可导出为可导入格式）。
- 三个可选出路（需 owner 拍板，设计需择一并写清）：
  - **(a) 全新开始**：显式迁移 = 在新租户重建 team（空账本），旧 server 数据**只读归档**——必须作为产品决定显式声明，不能默认；
  - **(b) 设计 team 状态导出/导入**（ledger + taskx + goal + members 的一次性打包，类似 #98 时代讨论过的台账一致性口径）——工作量一个 PR 量级，但涉及 id/seq 连续性与引用完整性；
  - **(c) v1 不迁移存量**：第二步只服务**新建租户**（console 创建即新世界），存量 step-1 member 永远留在旧 server（与 D11「不强制迁移」最一致，代价是长期双轨）。
- 我的倾向：**(c) 为 v1 默认 + (a) 作为显式选项**，(b) 列后续。但这是 owner 的产品决定。

## 必改（3）

### ① platform-console service 证书的**签发路径**未定义 🔴

§6.5 定了信任方向（租户 CA 签发平台 console 的 service 证书——方向正确，我认可），但**没有定义发放管道**：

- CA 由谁生成？§4 说「缺失则生成并写回」＝**租户 server 首启自生成**——那么 **CA 私钥在租户 server 手里，平台拿不到**，平台就无法给自己签 `platform-console` 证书（鸡生蛋）。
- 若改为「平台在建租户时生成 CA」（平台经 kubeconfig 写 Secret）——则 §4 的「缺失则生成」要改，且平台短暂经手 CA 私钥（写入 Secret 的那一刻），需说明合规口径。
- 若由 server 提供签发端点——需要一个新的受保护 RPC（bootstrap token 门控），又是一块新面。

**三选一必须写明**，否则 §6.5 落地时现场发明。我的倾向：**平台在建租户时生成 CA 与 platform-console 证书**（同一次 kubeconfig 操作，Secret 全部落租户 namespace；平台不保留 CA 私钥副本——写完即忘），语义最干净。

### ② CA 轮换的**成员侧换发触发器**未定义 🟠

§4 轮换 = 「生成新 CA → server 重载 → 成员证书**逐步换发** → 废旧 CA」。但 member 证书是**静态 letter**（013：Pod 创建时 import，无在线续期机制）——「逐步换发」靠什么发生？

- 现实只有一条路：**逐个重建 member Pod**（PVC 保留，重走 setup）。那么「废旧 CA」的前置条件必须是「**该 CA 签发的全部有效证书已换发完毕**」——这需要 server 能枚举自己签发的证书（按 CA 分代的序列号台账），**目前没有**。
- 必改：① 定义换发触发（如"轮换开始后，member Pod 逐个滚动重建"的 runbook，或 server 在成员下次连接时下发"请重导入"标记）；② 「废旧 CA」加前置校验（旧 CA 签发的有效证书数 = 0，可从服务端证书台账断言）；③ 写明换发期间**成员会话会中断一次**（重建语义）。

### ③ 三元组不变量对「非 GitHub 来源租户」不成立 🟠

不变量 1 说「任何一层缺失/重复都是非法状态」，但 `installation_id` 来自 GitHub App——**CLI 创建的团队、IM 来源的团队（#22）没有 installation**。若不变量按字面执行，这些合法租户全部非法。

**必改**：不变量改为——`team_id ↔ namespace` **恒 1:1**；`installation_id` **可空**（非 GitHub 来源租户），有值时才参与 1:1 断言。§9.2 的一致性负例同样加限定（仅对 GitHub 来源租户断言 installation 维度）。

## 建议（2，非阻塞）

1. **「私钥加密存储」落机制**：K8s Secret 默认只是 base64（etcd 明文）。要么声明启用 etcd encryption at rest（集群级前提，写进 013 的 CNI 前置同款清单），要么把「加密存储」改为「RBAC + namespace 隔离约束下的存储」——别让"加密"二字成为没有实现的承诺。
2. **租户 server Pod 的 PriorityClass/PDB**：013 v3 的 M7 给了 member/server 两类工作负载分档；第二步的**租户 server Pod**（单副本、扛全租户控制面）必须用 server 档（高优先 + PDB maxUnavailable=0）——§3 只写了 Recreate，补一句调度保护。

## 与 010–013 的衔接核对（一致性 ✅）

D2 立场落地 ✅ · C11 三元组 ✅ · C15 平台 bootstrap 复用 006 token ✅ · 013 的 §4 setup/PVC/Secret 复用 ✅ · §6.5 复用 013 时代的二维服务白名单（P0b）✅ · D11 不强制迁移 ✅ · D10 fail-closed 元数据 ✅。**未见第二步被第一步锁死**；§8 的证书换发/同批切换设计正确（这正是我第一轮担心的断链点，已闭环）。

## 结论

**CHANGES-REQUESTED**：重大缺口 1（team 状态迁移）+ 必改 3（签发路径 / 换发触发器 / 三元组限定）。均为局部修订，不动骨架；其中重大缺口需 owner 先做产品决定（(a)/(b)/(c) 三选一），其余两条可直接落 design 修订。

—— weiye（co-lead · 架构/评审，task-92）
