# 评审：design/006 invite 时开通云主机（server 侧 AWS CLI）

- reviewer: xiaods（contributor，外部/成本视角；非作者，作者 caishu/owner）
- target: `.teamx/docs/design/006-invite-provision-node.md` @ 台账 HEAD `f0727ae`（+ `.meta.json`；含 D–V 裁决与 §3.1/§5.0/§5.1）
- task: review-design-006-xiaods（high）
- 裁决基线：#6 owner 裁决 `#4101`；owner 补充裁决 seq 4934（serve 单中心 + member node + AWS 先行）；006 更新裁决 D–V、§3.1、§5.0
- date: 2026-09-19
- verdict: **PASS（含 5 项必改——均为加法澄清/口径锁定，不动模型；与 caicai 3 项、weiye-mini 2 项互补不重复）**

## 结论

模型正确（owner/lead 触发、server 持 owner 凭据执行、server 持 per-node SSH 私钥），默认关/异步/A/B/C 与沿用裁决及 D–V 均落地，幂等/状态机/安全清单质量高。当前版已含 caicai 3 项与 weiye-mini 2 项必改（逐项核对在位，见下）。本评审补充 5 项两位未覆盖的缺口（计数笔误、示例矛盾、TTL 执行者、approve 触发链、磁盘口径 owner 输入），补齐后可进实现。建议通过。

## 核验点（对照任务 6 点）

1. 模型：PASS。触发者 owner/lead ✓；执行者 teamx server（AWS CLI + owner 凭据）✓；server 持 SSH 私钥是“server 创建机器”的直接推论 ✓；与 §7（serve 单中心、被开机器是 member node）一致 ✓。
2. 已定裁决：PASS。默认关 ③ ✓；异步 ④ ✓；A（instance profile 优先，否则 access key 加密存）✓；B（仅 Run/Terminate/Describe + keypair + 标签，无通配）✓；C（per-node keypair + server 加密存）✓；沿用（S/TTL30d/auto-stop/单 user 1 台/destroy/region 白名单/加密轮换审计/熔断 T3/计量）✓。
3. 新增裁决 D：PASS（最便宜 + 下限 1C2G + 可 `--size` 覆盖已落字）；E–V 逐条有落点 ✓（E ARM 优先 / F region 指定+推荐 / G SG 仅 server IP / H 软件清单 / I Ubuntu arm64 不预装 / J 磁盘 / K 标签 / L KMS / M 幂等 / N 计量 / O SSH 直推 / P 两级 auto-stop / Q 1C2G / R 同 region 缺省 / S 无 EIP / T 官方脚本 / U 失败通知不自删 / V approve 后启动）。
4. 流程步骤：PASS 框架，计数笔误与两处断裂见必改 2/4/5；步骤与 §3.1/§5.1 引用一致 ✓；失败边（超时重试上限 + 失败态 + 通知）✓；host-key 指纹校验 ✓。
5. 安全：PASS 框架。server 双持高价值目标定性 + per-node keypair + 加密/轮换/审计 + IAM 最小 ✓；destroy 清单含 EIP/安全组/DNS/keypair/secret/证书/邀请/快照/OS 用户 + 双负例 ✓。
6. 数据/幂等/状态机：PASS。三表（含 `os_user`/`keypair_ref` 必填）✓；idem=(team_id,member_id) + 部分唯一索引 + INSERT OR IGNORE ✓；复用优先（§3.1，同 user 查 nodes）✓；7 态 + 迁移落台账 ✓。

## 同行评审覆盖确认（不重复）

- caicai 3 项：① workspace/repo 拉取 → 步骤 8 在位 ✓；② destroy EIP/安全组/DNS + `os_user`/`keypair_ref` → §6/§4 在位 ✓；③ 超时失败边 + host-key → §3 失败边/步骤 5 在位 ✓。
- weiye-mini 2 项：① 复用优先 → §3.1/§7 在位 ✓；② auto-stop 两级粒度 → 裁决 P 在位 ✓。
- 以下 5 项为两位未覆盖的独立增量。

## 必改（5 项）

1. **磁盘口径锁定（owner 输入）**：裁决 J 现为“`gp3`，默认 20–30 GiB（可调）”。owner 输入（2026-09-19）：**默认 30GiB gp3，不用可调**。请改 J 为固定值；`--size` 保留给机型（D/Q），磁盘不再参数化（简化计费与快照策略）。
2. **步骤计数笔误**：§3 标题“流程（9 步）”下实为 10 条。请改标题为 10 步（或合并 Annotations），防后人按“9 步”验收漏第 10 条（回写 nodes + 台账恰是最关键的可观测步骤）。
3. **§3.1 示例与用户名规则矛盾**：§3.1 规则为 `<team-slug><4位随机数字>`（如 `abc1234`），但步骤 7 与 §3.1 首段示例写“team `abc` → OS 用户 `abc` + `/home/abc`”。请统一示例为规则形态（如 `abc1234` + `/home/abc1234`），身份语义处不容一例两制。
4. **TTL 到期执行者缺失**：K 标签打了 TTL、`沿用` 写了“TTL 30d 可续”，但全流程无“到期谁执行 terminate/续期”的主体（sweeper 定时任务？续期入口命令？到期前通知？）。请补一段：到期扫描者 + 续期方式 + 到期默认动作（terminate 还是转 stopped），否则 TTL 标签落了无人执行。
5. **V 的 approve→启动触发链缺失**：步骤 9 要求“等 owner approve 后才启动”，但 approve（`membership.approved` 台账事件）到节点 systemd 启动之间无触发机制。请写死二选一：server 监听 approve 事件后 SSH 启动，或节点 bootstrap 后轮询 membership 状态；并明确 pending 期间已装好的服务处于什么状态（disabled 非 active）。

## 建议（非阻塞）

1. U 的“失败不自动 terminate”有成本泄漏面：failed 实例持续计费直到 owner 手动处理。建议加保留期（如 failed 超 7d 自动 terminate，快照保留），或失败通知附带一键清理入口。
2. G/S 的 server 出口 IP 漂移：若 server 非 Elastic IP，NAT/重启换 IP 即锁死 SSH（caicai 建议 3 同源）。建议 §2-G 补一句（Elastic IP 优先，或 SG 自更新 job）。
3. O 与一次性 token 路径的分工（caicai 建议 4）§8.2 已 deferred 到 #8——确认即可，无需本设计展开。
4. 凭据轮换流程（caicai 建议 5）与 #8 secret 轮换同口径——确认纳入实现约束即可。
5. ttyd/tx-expose 的公网可达面未定义（§5.1 列了组件但没写经 tunnel 还是直连）。建议补一句“经 `teamx tunnel` 暴露，不直接公网监听”，与 G（无公网入站）自洽。

## 缺失测试 / 风险

- 实现测试矩阵（供 t1-impl-test 引用）：TTL 到期 terminate/续期；approve 后服务启动（pending 期 disabled 断言）；failed 保留期；复用语义（同 user 两 team 只加 OS 用户）；destroy 双负例；host-key 篡改拒。
- 本机有 cargo 但属 doc-only 设计评审，以裁决对照 + 可实现性核验为准，未跑代码测试。
- reviewer≠author 满足（作者 caishu/owner）。
