---
status: active
protocol_version: 1
owner: caishu
created_at: 2026-09-25
updated_at: 2026-09-25
---

# 019 — Teamx Cloud 官网（单页）grill 设计访谈

> grill-with-docs 协议的 Design Session Record。范围：**面向公众的 teamx / teamx cloud 单页官网** ——
> 品牌介绍 + 安装方式（brew）+ 右上角 Login 跳转 teamx cloud 登录页。
> 不含：登录页本身的实现（已由平台控制台提供）、控制台内部功能（design/014-018）。

## Context

owner 明确的需求（已定，见 SD-1…SD-3）：做一个 **single page 网站**，介绍 **teamx** 与 **teamx cloud**，
提供 **teamx 安装方式（目前只有 `brew install`）**，右上角放 **Login**，点击进入 **teamx cloud 的登录页**。

现状：

- teamx 的对外承诺在 `README.md` 已成型（标语「三个臭裨将，顶个诸葛亮」、共享目标模型 vs 多智能体、
  四条原则、可计量的协作成本）。
- 安装方式**已实现并验证**：`brew install caishu97/tap/teamx`（v0.2.0，见 `docs/28-manual-homebrew-install.cn.md`）。
- **登录页已有**：平台控制台自带 `/api/login/password` + GitHub OAuth（`design/014` Phase 1–3）。
- **没有任何官网/站点**：仓库无 `site/`、无 CNAME、台账无该议题（本 session 首个）。

## 已核实事实（fact-finding）

- **F1 安装方式（唯一）**：`brew tap caishu97/tap` → `brew install caishu97/tap/teamx`
  （macOS Apple Silicon 直装 `aarch64-apple-darwin`；Linux x86_64/arm64 同样适用）。
  **Windows 不走 brew**（企业版 zip 手动装）。另有源码安装 `install.sh`（编译 Rust + 装 opencode 插件）。
  公开性：`caishu97/homebrew-tap` 是**公开仓**（实测 HTTP 200），二进制 tarball 镜像在那里；
  `caishu97/teamx-enterprise` 源码仓是**私有**的。
- **F2 文案素材现成**：`README.md` 有标语、信念、共享目标 vs 多智能体对比、四条原则、幂等交付概念；
  `docs/27-teamx-methodology.{cn,en}.md` 方法论全文（双语）；`docs/01–05` 设计文档（双语）。
- **F3 可复用资产**：架构图 `docs/teamx-core-architecture.png`（1.1MB）；
  演示视频 `docs/prototype/screencast/teamx-demo.mp4`（3.5MB）+ 字幕 `.srt`；
  静态原型 `docs/prototype/`（`index.html` 是**登录页原型**、`dashboard.html` 2057 行、`members-live.html`）。
- **F4 登录落点存在但**只能内网**到**：平台控制台登录页在 `https://192.168.0.229:9540`
  （**自签证书 + LAN IP**）。官网的 Login 要**公开可达** → 需要**域名 + 有效 TLS + 公网端点**，
  与既有开放项 **O-5「正式端点未裁决」** 直接耦合（`design/014 §9.6`）。
- **F5 控制台有三个概念**（`AGENTS.md` 术语约定）：**admin-console**（`teamx ui`，owner/lead，读 server DB）、
  **user-console**（`teamx console`，成员工作区级）、以及本项目新增的 **平台控制台**
  （`teamx platform console`，含用户注册/登录/GitHub OAuth、团队发现、TCoin、Credit）。
  "teamx cloud 的登录页"到底指哪个，需 owner 明确（DQ-0005）。
- **F6 设计语言可复用**：控制台/原型的视觉是蓝色系（主色 `#4A90E5`、卡片 + `#EBEEF5` 边框、
  `Microsoft YaHei/PingFang SC` 字体栈），原型里已有 header + logo + 卡片的成稿。
- **F7 双语是既有惯例**：文档 `docs/NN-*.cn.md` + `*.en.md` 成对；README 为英文 + 中文标语。
- **F8 无构建/无托管线索**：仓库无静态站点脚手架（site/web/CNAME 均无），无域名配置。

## Settled Decisions（owner 已明确）

- **SD-1** **单页（single page）** 官网：介绍 **teamx** 与 **teamx cloud**。
- **SD-2** 提供 **teamx 安装方式**；**目前只有 `brew install`**。
- **SD-3** 右上角 **Login** → 点击进入 **teamx cloud 的登录页**。

## Design Tree

