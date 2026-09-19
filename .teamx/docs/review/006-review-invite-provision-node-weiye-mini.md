# Review: design/006 invite 时开通云主机（server 侧 AWS CLI 执行）

- reviewer: weiye-mini（非作者；作者 caishu/owner）
- target: `.teamx/docs/design/006-invite-provision-node.md` @ ff40167（state=review，含 D–O 全部裁决与 §3.1/§5.0/§5.1 更新）
- date: 2026-09-19
- verdict: **PASS（含 2 项必改——均为模型层缺口，与 caicai 的 3 项必改互补不重复）**

## 结论

模型正确、裁决落地完整、幂等/状态机/安全清单质量高。caicai 必改 3 项（补 workspace/repo 步骤、destroy 补 EIP/安全组/DNS、超时失败边+host-key 校验）我全部附议。本评审补充 **2 项 caicai 未覆盖的模型层缺口**（多 team 复用同一主机的语义、auto-stop 粒度），补齐后可进实现。

## 核验点

1. **模型正确性**：PASS。触发者 owner/lead（与治理铁律一致——agent 不自行 provision）；执行者 server 持 owner AWS 凭据 + aws cli；server 持 per-node SSH 私钥的推论成立（§6 已把 server 列为高价值目标并给出对冲）。
2. **已定裁决落地**：PASS。默认关/异步 job/A instance profile 优先/B IAM 仅 RunInstances+Terminate+Describe+keypair+tag 无通配/C per-node keypair+加密存储——逐条与 #6 裁决一致；沿用清单（S 规格/TTL 30d/auto-stop/destroy/region 白名单/熔断 T3/计量）逐字引用无漂移。
3. **新增裁决 D（最便宜机型·下限 1C2G）**：落地 ✓；见建议 4 的实测数据点。
4. **幂等/状态机**：C4 模式复用（部分唯一索引 + INSERT OR IGNORE + 冲突回读）✓；状态机 7 态 + 每迁移落台账 ✓。
5. **§5.0 补开（node provision --member）**：设计合理——与 invite 同路径同幂等键，ready 直接返回/terminated 可重开；见必改 1 对复用语义的补全。
6. **§3.1 隔离模型**：一主机一用户多 team 实例 + 每 team 实例独立 OS 用户 + systemd 每实例一套——隔离收益与回收语义清晰；OS 用户名 `<team-slug><4位随机>` 冲突重生成规则可落地。**但该模型与幂等键/上限条款存在两处未调和（见必改）**。

## 必改（模型层，caicai 未覆盖）

### 必改 1：「一主机多 team 实例」与幂等键/上限的复用语义未定义

- §3.1 说「一台云主机 ↔ 一个用户（人），可跑多个 team 实例」；沿用裁决说「**每 user 常驻上限 1 台**」；但幂等键 `idem_key=(team_id, member_id)` 只能防「同 team 重复 invite」，**不表达「同 user 已有 ready 主机时应复用」**。
- 后果：同一人被两个 team 各自 `invite --provision-node` 时，两个 job 各自通过各自的幂等键检查 → **开出两台主机**，直接违反「每 user 1 台」。
- **修法**：provision job 入队前先按 `user_id` 查 `nodes` 表——已有 `ready/degraded/stopped` 节点 → **复用该主机**（仅新增该 team 的 OS 用户 + systemd 实例，job 状态直接进 bootstrapping）；无 → 新建主机。`terminated` 才允许新建。文档 §3/§4 各补一段「复用优先于新建」的判定顺序，并在 `nodes` 表确认 `user_id` 上可建查询（现表有 user_id 列，够用）。

### 必改 2：auto-stop 粒度与多 team 实例冲突

- 沿用裁决「空闲 auto-stop（CPU<5% 30min）」未定义粒度。§3.1 之后一台主机跑多个 team 实例——**整机级 auto-stop 会殃及同机其他 team 的活跃实例**（team A 空闲但 team B 正在干活，30min 后整机停机 = B 被误杀）。
- **修法**：明确两级粒度——**实例级** idle 判定按该 team 实例自身 activity（CPU 计到 systemd slice 或按 member activity 记录），停的是该实例的 systemd 服务组；**整机级** stop/terminate 仅当该主机上**所有**实例都满足 idle 条件。§2 沿用清单处补一句粒度定义即可。

## 建议（非阻塞）

1. **opencode 安装方式未定**（步骤 6「装 opencode ≥1.18.0」）：opencode 不在 apt 源；arm64 Linux 需官方安装脚本或 GitHub release 二进制 + PATH 软链 + **版本锁定与 checksum 校验**（我在 muse-egress 任务实做过同款安装：brew 不可用，二进制 + 软链是可行路径）。建议 §5.1 补安装来源与锁定版本。
2. **CA/server URL 分发**：步骤 7 import 邀请函需要节点知道 `TEAMX_SERVER_URL` + `ca.crt`——§5.1 列了 `~/.teamx/` 但没写 server URL/CA 如何随 SSH 推送分发，建议步骤 7 明示一并推送。
3. **systemd 依赖顺序**：unit 模板建议声明 `After=opencode-serve` / worker `Wants=` 依赖 + `Restart=always`，防启动竞态（worker 先于 opencode-serve 起来会空轮询）。
4. **sudo 白名单**：server 经 SSH 用 `ubuntu+sudo` 自动化——server 被攻破即所有节点 root（§6 已认知）；建议 sudoers 限定可执行命令清单（useradd/systemctl/apt 等），缩小爆半径。
5. **1C2G 实测数据点**：headless `opencode serve`（bun/node 运行时）+ worker 同机常驻，1C2G 是下限口径——建议标注「实际建议 2C4G 或至少配 swap」，具体以试点实测为准（可复用现网 caicai 主机的多实例内存数据）。
6. **安全组动态更新**：G 的「SSH 只允许 server 出网 IP」要求 server 出网 IP 稳定——server 若用弹性 IP 则安全；若 NAT 后出网 IP 会变，需定义「server IP 变更时同步安全组规则」的机制，建议 §2-G 补一句（弹性 IP 优先）。
7. **im-gateway 常驻（可选）**：与我 T1 文档（t1-im-client §3 通道层）衔接一致——im-gateway 单连接硬规则（app_id↔gateway 1:1）与租约互斥是 serve 侧逻辑，节点侧只需 systemd unit + 凭据存储前置，两文档无冲突。

## 缺失测试与风险

- 测试清单建议（进 t1-impl-test 总装）：**复用语义**（同 user 两 team invite→第二台不开、只加 OS 用户）；**auto-stop 粒度**（A 实例 idle 不停 B 实例）；**幂等双 invite 并发**（C4 既有）；**destroy 可验证清单负例**（caicai 已列）。
- 风险：server 集中持凭据的爆半径已被 §6 认知并对冲（加密/轮换/审计/per-node keypair/IAM 最小权限），残余风险可接受。
- 流程：本文与 `004-t1-provider-external` 的修正（拓扑歧义）已闭环（§7 明示修正关系）；两文档无冲突。
