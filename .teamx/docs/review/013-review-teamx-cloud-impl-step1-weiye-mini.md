# Review: design/013 Teamx Cloud 第一步实现设计（provider=k8e + member 容器化）

- reviewer: weiye-mini（非作者；作者 caishu/owner）
- target: `.teamx/docs/design/013-teamx-cloud-impl-design-step1.md` @ 2c6907c（141 行，state=draft）
- date: 2026-09-21
- verdict: **PASS（含 2 必改 + 4 建议）**——总体质量很高，抽象映射表/生命周期/安全清单均可直接实现；2 处内部矛盾需澄清后合入

## 总评

§2 抽象映射表清晰（006→k8e 逐项对照）、§5 生命周期映射优雅（approve 后才建 Pod 天然满足裁决 V）、§7 安全清单全面（PSA/seccomp/NetworkPolicy/封 IMDS/Secret 短 TTL）。§10 需求映射完整（FR-6/7/8/9/10/12/13/14/15/18 + NFR-1/10/11/12 全覆盖）。与我的 t1-im-client P0b/P1 设计（二维白名单→K8s SA、唤醒路径复用 FR-5）无缝衔接。

## 核验点

1. **不变量四条** ✓：server 不持私钥（D6 精神）、一 team-member 一 Pod（D1=B）、/workspace 唯一事实源（D3）、approve 后启动（裁决 V）
2. **命名规范** ✓：含成员短 id 防碰撞（D6 子约束 2）+ labels 含 user_id（D8 三处 users.id 锚统一）
3. **安全清单** ✓：PSA restricted / seccomp / readOnlyRoot / runAsNonRoot / NetworkPolicy deny-all egress + allowlist + 封 IMDS / 无 RBAC / Secret 短 TTL——与 NFR-1 完全对齐
4. **§10 需求映射** ✓：11 FR + 4 NFR 全有落点，无悬空裁决

## 必改

### 必改 1：§3 create 与 §5 生命周期的矛盾（Pod 何时创建）

§3 `create()` 说 "apply PVC + Secret + **Pod**（默认 `suspend`）"，但 §5 说 provision 阶段 "**不建 Pod**"、approve 才 "创建 Pod"。两者矛盾：
- 如果 Pod 在 provision 时就创建（suspend），则 §5 的"不建 Pod"应改为"Pod 已创建但 suspend/不启动服务"
- 如果 Pod 在 approve 时才创建，则 §3 create() 不应包含 Pod（只返回 PVC/Secret 的确认）

**建议**：明确选一个。我推荐 **approve 后才创建 Pod**（§5 方案）——理由：① Pod 尚不存在时"pending 期间不启动服务"**天然成立**（设计原文已指出）；② 避免 suspend 语义的边缘 case（K8s 对 suspend Pod 的 GC/调度行为不如"不存在"干净）；③ provision 失败只需删 PVC+Secret（无 Pod 残留）。

### 必改 2：§3 `K8sProvider` 的 `namespace_for: fn(team_id) -> String` 应为可注入配置

函数指针 `fn(team_id) -> String` 意味着命名空间映射在编译期硬编码——但 C11（1:1:1）要求 namespace 从 installation/team 映射中动态获取（K8s ResourceQuota/NetworkPolicy 也按 namespace 施加）。**建议**：改为 `Box<dyn Fn(&str) -> String + Send>`（或配置结构体），并注明 v1 默认实现为 `tenant-{team_id 前 8}`（与 §3 命名规范一致），第二步从 installation 映射表读取。

## 建议（非阻塞）

1. **initContainer 幂等性加固**：§4 说 "已 import/已 clone 则跳过"——但如果 initContainer 在 git clone 中途崩溃（Pod 重启），PVC 上有半拉取状态。建议加**锁文件**（`.teamx/bootstrap.lock`，import/clone 完成后删除；存在且非空则走修复路径而非跳过）。
2. **ResourceQuota + LimitRange 缺失**：§7 提了 NFR-1 的 NetworkPolicy 但没提 namespace 的 ResourceQuota/LimitRange manifest——D14 裁决「K8s 层兜底」的落点就在这里。建议 namespace 创建时一并 apply。
3. **`ip_of` 返回值**：§3 说 "返回 Service DNS + Pod IP"，但 §11.5 说 "第一步是否需要 Service" 是开放项——如果 v1 不建 Service，`ip_of` 返回什么？建议明确：v1 返回 Pod IP（集群内可达），Service 留第二步。
4. **§2 映射表补一行**：006 的「按 user 复用主机」（§3.1）在 k8e 路径 **不适用**（D1=B）——006 评审时我提的这条关键语义在 §2 映射表中漏了，建议补一行防止实现方按 006 语义复用主机。

## 缺失测试

- **PVC 持久化断言**：Pod 重建后 `/workspace` 文件清单一致（§9.3 已覆盖，补充断言细节：文件数 + 抽查哈希）
- **initContainer 幂等性**：Pod 重启（非首次）→ setup 容器跳过已完成的步骤（import/clone 各断言）
- **NetworkPolicy 白名单负例**：Pod 出网到非 allowlist 目标 → 超时/拒绝（非 200）