| DQ | 问题 | 状态 | 结论 |
|---|---|---|---|
| DQ-0001 | **站点目标与边界**（只做介绍+安装+登录引流？文档是否外链？） | ⬜ open | — |
| DQ-0002 | **语言**：中文 / 英文 / 双语切换 | ⬜ open | — |
| DQ-0003 | **托管与仓库**：公开静态仓 + GitHub Pages / 自有服务器 / 第三方托管 | ⬜ open | — |
| DQ-0004 | **技术形态**：单文件零构建 HTML / 轻量框架 | ⬜ open | — |
| DQ-0005 | **Login 的落点**：哪一个控制台的登录页 + 公开可达如何解决 | ⬜ open | — |
| DQ-0006 | **安装区块呈现**：brew 命令/复制按钮/Windows 与 Linux 说明 | ⬜ open | — |
| DQ-0007 | **内容板块与顺序**（Hero → 为什么 → 原则 → 安装 → cloud → 文档/视频 → 页脚） | ⬜ open | — |
| DQ-0008 | **视觉**：复用控制台设计语言 / 新视觉；深浅色 | ⬜ open | — |
| DQ-0009 | **资产复用**：架构图 / demo 视频 / 控制台截图（可否公开） | ⬜ open | — |
| DQ-0010 | **埋点与分析**：v1 是否需要 | ⬜ open | — |
| DQ-0011 | **维护方式**：内容与 README/docs 的关系（手工 / 派生） | ⬜ open | — |
| DQ-0012 | **移动端与性能**：响应式要求、视频体积策略 | ⬜ open | — |

## Current Frontier（Round 1）

**主题：目标与边界 + 语言 + 托管 + 技术形态 + 登录落点 + 安装呈现**（DQ-0001…DQ-0006）

建议（待 owner 裁决）：

1. **DQ-0001 目标 = 转化两件事**：① 让人装 teamx（brew 命令一键复制）② 让已有用户 **Login**。
   文档/方法论**不复制**到站内，改为**外链** GitHub（README / docs/27 / 方法论文）。站内只保留
   "是什么 / 为什么 / 怎么装 / 怎么进"。
2. **DQ-0002 语言 = 双语单页 + 右上角切换（中/英）**，默认**跟随浏览器**（`Accept-Language`），
   不做多页路由（保持 single page）。
3. **DQ-0003 托管 = 新建**公开**仓（如 `caishu97/teamx-site`）+ GitHub Pages**（自定义域待定）。
   理由：源码仓私有、tap 仓不适合放站点；Pages 免费且与 brew 分发同源（都在 GitHub 生态）。
4. **DQ-0004 技术形态 = 单文件零构建静态 HTML**（内联 CSS/JS），无 npm/无构建步骤；
   理由：single page、内容以文案为主、可被任意静态托管、审阅与改动成本最低（也便于以后换成别的托管）。
5. **DQ-0005 Login 落点 = 平台控制台登录页**（含 GitHub OAuth），但**公开可达是硬依赖**：
   在域名/TLS/公网端点未定（O-5）前，建议 v1 先把 Login 指向**一个配置常量**，
   默认值留空 → 点击提示"即将开放"或指向 GitHub 仓库；**不硬编码 LAN 地址**。
   （本 DQ 需要 owner 给出：目标 URL / 域名是否有 / 何时可得）
6. **DQ-0006 安装区块 = 主推 brew（一键复制）+ 次要说明**：
   ```bash
   brew install caishu97/tap/teamx
   ```
   macOS/Linux 同命令；**Windows 一行注明"暂不支持 brew，见文档"**；不展示源码安装（保持单一入口）。

**Round 2（待 Round 1 settled）**：内容板块顺序、视觉、资产（架构图/视频/截图）、埋点、维护方式、移动端。

## Open Questions

- 官网的**域名**是否已有（或计划）？
- Login 目标 URL 现在是否能给出（哪怕是临时域名/隧道地址）？
- 站内是否**必须**出现价格/TCoin/Credit 等商业化信息？（当前 TCoin/Credit 已实现，但未对外定价）

## Fact Requests and Reports

（暂无）

## Remaining Branches（本 session 之外）

- 多页站点（文档站/博客/定价页）
- 站点分析、SEO、Open Graph 卡片
- 英文之外的语种
- 官网与 tap 仓、docs 的自动化同步

## Related Artifacts

- `README.md`（对外承诺/标语/四原则/方法论入口）
- `docs/28-manual-homebrew-install.cn.md`（brew 安装与发布，v0.2.0 已验证）
- `docs/27-teamx-methodology.{cn,en}.md`（方法论全文，双语）
- `docs/prototype/`（`index.html` 登录页原型、`dashboard.html`、`screencast/teamx-demo.mp4`）
- `docs/teamx-core-architecture.png`（架构图）
- `design/014` §9.6（平台控制台能力 + **O-5 正式端点未裁决** —— 本项目的 Login 依赖）
- `AGENTS.md`（admin-console / user-console 术语约定）
- `CONTEXT.md`（术语表）
