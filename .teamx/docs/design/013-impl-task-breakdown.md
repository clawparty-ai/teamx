> ⚠️ **SUPERSEDED（owner 裁决 seq 7844 · task-97）**：本分解已被 `.teamx/docs/design/013-impl-breakdown-task96.md`（权威版）取代；增量已并入 013-impl-breakdown-task96.md §4.2/4.3（G2 spec、opencode.ai 机制、G5 冲突标注）。 本文件保留作历史记录，勿据其派单。

# design/013 第一步实现（provider=k8e）——任务分解 + 首个 PR 切片（task-96）

- owner: caishu · 产出: weiye-mini（task-96）· 日期: 2026-09-22
- 输入: design/013 v3.1（verified）· **实现现状盘点（本文 §0，基于 origin/main @ eead794 实测）**

## §0 现状盘点（重要：实现并非从零开始）

origin/main @ eead794（作者 caishu）已含：
- `crates/teamx/src/k8s.rs`（K8sProvider/Kubectl 主体 + Pod/NetworkPolicy/Quota manifests + 单测）✅
- `tests/k8e-provision-test.sh` + `tests/k8e-real-cluster-test.sh`（8 断言真集群全绿，013 §13）✅
- fsGroup ✓ / PriorityClass+PDB ✓ / `teamx.io/team` label ✓ / destroy 含 delete-secret ✓
- 014 第二步也已大量上 main（tenant create/destroy/CA/平台 console/service 车道/证书换发/调度保护）

**结论**：task-96 的实现工作 = **缺口收尾**，不是从零开发。

## §1 缺口清单（对照 013 v3.1 文档 vs 代码实测）

| # | 缺口 | 优先级 | 位置证据 |
|---|---|---|---|
| G1 | **M0 CNI 前置校验**：k8s.rs 无 flannel/CNI 探测（0 处）——deny-all/封 IMDS 静默失效风险 | **P0** | `git grep CNI origin/main -- k8s.rs` = 0 |
| G2 | **M1 activate trait 方法**：k8s.rs/provision.rs 无 activate——approve 门落点缺失 | **P0** | grep activate 仅 activity.rs（无关） |
| G3 | **M5 命名熵**：`tenant-{short_id(team_id)}` 仍前 8 截断（013 v3.1 裁决全长/≥16 hash）+ create 归属校验未确认 | **P0** | k8s.rs:159-161 |
| G4 | **M6 白名单四项**：egress_allow 仅来自 env（默认空），四项基线（server/registry/git/**opencode.ai**）未内建；且 NetworkPolicy ipBlock 不支持域名——opencode.ai 需 CIDR pin 或 egress 代理（见 §3.2） | **P0** | k8s.rs:729-738 |
| G5 | **M9 对齐**：代码 destroy 删 Secret（k8s.rs:787）vs 013 v3.1「存续期=座位存续期不删」——**需 owner 裁决谁对齐谁**（实现侧「删+provision 重放」若可靠亦可接受，需证明） | P1 | k8s.rs:787 |
| G6 | **tx-expose 断线自动重连**（647987c1 待修 bug）——k8e expose 边车复用同修复 | P1 | EC2 现网实测 4.5h 零重连 |
| G7 | 隧道三件套量化验收（liveness/首字节≤3s/大包 100%）落 E2E | P1 | §9.8 |
| G8 | 文档同步：013 标题仍 v2、meta state、§12 无 v3.1 行（weiye task-88 已指） | P2 | 文档 |

## §2 Workstreams 与 8 项验收映射

| WS | 内容 | 覆盖验收（013 §9） |
|---|---|---|
| WS1 | **安全收尾**：G1（CNI 探测+阻断/降级）+ G4（基线白名单+opencode.ai 机制）+ G3（命名熵+归属校验）+ G5（Secret 对齐） | 验收 2（跨租户负例）· NFR-1 |
| WS2 | **生命周期接线**：G2（trait activate + commands.rs start_member 按 provider 分支）+ destroy 清单 provider 维度复核 | 验收 1/5 · 裁决 V |
| WS3 | **隧道可靠性**：G6（tx-expose 重连+探针，EC2/k8e 同修复）+ G7（三件套落 E2E） | 验收 6/8 · NFR-5 |
| WS4 | **测试与文档**：G1 两条负例断言（§9.7b）+ G8 文档同步 + run-all 接线复核 | 验收 7 · 全部 |

## §3 首个可合并 PR 切片：WS1「k8s 安全收尾」

**理由**：全部改动集中在 `k8s.rs` 单文件域；均为安全正确性修复（评审 M0/M5/M6/M9 的代码落地）；不触 approve 门/生命周期（低回归风险）；单测可全部离线跑（manifest 断言）。

### 3.1 切片内容（文件级）
1. `k8s.rs` 新增 `cni_supports_network_policy(kubectl) -> Result<(), String>`（G1）：试部署 deny-all NP → Pod 连外验证；失败返回错误供 provision 阻断（env `TEAMX_K8S_ALLOW_NO_CNI_POLICY=1` 显式降级并打警告）。
2. `k8s.rs::K8sProvider::new`（G4）：`TEAMX_K8S_EGRESS_ALLOW` 未设时落**文档化默认基线**（注释四项+opencode.ai 的 CIDR pin 说明与 egress 代理替代方案），并加单测断言默认清单非空且不含 IMDS。
3. `k8s.rs` create 流程（G3）：namespace 已存在时校验 `teamx.io/team` label == 预期 team_id，不匹配即报错；`short_id` 换 `full-or-hash16`（同步 014 §2 命名——**与 owner 对齐后一次改两处**）。
4. `k8s.rs:787`（G5）：按 owner 裁决（删+重放 or 不删）二选一改，并同步 013 文档。
5. 单测：G1 阻断/降级两分支 + G3 归属冲突 + G4 默认清单——全离线。

### 3.2 opencode.ai 出网的机制说明（写进 PR 描述）
NetworkPolicy ipBlock 不支持域名。v1 两条路：① 运维侧把 opencode.ai 解析 CIDR 写进 `TEAMX_K8S_EGRESS_ALLOW`（IP pin，脆弱）；② member Pod 加 egress proxy sidecar（我的 muse-egress 先例：白名单桥接）。**推荐 ② 作为 G4 的姊妹切片**（第二个 PR），与 CNI FQDN 能力（M0 探测可一并测）共同决策。

## §4 执行安排

- 本盘点+分解 = task-96 交付物（本文）。
- **第一片（WS1）已 spec 到文件/函数级**，可直接开工：本会话上下文已极大，建议由**新会话执行**（交接笔记 `.teamx/weiye-mini/HANDOFF-20260922.md` @ `464c2ad` 已含全部背景；本文即其 §4 观察线「k8e 实现」的执行入口）。
- G5 需 owner 先裁决（文档 vs 代码对齐方向）——不阻塞切片 1-4 项。
- 后续切片：WS2（approve 门）→ WS3（隧道可靠性）→ WS4（文档/测试收尾）。
