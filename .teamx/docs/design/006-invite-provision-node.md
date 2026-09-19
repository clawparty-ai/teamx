# 设计：invite 时开通云主机（server 侧 AWS CLI 执行）

- 状态：review
- 所有者：tech-lead
- 创建者：caishu（owner）
- 创建日期：2026-09-19
- 范围：**只覆盖「invite 时（可选）给被邀请人开通一台云主机」这一件**。#8（GitHub App / 多租户 / 开通页）另案，不在本文。
- 关联：讨论 #6（owner 裁决 `dsc_18d644d8ab1de0272222bc#4101`）、`design/004-t1-provider-external.md`、`design/005-t1-impl-test.md`

## 1. 模型

- **触发者**：team owner / team lead 执行 `teamx invite`（带 provision 参数）。
- **执行者**：**teamx server**（不是被邀请人、也不是 CLI 本地）。
- **前提**：teamx server 上部署 **AWS CLI**，并配置 **team owner 提供的 AWS 凭据**。
- **推论**：因为云主机由 server 创建，**server 天然持有 SSH 私钥**，可直接登录这些主机做初始化/运维/回收。

## 2. 已定裁决

| # | 点 | 结论 |
|---|---|---|
| ③ | **默认关** | `invite` 不带 provision 参数 → **不开机器**（走「邀请即引导」） |
| ④ | **异步** | invite 立即返回；provision **入队异步 job**，invite 成功不以 job 成功为前提 |
| A | **AWS 凭据形态** | 优先 **instance profile / IAM role**（server 在 EC2 上则无需落 access key）；否则用 owner 提供的 access key，**加密存 server** |
| B | **IAM 最小权限** | 仅 `ec2:RunInstances` / `TerminateInstances` / `DescribeInstances` + keypair + 打标签；**不给通配** |
| C | **SSH 私钥** | **per-node keypair**（单台可吊销）+ server 侧**加密存储** |
| D | **云主机规格** | **默认选该 region 最便宜的机型**，规格下限 **1 vCPU / 2 GiB**（可 `--size` 覆盖） |
| E | **架构** | **优先 ARM**（AWS `t4g` 系）；镜像/工具链不兼容时回退 x86（`t3`） |
| F | **region** | 由 `invite` CLI 参数指定；未指定时**询问 owner/lead**，并**推荐若干便宜 region** 供选 |
| G | **安全组** | SSH **只允许 teamx server 的出网 IP**（不开 `0.0.0.0/0`）；节点需出网（连 serve + opencode.ai），不需公网入站 |
| H | **节点软件** | teamx + opencode(≥1.18.0) + ttyd + bun/node + git，按 systemd 常驻（清单见 §5.1，参考现网 caicai 主机） |

**沿用既有裁决**：默认 S 规格 + TTL 30d 可续 + 空闲 auto-stop（CPU<5% 30min）+ 每 user 常驻上限 1 台 + `destroy` 可验证清单（terminate + 清 key/secret + 吊销 member 证书）+ region 白名单（开通时校验）+ 凭据/私钥加密存储·可轮换·全审计 + `budget.alert` 熔断前移 T3 + 计量落台账。

## 3. 流程（9 步）

```text
owner/lead: teamx invite <role> --provision-node [--provider aws --region .. --size .. --ttl ..]
      │
      ▼  teamx server 执行
 1. 建 seat：member + 邀请函（现有 invite 逻辑）
 2. 入队 provision job（异步；invite 立即返回）
 3. aws cli：RunInstances（per-node keypair / 安全组 / region / 规格 / TTL 标签）
 4. 等实例 ready
 5. SSH 登录（server 持该节点私钥）
 6. 初始化节点：装 teamx（+ opencode / ttyd / bun / git 等，见 §5.1）
 7. **按 team 创建独立 OS 用户 + 家目录**（见 §3.1），在该 OS 用户下 import 邀请函 → 以该 member 身份入队
 8. 以该 OS 用户身份注册并启动 systemd 服务：worker / opencode-serve / ttyd / fs / tx-expose /（可选）im-gateway
 9. 回写 nodes 表 + 台账；状态 ready
```

### 3.1 节点实例隔离模型（owner 裁决）

