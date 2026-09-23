# Review: task-102 — Teamx Cloud 后端代码（租户 CA + O-2 签发台账 + service 车道）

- reviewer: weiye（co-lead，架构/评审；task-102，reviewer≠author 成立——作者 weiye-mini/并行会话）
- 对象：origin/main 上 Teamx Cloud 后端实现线（`dd9aebb` 租户 CA → `f02ce86` O-2 台账 → `aa981fc`/`38c35c2`/`54efbdc`/`e2ab392`/`1e43765` service 车道五连修 → `cccec3c`/`d180618`/`6d6c165` 平台面 Phase 1-3）
- date: 2026-09-23
- verdict: **CHANGES-REQUESTED（1 必改 + 1 安全可见性必改 + 1 排期项 + 2 建议；均为局部，方向与实现质量好）**

## 总评

这条实现线把 design/014 的三个核心裁决落成了**正确且可审计**的代码：

- **租户 CA（dd9aebb）**：env 探测（`TEAMX_TENANT_TEAM_ID` + Secret 挂载 `/data/ca`）→ invite 用租户 CA 签发、letter 内嵌租户 CA 链——正是 014 §4 的语义；平台在建租户时生成 CA 并写入 Secret（我 014 评审推荐的"平台建租户时生成"路径 ✅）。
- **O-2 台账（f02ce86）**：所有签发点落行（serial/CN/kind/member/team/**ca_fingerprint**/有效期/issuer），`tenant_ca_audit` 合并平台+租户双库按 CA 分代、报 live_count 与「旧 CA 可退役」——**直接实现了我 design/014 评审的必改②**（轮换前置可证明）。
- **service 车道**：`service:platform-console:console-<inst>` / `cli-<uuid8>` 双前缀审计可辨；短 TTL + O-2 落行 + args 强制带 team（拒 member_id 伪装）；`issued.list` 无 team 例外的**每笔都进 platform_audit** ✅；RPC 信封剥离 + 线程 panic 隔离 ✅。
- 平台面 Phase 1-3（密码登录→自助建 team→GitHub OAuth）超出本 task 范围（task-103 mini 评 console），抽查未见与后端矛盾。

## 必改（1）+ 安全可见性必改（1）

### 🔴 必改①：成员证书换发路径未落 O-2 台账（覆盖面缺口，危及轮换前置校验）

`98f8059`（design/014 §8 迁移 ①②③：成员证书换发）**没有任何 `issued::record` 调用**（该提交 grep 命中 0）。后果链：

- 换发证书（租户 CA 签发的新成员证书）不在 `issued_certs` 表里；
- 未来对该租户 CA 做轮换时，`live_count(ca_fingerprint=旧CA)` **漏计这些换发证书** → 「旧 CA 可退役」前置校验**假通过** → 废旧 CA 后，**已换发但未记账的成员全部失联**；
- 且审计不完整（谁在何时换了什么证书查不到）。

**改法**：换发路径补 `issued::record`（kind=member，ca_fingerprint=实际签发 CA，issuer 标 `reissue`）；顺手加一条单测断言「换发后 live_count(+1)」。

### 🟠 必改②（安全可见性）：租户模式下 CA key 缺失时，invite **静默回落实例 CA**

`dd9aebb` 的 fallback 只有 `eprintln!` WARN——stderr 在容器里没人看。后果：租户 server 在 Secret 缺失/挂载错误时会用**实例 CA** 签发成员证书，成员拿到的是实例 CA 链——租户隔离模型被静默换成"共享实例信任域"，与 M0（CNI 静默失效）**同判据：安全姿态的静默降级不允许**。

**改法（二选一）**：① fail-closed——租户模式（`TEAMX_TENANT_TEAM_ID` 非空）下 CA 材料缺失直接报错拒签；② 降级可用但**必须落审计事件 + 在 `team status`/node note 里可见**（与 D17 如实声明一致）。我的倾向：①（邀请是低频操作，失败可见比静默可用更符合本设计的整体姿态）。

## 排期项（1，给 owner）

### 🟠 M0/G2/A 三个 PASS 对象（task-101）尽快合并

Teamx Cloud 已在真集群跑 Phase 1-4（平台控制台、GitHub OAuth、service 车道都已部署），而 **main 的 k8e provision 路径还没有 CNI 前置校验**（354a560 不在 main）——当前任何真集群部署都必须**显式设 `TEAMX_K8S_CNI_DECLARED`**，否则 §7 的 deny-all egress/封 IMDS 未经验证。三个对象已全部 PASS（task-101），建议本轮就合（顺序 B→G2→A）。

## 建议（2，非阻塞）

1. **`issued revoke` 的语义边界写进 design/014**：revoked 证书**不会在 mTLS 认证时被拒**（serve 认证路径不查 `issued_certs`，已核：serve.rs 唯一 `issued::` 引用是 issued.list RPC）——密码学拒绝靠**废 CA**（信任 bundle 移除）；per-cert revoke 是轮换前置的**记账**。建议后续在 serve 认证时查 revoked serial（缓存集合即可，一个查询的代价），或至少在 CLI help/文档里把"记账性吊销"说透。
2. 「私钥加密存储」的措辞沿用我 014 评审建议：K8s Secret 默认 base64 非加密——要么声明启用 etcd encryption at rest，要么改为「RBAC + namespace 隔离约束下的存储」。

## 覆盖面与质量（好的部分，供合并参考）

- `issued::record` 现有 6 个调用点覆盖：invite ✅ / clone 预签 ✅ / platform-cli service cert（短 TTL）✅ / 租户 server 自身证书与 platform-console 初始证书（provision.rs 三处）✅ / **换发 ✗（必改①）**。
- service 车道的异常路径处理成熟：信封剥离兼容直调形状（e2ab392）、线程 panic 隔离（1e43765）、"无 team 按 CA 查"分支的审计（38c35c2）、撤掉自动补 team 的判断（54efbdc）——五连修每一条都有真集群踩坑背景，质量好。
- Phase 1-3 平台面与后端的边界（平台 users.id 权威、租户引用）符合 014 评审建议 ✅。

—— weiye（co-lead · 架构/评审，task-102）
