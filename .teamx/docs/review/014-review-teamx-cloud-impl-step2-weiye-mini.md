# Review: design/014 Teamx Cloud 第二步实现设计（每租户 server + 独立 CA + 平台控制台）

- reviewer: weiye-mini（非作者；作者 caishu/owner）
- target: `.teamx/docs/design/014-teamx-cloud-impl-design-step2.md` @ 当前台账（132 行，state=draft）
- task: task-93
- date: 2026-09-22
- verdict: **CHANGES-REQUESTED（2 必改 + 1 重大缺口 + 3 建议）**

## 总评

结构完整（三元组/租户 server/独立 CA/平台控制台/bootstrap/迁移），§4 独立 CA 的轮换/吊销/灾备/边界四件套齐全，§7 安全与可见边界与 D10/NFR-13 完全对齐（封闭枚举 fail-closed + break-glass 全要素 + 审计独立），§9 验收 8 条可执行。**但有两个必改与一个重大缺口**——其中迁移证书换发缺失会导致第二步上线即断链。

## 核验通过

- 不变量四条 ✓（1:1:1 / 独立 CA+灾备 / D10 fail-closed / 不破坏第一步）
- §3 每租户 server：Deployment 单副本 + Recreate（SQLite 单写者语义正确）/ CA 首次生成写回 / 版本 pin + 回滚 / 灾备含 CA ✓
- §6 bootstrap 复用 006 一次性 token（不另造协议）+ App 凭据口径与 #8 一致 ✓
- §9 验收 8 条（三元组负例/CA 轮换/吊销/灾备/可见边界/升级回滚/bootstrap）可执行 ✓
- §10 需求映射 9 项全覆盖 ✓

## 必改

### 必改 1：§2 namespace 命名违反 M5（013 v3 已裁决全长/≥16 hash）

§2 表写 `namespace: tenant-<team_id 前 8>`——**这正是 013 v3 评审 M5 判死的前 8 字符截断**（32bit 熵，第二步 namespace 是**安全边界**，截断碰撞 = 跨租户混住）。013 v3.1（task-86 修正、verified @ 7094）已裁决「全长或 ≥16 字符 hash」。**改为 `tenant-<team_id 全长或 ≥16 字符 hash>`**，并同步 pod/pvc/secret 命名口径（同前 8 截断在 namespace 内碰撞风险低，但统一口径防漂移）。

### 必改 2：§8 迁移缺「成员证书换发」——第二步上线即断链

§8 说「member Pod 重建后 `server_url` 切到本租户 server 生效」——但 member 手里的证书是**旧平台 CA 签发的**，切到新租户 server（新独立 CA）后**旧证书无法通过 mTLS 验证，member 直接失联**。迁移路径必须包含：
1. 新租户 server 就绪后，用**租户 CA** 为该租户成员**签发新证书**（经 Secret 注入 → member Pod 重建时 import，复用 013 §4 setup 机制）；
2. `server_url` 切换与证书换发**同批生效**（先后会失联）；
3. 旧 server 保留至新链路验证通过（对应 D11 源保留原则）。
缺此三步，§9.1「租户全链」验收无法通过。

## 重大缺口：平台 console ↔ 租户 server 的认证通道未设计

§5 平台 console 的功能面包含「邀请成员 / 一键 provision / 查看用量」——这些操作要**调租户 server 的 RPC（mTLS）**。问题：**平台 console 用什么身份连租户 server？**
- 平台不是成员（无成员证书）；若租户 CA 给平台签一张**平台服务证书**，则平台进入租户信任域——这与 §4 边界「console 不得做成信任所有租户 CA 的角色」方向相反，需要明确豁免与最小化（证书只授白名单方法）；
- **现成答案**：这正是 t1-im-client 的**二维服务白名单**（`service_kind → allowed_methods`，012/我实现的 task-60）设计的场景——平台 console 作为 **service 车道**（新 service_kind=`platform-console`），证书由租户 CA 签发、方法白名单 fail-closed（仅 tenant. 元数据读写 + 成员邀请/provision 触发），配额与审计在 serve 侧。**建议 §5/§4 补一节「平台服务证书 + 二维白名单车道」**，并引用 013 M2 的 serve 侧闸门原则。

## 建议（非阻塞）

1. **M11 消除的闭环声明**：013 §3 承诺「第二步 server 入集群后消除 kubeconfig 跨租户信任域」——014 应显式写：平台 console 建租户用的 kubeconfig 权限收敛（namespace 模板化 + 最小 RBAC，或 operator 模式），并声明**成员数据面不再依赖集群级凭据**。
2. **§3 「滚动升级」措辞**：`strategy: Recreate` 是**停机重建**非零停机滚动——建议改「重建式升级（短暂下线 + 恢复断言）」，与 §9.7 断言口径一致。
3. **users 双层关系**：平台库（users.id ↔ GitHub identity）与租户 server 库（member ↔ user）的关系应写明——**平台 users.id 为权威源**，租户 server 引用（避免双权威漂移，D8 三处锚点统一的前提）。
4. **双 CA 并存窗口的实现**：server TLS 需同时信任新旧 CA（trust bundle 拼接）——建议 §4 补一句实现方式，防实现时漏配。

## 开放项补充（§11）

6. **平台 console 自身的 HA 与灾备**（§11.5 提了平台库单副本）——建议补：平台库也进灾备清单 + console 重建 runbook（平台挂了全租户无法新建/回收）。
