# teamx 企业版开发团队

## 背景
面向 **teamx 企业版**（teamx-enterprise 仓库）的研发团队：开发与完善企业级能力——mTLS 网络模式与成员/管理 console、抓包/分析/回放、团队文档与任务契约、IM 接入（远程操作与管理）等。
团队遵循三条铁律：**设计优先 / 必须评审 / 测试优先**——每个交付物先设计、再评审、后测试，通过后才进入执行与交付。

## 目标
- 开发和完善 teamx 企业版
- 以「设计 → 评审 → 测试 → 交付」闭环推进每个迭代，关键能力均有文档与测试覆盖

## 工作规则
- **设计优先**：每个交付物先产出设计（需求/方案/接口/测试方案），设计评审通过前不得用于交付与执行
- **必须评审**：评审至少 3 名评审人员（被评审文档的作者不参与），意见全部关闭后才定为「终稿」
- **测试优先**：开发与功能必须先有测试方案与用例（经评审），然后才进入开发
- **文档入库（标准目录，强制）**：所有团队文档（requirements / design / issue / pr / test-plan / release-note）**必须**写入标准文档目录
  `<workspace>/.teamx/docs/<doc_key>/<NNN-slug>.md`（+ 同名 `<NNN-slug>.meta.json`），遵循本文档 `## 文档` 契约。
  - **禁止**把文档散落在台账仓（如 `teamx-ent`）、临时目录、讨论帖或其它非标准路径；已散落的需迁入标准目录（可保留 `source` 指回原件）。
  - 命名用 `<NNN>-<slug>` 顺序编号（每类型内递增）；`.meta.json` 必含 `doc / id / state / owner / created_at / updated_at / history`。
  - 文档实例是 **git 跟踪的组织过程资产**，须提交到 workspace 仓；状态推进（draft → review → approved → done）通过 `.meta.json` 的 `state` + `history` 记录。
  - user-console 的 **Documents 面板只读该目录**（`<console workspace>/.teamx/docs`），写到别处面板不可见。

## 成员
### caishu
- 姓名: caishu
- 角色: owner
- 分工: 团队负责人（lead）；定目标、拆任务、验收与合并；方案与架构把关
- 技能: Rust、系统设计、mTLS/网络、协作流程
- 输出: 目标与决策、方案、评审与合并

### weiye
- 姓名: weiye
- 角色: tech-lead
- 分工: 技术负责人/评审：方案评审、代码 review、疑难定位
- 技能: Rust/TypeScript、分布式与安全、review
- 输出: 评审意见、架构方案

### caicai
- 姓名: caicai
- 角色: contributor
- 分工: 研发：Rust CLI/服务端、SQLite、插件
- 技能: Rust、TypeScript、SQLite、测试
- 输出: 代码、PR、测试

### weiye-mini
- 姓名: weiye-mini
- 角色: contributor
- 分工: 研发：dsh 插件、构建与发布
- 技能: TypeScript、bun、构建/发布
- 输出: 插件代码、构建产物、PR

### teamx-ent
- 姓名: teamx-ent
- 角色: owner
- 分工: 团队 bootstrap 席位（owner）；系统级广播与运维
- 技能: 运维、网络
- 输出: 运维与广播

## 文档

### requirements
- 标题: 需求文档
- 用途: 定义需求与验收标准，是设计/开发/测试的依据
- 模板: 背景 | 目标 | 范围 | 用户故事 | 验收标准 | 变更记录
- 创建者: [owner, tech-lead]
- 所有者: owner
- 审批者: [owner, tech-lead]
- 状态流: draft -> review -> approved -> done
- 变更响应:
    - on created: 通知 owner 细化需求
    - on approved: 通知 contributor 开始设计与开发

### design
- 标题: 设计文档
- 用途: 记录方案设计（架构/接口/数据模型），设计评审通过后方可实施
- 模板: 背景 | 目标 | 方案 | 接口与数据 | 风险 | 测试要点
- 创建者: [tech-lead, contributor]
- 所有者: tech-lead
- 审批者: [owner, tech-lead]
- 状态流: draft -> review -> approved -> done
- 变更响应:
    - on created: 通知 tech-lead/owner 评审
    - on approved: 通知 contributor 开始实现

### issue
- 标题: 缺陷 / 改进请求
- 用途: 记录缺陷与改进请求，驱动修复流程
- 模板: 描述 | 复现步骤 | 预期行为 | 实际行为 | 影响范围 | 优先级
- 创建者: [owner, tech-lead, contributor]
- 所有者: owner
- 状态流: opened -> triaged -> assigned -> fixing -> verified -> closed
- 变更响应:
    - on created: 通知 owner 分析并分诊
    - on verified: 通知 owner 关闭并记录到 release-note

### pr
- 标题: 代码合并请求（PR）
- 用途: 代码变更的评审与合并
- 模板: 变更描述 | 关联 issue | 测试说明 | 回滚方案
- 创建者: [contributor, tech-lead]
- 所有者: 提交者
- 审批者: [tech-lead, owner]
- 状态流: opened -> reviewing -> approved -> merged
- 变更响应:
    - on created: 通知 tech-lead/owner 评审
    - on approved: 合并并关闭关联 issue
    - on merged: 更新 release-note

### test-plan
- 标题: 测试方案
- 用途: 测试优先：定义测试范围/用例/环境/通过标准
- 模板: 范围 | 用例 | 环境 | 通过标准
- 创建者: [contributor, tech-lead]
- 所有者: tech-lead
- 审批者: [owner, tech-lead]
- 状态流: draft -> review -> approved -> done
- 变更响应:
    - on approved: 进入开发与测试执行

### release-note
- 标题: 发布说明
- 用途: 汇总每次发布的功能、修复与已知问题
- 模板: 版本 | 日期 | 新功能 | 修复 | 已知问题
- 创建者: [owner]
- 所有者: owner
- 状态流: draft -> review -> released
- 变更响应:
    - on released: 广播给全体成员

### review
- 标题: 评审记录
- 用途: 记录对某份文档的评审结论与意见（结论 | 评审点 | 问题 | 建议 | 风险 | 后续）
- 模板: 被评审对象 | 结论 | 评审点 | 问题（必改） | 建议（可采纳） | 风险 | 后续
- 创建者: [reviewer, tech-lead, owner]
- 所有者: 被评审文档的 owner
- 审批者: [owner, tech-lead]
- 状态流: draft -> review -> approved -> done
- 变更响应:
    - on created: 通知被评审文档的作者
    - on approved: 通知作者按必改项修正，并将被评审文档推进/退回对应状态
