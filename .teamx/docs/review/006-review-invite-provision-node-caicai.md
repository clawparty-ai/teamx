# 评审：design/006 invite 时开通云主机（server 侧 AWS CLI）

- reviewer: caicai（contributor，实现/测试视角；非作者，作者 caishu/owner）
- target: `.teamx/docs/design/006-invite-provision-node.md` @ 台账 HEAD `ff40167`（+ `.meta.json`）
- task: review-design-006-caicai（high）
- 裁决基线：#6 owner 裁决 `#4101`（默认不开/invite 异步/provision 便利层/熔断 T3/一次性 token/destroy 清单）；owner 补充（seq 4934：serve 单中心，被开机器是 member node；AWS CLI 先行，k8e 后议）；006 更新裁决 E–O、§3.1、§5.0。
- date: 2026-09-19
- verdict: PASS（含 3 必改，均为加法澄清，不动模型）

## 结论

模型正确，9 步流程、§3.1 隔离模型、§5.0 补开、裁决 D–O 均与 #4101 及补充裁决一致。3 处必改均为加法（补步骤/字段/失败边），落实后可进实现。建议通过。

## 核验点（对照任务 6 点）

1. 模型：PASS。触发者 owner/lead ✓；执行者 teamx server（持 owner AWS 凭据 + aws cli）✓；server 持 SSH 私钥是“server 创建机器”的直接推论 ✓。与 §7（serve 单中心、被开机器是 member node 非 serve）一致。
2. 已定裁决：PASS。默认关 ③ ✓（§2 + 流程步 2）；异步 ④ ✓（invite 立返，job 独立状态机/重试/台账）；A ✓（instance profile 优先，否则 access key 加密存）；B ✓（仅 Run/Terminate/Describe + keypair + 标签，无通配）；C ✓（per-node keypair + server 加密存）；沿用（S/TTL30d/auto-stop/单 user 1 台/destroy/region 白名单/加密轮换审计/熔断 T3/计量）✓。“一台云主机↔一用户 + 多 team 实例”与“每 user 常驻上限 1 台”一致 ✓。
3. 新增裁决 D：PASS（最便宜 + 下限 1C2G + 可 --size 覆盖已落字）。E/F/G/H/I/J/K/L/M/N/O 逐条有落点 ✓。
4. 9 步流程：基本 PASS，缺 1 步（见必改 1）；步骤与 §3.1/§5.1 引用一致 ✓。
5. 安全：PASS 框架。server 双持（AWS 凭据 + SSH 私钥）的高价值目标定性正确；per-node keypair + 加密 + 可轮换 + 全审计 + IAM 最小 ✓；destroy 清单 6 项 + 双负例 ✓（与 #62 式吊销演练同源）。缺 EIP/安全组清理（见必改 2）。
6. 数据/幂等/状态机：PASS。nodes/provision_jobs/node_secrets 三表 ✓；idem=(team_id,member_id) + 部分唯一索引 + INSERT OR IGNORE（C4 复用，覆盖并发）✓；状态机 7 态 + 迁移落台账 ✓。

## 必改（3 项，加法）

1. **9 步缺 workspace/repo 拉取**：member import 入队后没有工作区——原 #6 提案有“拉团队 repo + workspace”，006 流程（步 7–8）丢了。请补一步（server SSH 直推 repo clone，或声明复用 token-bootstrap 既有步骤），否则 worker 上线即空转。
2. **destroy 清单 + nodes 字段**：(a) 清单补 **EIP/公网 IP 释放** +（若安全组按节点建）**安全组规则清理** + **DNS 记录**（若有）；(b) nodes 表补 **`os_user` 与 `keypair 引用` 字段**——§3.1 用户名随机（重生成）、§5.0 terminated 重开、destroy 删用户/吊销都依赖这两列落盘，否则实现时无处可查。
3. **失败边与 SSH 信任**：(a) 步 4/5/6 补**超时与重试上限**（等 ready/SSH 首登/安装三处无界等待会卡死 job；job 表已有 attempts/last_error，请写死上限、失败迁移与 owner 通知）；(b) 步 5 补 **SSH host-key 指纹校验**（用云侧控制台指纹比对，防首登 MITM；与 G 收紧入站同等重要）。

## 建议（非阻塞）

1. D「最便宜」确定性：最便宜随 region/时段变化——请写死选择程序（provision 时查价快照 + 落 tag 备查）与兜底（最便宜 AZ 无容量时的 fallback 顺序），否则不可复现、不可审计。
2. F 非交互 fallback：未指定 region 时“询问 owner/lead”是交互动作——请给非交互行为（报错并附候选便宜 region 列表），防自动化调用挂起。
3. G 出口 IP 漂移：server 出网 IP 变化（NAT/重启）会锁死 SSH——请给 SG 更新机制（server 持有 Elastic IP，或 SG 规则随 server IP 自更新 job）。
4. O 与 token 路径关系一句话：server SSH 直推 import（可信执行者已持私钥）与“一次性 token 唯一路径”（用户自带机器）并存——请在 §6 落一句两者分工，防后人误读为 token 路径作废。
5. 轮换流程：AWS 凭据 + per-node SSH key 的“可轮换”请展开为流程（周期/触发/不断连做法），与 #8 secret 轮换文档同口径。
6. 凭据归属表述：team owner 统一凭据 vs #4101-2 BYO 先行——§8.3 已 deferred 到 #8，确认即可；但请在 §1 加一句“熔断同批（N+T3）即 #4101-2 的平台账号前置条件”，闭环 BYO 例外的合规论证。
7. §5.0 重开语义：terminated 重开是复用旧 OS 用户名还是新建随机后缀——请写死（建议新建，避免残留家目录/服务名冲突；destroy 已删用户则自然新建）。
8. E 回退判定点：ARM→x86 回退由谁在何时判定（步 3 按 region 容量/镜像标签，还是步 6 工具链校验失败）——请写死，避免两处都判或都不判。

## 缺失测试 / 风险

- 实现时测试矩阵（供后续任务引用，不属本文）：幂等（并发双 invite 单台）/ destroy 清单逐项断言 + 吊销演练（旧证书打任意 RPC 立刻被拒）/ token 四负例（自带机器路径）/ host-key 篡改拒 / 超时失败迁移 / 重开幂等（ready 直返 vs terminated 重开）。与 t1-impl-test §3–§5 同源。
- 本机无 cargo；doc-only 设计评审，以裁决对照 + 可实现性核验为准。
- reviewer≠author 满足。