- **一台云主机 ↔ 一个用户（人）**；该用户可在此主机上运行**多个 team 的 member 实例**。
- **每个 team 实例 = 一个独立 OS 用户**：例如 team `abc` → OS 用户 `abc` + 家目录 `/home/abc`；该实例的**目录、证书/letter、systemd 服务**都归属该 OS 用户。
- **所有组件都以 systemd service 注册**（每实例一套）：`opencode-serve` / `teamx-worker` / `teamx-fs` / `ttyd` / `tx-expose` / `im-gateway`（可选）。
- **收益**：team 之间 **OS 级隔离**（文件/进程互不可见）；单实例回收 = 停服务 + 删 OS 用户 + 吊销该 member 证书。
- **待定**：OS 用户名映射规则（team name slug vs team_id）、用户名冲突/长度约束、初始化用的特权账号（server 侧）。

> 参考：现网 caicai 主机已跑多 team/多 workspace，但**共用同一个 OS 用户**（`caicai`）；本设计在此基础上**升级为按 team 分 OS 用户**，以获得 team 间隔离。

## 4. 数据模型

```text
nodes(id, team_id, user_id, member_id, provider, region, instance_id,
      ip, state, im_enabled, created_at, ready_at, terminated_at, cost_snapshot,
      compliance_tags)
provision_jobs(id, node_id, state, attempts, last_error, idem_key, created_at)
node_secrets(node_id, kind, ref, created_at)     -- 只存引用，不存明文
```

- 幂等键：`(team_id, member_id)`；`provision_jobs` 对该键建部分唯一索引 + `INSERT OR IGNORE` + 冲突回读（复用 C4 模式，覆盖并发双 invite）。
- 状态机：`requested → provisioning → bootstrapping → ready → degraded → stopped → terminated`，每次迁移落台账。

## 5. 接口

```text
teamx team invite <role> --user-name <人> --provision-node \
      [--provider aws --region .. --size .. --ttl 30d]
teamx node list | status <id> | ssh <id> | stop <id> | start <id> | destroy <id>
```

### 5.1 节点软件清单（标准，参考现网 caicai 主机）

参考实现：caicai 云主机 = **Ubuntu 24.04.4 LTS `aarch64`**（AWS）。

| 组件 | 版本 / 说明 |
|---|---|
| OS | **Ubuntu LTS arm64**（配合 E：优先 ARM 机型） |
| `teamx` | CLI；版本需与 serve **semver 兼容**（bootstrap 版本门 fail-fast 校验） |
| `opencode` | **≥ 1.18.0**（muse free tier 要求）；headless `opencode serve` |
| `ttyd` | web 终端（1.7.x） |
| `bun` / `node` | opencode 插件 / agent 运行时 |
| `git` | 2.43+ |

**systemd 常驻单元（模板，每个 team 实例一套，归属对应 OS 用户）**：

- `opencode-serve-<member>-<ws>.service` —— headless opencode server
- `teamx-worker-<member>-<ws>.service` —— 自主 worker（轮询 inbox）
- `teamx-fs-<member>.service` —— 成员文件服务（可选）
- `ttyd-<member>.service` —— web 终端（可选）
- `tx-expose-*.service` —— tunnel expose（opencode / ttyd / fs，可选）
- （#6 可选）`im-gateway` 常驻（飞书通道层，依赖 T4）

**配置与目录**：`~/.config/opencode/opencode.jsonc`、`~/.teamx/`（letter / instance / worker 状态）、workspace 目录。

## 6. 安全（server 是高价值目标）

- server 同时持有 **AWS 凭据** 与 **各节点 SSH 私钥** → 必须：加密存储、可轮换、读取全审计、IAM 最小权限。
- **per-node keypair**：单台泄露/回收只影响该台，`destroy` 即吊销。
- `destroy` 可验证清单：实例 terminated、keypair 删除、secret 清理、member 证书吊销、invitation revoke、快照按策略；负例：destroy 两次第二次为无操作成功、destroy 不存在报明确错。

## 7. 与既有文档的关系

- **修正 `t1-provider-external` 的拓扑歧义**（评审必改项 3）：**serve 只在中心一台（teamx server）**；被开通的机器是 **member node（worker/agent）**，**不是 serve**。
- **provider 首批顺序（owner 裁决）**：**AWS CLI 先行**（本文的 invite-provision 主路径就是 AWS CLI 驱动）；**k8e-sandbox 等 AWS CLI 落地后再讨论**，不在首批。

## 8. 开放项

1. ~~provider 首批顺序~~ → **已定：AWS CLI 先行；k8e-sandbox 后续再议**。
2. 一次性 token 路径的保留范围（「用户自带机器」路径）——归 #8 讨论。
3. 云账号归属的对外表述（团队 owner 提供凭据 vs 用户自带云账号）——归 #8 讨论。
