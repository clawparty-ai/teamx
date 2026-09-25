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
- **SD-4（Round 1）站点的首要目标是「让用户了解并使用 teamx cloud」**；teamx 客户端只是
  **teamx cloud 的入口之一**（不把官网写成 CLI 工具介绍页）。
- **SD-5（Round 1）中英双语**；**平台控制台（console）也要双语**（新增范围）。
- **SD-6（Round 1）不新建仓库**：站点放在**当前仓库的 `site/` 目录**。
- **SD-7（Round 1）站点静态资源**编译**进 `teamx` 二进制**，用 **`teamx site`** 启动即可浏览
  （与 console 内嵌 `INDEX_HTML` 同思路）。
- **SD-8（Round 1）Login 指向平台控制台登录页**（含 GitHub OAuth）。
- **SD-9（Round 1）安装区不提 Windows**。
- **SD-10（Round 1）必须有基础站点安全**：① **登录防暴力破解**（限速/锁定/退避）
  ② **强制 HTTPS**（证书可从 Let's Encrypt 获取）。
- **SD-11（Round 2）`teamx site` 一个进程同时服务官网与控制台**：`/` 官网、`/console` 平台控制台；
  **Login 是站内相对路径**（同域、共享 HTTPS 与 cookie）。
- **SD-12（Round 2）强制 HTTPS = 外部签发 + 二进制加载证书**：
  `teamx site --tls-cert --tls-key`（Let's Encrypt 签发的证书）；**未配证书时只监听 127.0.0.1**；
  HTTP 端口 301 → HTTPS。**内建 ACME 列为 v2**。
- **SD-13（Round 2）登录防爆破 = 双维度计数 + 指数退避 + 锁定 + 审计**：
  per-IP 与 per-账号同时计数；15 分钟 ≥5 次失败 → 指数退避（1s→60s）；1 小时 ≥10 次 →
  锁账号 15 分钟；成功清零；失败/锁定写平台审计；阈值可配。
- **SD-14（Round 2）console 双语 = 内嵌字典 + cookie 语言 + 服务端错误改 error code**：
  前端 `I18N{zh,en}` + `data-i18n` + `applyLang()`；默认跟随 `Accept-Language`，选择存 cookie；
  **服务端只回 error code，文案由前端映射**；覆盖平台控制台全量 UI 文案。
- **SD-15（Round 2）视觉沿用控制台设计语言**（主色 `#4A90E5`、同一字体栈与 CSS 变量），
  官网用大字号 Hero。
- **SD-16（Round 2）不内嵌大文件**：`site/` 内 HTML/CSS/JS/小图用 `include_str!`/`include_bytes!`；
  视频/大图不内嵌（外链或按需）。v1 **不做埋点**，只做最小 SEO（title/description/OG）。
- **SD-17（Round 2，内容）不放视频**；改用**多图轮播**代替。
- **SD-18（Round 2，内容）不堆技术与技术细节**；重点讲**「人和 AI 结合的团队」**及其**优点**。
- **SD-19（Round 3）用「各国谚语」作为开场母题**：并列呈现不同文化的"人多智慧大"谚语
  （中文「三个臭皮匠，顶个诸葛亮」、英语 "Two heads are better than one"、日语「三人寄れば文殊の知恵」…），
  收束成一句：**所有文化都懂这个道理，teamx 第一次把它用在 AI 上**。
  ⚠️ 上线前**逐条核对原文、含义与出处**（避免张冠李戴）；中文成语的**正体用字**也需确认
  （通行写法为「三个臭**皮匠**」，README 现作「裨将」）。
- **SD-20（Round 3）优势四条 = 更快 / 更强 / 可控 / 持续成本优化（性价比）**：
  ① **更快**：人机成员同时推进同一目标（并行而非排队）；
  ② **更强**：普通模型 × 协作 ≥ 一个昂贵模型单打独斗（原主张 B）；
  ③ **可控**：owner 在环——审批、纠偏、验收（原主张 C）；
  ④ **持续的成本优化**：同预算买到一支团队；用量与成本可见 → 可不断优化性价比。
- **SD-21（Round 3）key point 用「卡通风格插画」表达**（不放照片/截图堆砌），五个关键点：
  ① 分享目标、共同努力；② 人 + AI 混合的成员构成；③ 多种合作模式（分工 / 议会 / 讨论…）；
  ④ 成本、用量、审计；⑤ 治理、信用、效率管理。
- **SD-22（Round 3）页面结构 = 轮播 → 分段价值 → 快速开始 → 页脚链接**
  （Hero → 谚语母题 → **图片轮播** → 分段价值 → 快速开始 → 页脚）。
- **SD-23（Round 4）谚语收束句 = 公式式**：大字 **`team with AI = win`** + 中文副行
  「**AI 时代，会组队者胜**」（英文副行同义）。取代原先"所有文化都懂这个道理"的散文式收束。
- **SD-24（Round 4）优势四条细化**（在 SD-20 之上补语义）：
  ① **更快** = **人负责决策、AI 负责执行**，两边同时动 → 并行推进同一目标；
  ② **更强** = **无论普通模型还是强大模型，多个都会更强**（协作放大任何模型，模型越强增益越大）；
  ③ **可控** = **不止 owner**：**人在必要环节参与**——管控但不牺牲效率（不是事事审批）；
  ④ **持续的成本优化** = AI 时代新技术与产品层出不穷，**最快用上"低成本 + 高能力"的技术与产品**，
  这才是「**性价比之道**」。
- **SD-25（Round 4）谚语放进轮播，与插画交替**：**中日韩等有书法的谚语用书法呈现**；
  多条谚语**全部可用**，但**必须穿插开——同语言不得相邻**。
- **SD-26（Round 4）插画用 AI 生成**（owner 指示"试下用 AI 画"）。**实测可行**：
  - 通道：**OpenRouter `google/gemini-2.5-flash-image`**（Google 直连 key 配额 429；zen 无图像模型）
  - 产物 1024×1024 PNG ≈ **775KB** → 站点需**压缩**（`sips` 降采样 + JPEG/WebP，目标 ~50–80KB/张）
  - 风格：扁平卡通、圆角几何、统一描边、主色 `#4A90E5` + 珊瑚 + 薄荷、白底、**图内不写字**

## Design Tree

| DQ | 问题 | 状态 | 结论 |
|---|---|---|---|
| DQ-0001 | 站点目标与边界 | ✅ | SD-4（cloud 优先；teamx 客户端是入口之一） |
| DQ-0002 | 语言 | ✅ | SD-5（中英双语；console 也要双语） |
| DQ-0003 | 托管与仓库 | ✅ | SD-6（本仓 `site/` 目录，不新建仓库） |
| DQ-0004 | 技术形态 | ✅ | SD-7（静态资源内嵌二进制，`teamx site` 启动） |
| DQ-0005 | Login 的落点 | ✅ | SD-8（平台控制台登录页） |
| DQ-0006 | 安装区块呈现 | ✅ | SD-9（brew 为主，不提 Windows） |
| DQ-0007 | `teamx site` 形态（同进程 / 同域 / Login 相对路径） | ✅ | SD-11 |
| DQ-0008 | 强制 HTTPS 路径（外部签发 + 加载证书；ACME v2） | ✅ | SD-12 |
| DQ-0009 | 登录防爆破策略 | ✅ | SD-13 |
| DQ-0010 | console 双语实现方式 | ✅ | SD-14 |
| DQ-0011 | **站点内容与叙事**（本轮重点，拆为 DQ-0016…0023） | 🔄 | 见 Round 3 |
| DQ-0012 | 视觉（沿用控制台设计语言） | ✅ | SD-15 |
| DQ-0013 | 资产（**不放视频 → 多图轮播**） | ✅ | SD-16 / SD-17 |
| DQ-0014 | 内嵌方式与体积 | ✅ | SD-16 |
| DQ-0015 | 埋点 / SEO | ✅ | SD-16 |
| DQ-0016 | 核心主张（**各国谚语母题** + 收束句） | ✅ | SD-19 |
| DQ-0017 | 目标读者与深浅 | ✅ | 团队 owner / 技术负责人（口语、不炫技） |
| DQ-0018 | 优势清单 = **更快/更强/可控/持续成本优化** | ✅ | SD-20 |
| DQ-0019 | 轮播图（**多图轮播**，配"好处"文案） | ✅ | SD-17 / SD-22 |
| DQ-0020 | 技术信息边界 | ✅ | 只留安装命令 + 一句"团队独立、数据各自留存"；架构图不上首页 |
| DQ-0021 | TCoin / Credit 是否出现 | ✅ | **不出现名词**，只在"成本/治理"里作体现（SD-21 ④⑤） |
| DQ-0022 | 语气与双语 | ✅ | 克制、有信念不喊口号；中英同一语气 |
| DQ-0023 | CTA 主次 | ✅ | **Login 主 / 安装次** |
| DQ-0024 | **插画形态与来源**（卡通风格 SVG 自绘 / 外部图片 / AI 生图） | ⬜ open | — |
| DQ-0025 | **谚语清单与核对**（选哪 6–8 条、原文写法、出处） | ⬜ open | — |

## Current Frontier（Round 5：轮播编排确认 + 插画批量产出）

- **DQ-0024 插画形态 = AI 生成**（SD-26，已验证通道与风格）。
  待办：按同一 prompt 骨架批量产出 **5 张 key point 插画**（分享目标 / 人机混合 / 多种合作模式 /
  成本用量审计 / 治理信用效率），owner 挑选与微调。
- **DQ-0025 谚语清单**：**全部可用**（SD-25），但需定**轮播编排表**：谚语 ↔ 插画交替、
  同语言不相邻、CJK 用书法；上线前逐条核原文与出处。
- 待定：书法字体（CJK 毛笔字体**子集内嵌**，只为用到的字生成小 woff2，避免整字体数 MB）。

## Frontier（Round 4：插画实现 + 谚语定稿）

只剩两件（其余已 settled）：

- **DQ-0024 插画形态**：建议 **手写内联 SVG（扁平友好风）**，理由：① 单文件站点不引外部资源、
  ② 不增二进制体积（矢量极小）、③ 可随主题色 `#4A90E5` 走、④ 可双语（图内不写字或极少字）。
  备选：外部 PNG（需美术资源）/ AI 生图（风格不可控、栅格体积大）。
- **DQ-0025 谚语清单**：拟 8 条（中 / 英 / 日 / 韩 / 印地 / 俄 / 德 / 印尼 / 非洲），
  **逐条核对原文与出处**后再上（SD-19 已记约束）。

## Frontier（Round 3：内容与叙事）—— 已 settled

**主题：讲什么、对谁讲、用什么话讲**（DQ-0016…DQ-0023）—— owner 要求"深入讨论内容"。

拟稿见 `design/019-content-draft.md`（同目录），要点：

- **主张**（沿用 README 信念，但换成"人 + AI"的说法）：
  「**一个人加一个 AI，不是一支团队**」→ 「**teamx 让「人 + AI」组队，用共享目标把普通模型
  的合力做到超过单个昂贵模型**」。
- **优势四条（业务语言，不出现 mTLS/多租户等词）**：
  ① **结果更强**：普通模型 × 协作 ≥ 一个昂贵模型单打独斗；
  ② **成本更省**：同样的钱，得到一支团队而不是一次问答；
  ③ **始终可控**：owner 在环——审批、纠偏、验收，AI 不脱缰；
  ④ **看得见、算得清**：谁做了什么、进度与花费留痕，可复盘可度量。
- **轮播 5 张**（每张配"好处"而非功能名）：
  1. 团队列表与目标 → "一眼看清谁在为同一个目标工作"
  2. 团队详情/成员与角色 → "人和 AI 各有角色，分工写在台面上"
  3. 全部团队/发现与加入 → "找到合拍的团队，一个邀请函就能加入"
  4. 成本与用量 → "谁花了多少，一眼算清"
  5. 治理与信用 → "长期合作有据可依"
- **技术边界**：只保留**安装命令**与一句"每个团队独立、数据各自留存"的可信说明；
  架构图**不上首页**（要放就放到文档外链里）。
- **CTA**：主 **Login**（cloud 优先），次 **安装**（`brew install caishu97/tap/teamx`）。

## Current Frontier（Round 2）

**主题：`teamx site` 形态 + HTTPS/防爆破安全 + console 双语 + 内容与视觉**（DQ-0007…DQ-0015）

建议（待 owner 裁决）：

7. **DQ-0007 `teamx site` 形态 = 独立子命令，一个进程同时服务官网与控制台**：
   `teamx site --addr 0.0.0.0 --port 443`（默认 8443 便于本地试）→ `/` 官网、`/console`（或 `/app`）
   平台控制台；官网与控制台**同域**，这样 **Login 就是一个站内相对路径**（`/console`），
   不需要跨域配置，也天然共享 HTTPS。
   理由：SD-7 要求二进制内嵌；同域让 Login 与 cookie（`SameSite=Strict`）语义最简。
8. **DQ-0008 强制 HTTPS 路径 = 「外部签发 + 二进制加载」为 v1**：
   `teamx site --tls-cert /path/fullchain.pem --tls-key /path/privkey.pem`，
   证书由 **Let's Encrypt（certbot/任何 ACME 客户端）** 签发；**未配置证书时只监听 127.0.0.1**
   （拒绝明文对外）；并对 HTTP 端口做 **301 → HTTPS**。
   **内建 ACME**（二进制自己跑 ACME 协议、自动续期）列为 **v2 候选** —— 需要域名 + 80/443
   独占 + 续期调度，复杂度和风险都更高（先用外部签发把站点跑通）。
9. **DQ-0009 防爆破 = 双维度计数 + 指数退避 + 锁定 + 审计**：
   - 维度：**per-IP** 与 **per-账号** 同时计数（防单 IP 打多号 + 防多 IP 打单号）
   - 窗口/阈值：如 15 分钟内 ≥5 次失败 → 指数退避（1s、2s、4s…上限 60s）；
     1 小时内 ≥10 次 → **锁定该账号 15 分钟**（IP 侧同样锁定）
   - 成功登录清零；失败与锁定写**平台审计**（复用 `platform_audit`）
   - 实现复用既有模式（`direct.rs` 已有"窗口内计数 + reason_code"的现成做法）
   - **常量可配**，v1 先给保守默认值
10. **DQ-0010 console 双语 = 一个内嵌字典 + 前端切换 + 服务端消息双语**：
    - 前端：`I18N = {zh:{...}, en:{...}}`，`data-i18n` 属性 + `applyLang()`；选择持久化到
      **cookie（`teamx_lang`）**，默认跟随 `Accept-Language`
    - **服务端错误消息**：`/api` 返回的 `error` 文案也需双语（用 error code + 前端映射；
      或服务端按 `Accept-Language`/cookie 选语言）—— **建议 error code + 前端映射**（服务端不动文案）
    - 范围：**平台控制台全量 UI 文案**（含菜单/按钮/提示/空态），这是本 session 里**最重**的一块
11. **DQ-0011 内容板块（cloud-first）**：
    Hero（teamx cloud 一句话 + Login / 安装）→ **teamx cloud 是什么**（多租户、每租户独立 CA、
    团队发现/加入、TCoin/Credit 治理）→ **团队协作怎么发生**（共享目标 + 台账 + 人机同在）→
    **四种入口**（① 浏览器 Login ② 客户端 `brew install …` 接入 ③ 邀请函加入 ④ 观察者）→
    方法论外链 → 页脚
12. **DQ-0012 视觉**：复用控制台设计语言（主色 `#4A90E5`、卡片、同一字体栈），官网用**大字号 Hero**；
    与 console **同一套 CSS 变量**，避免两套皮肤。
13. **DQ-0013 资产**：架构图 PNG 上 Hero 下方；**demo 视频以"点开才加载"方式引入**
    （`<video preload="none" poster>`）避免首屏 3.5MB；截图用现有控制台界面（TCoin/Credit/我的团队）
14. **DQ-0014 内嵌方式**：`site/{index.html,assets/…}` 用 `include_str!`/`include_bytes!` 内嵌
    （与 console 同做法）；**视频等大文件不内嵌**（放 `docs/` 并外链，或按需打包）——需 owner 定
15. **DQ-0015 埋点/SEO**：v1 **不做埋点**；做最小 SEO（`<title>`/description/Open Graph 卡片）

**Round 3（若需要）**：域名与证书签发流程、上线 runbook、ACME 内建（v2）。

## Open Questions

- 官网的**域名**是否已有（或计划）？`teamx site` 上线时用它签发 Let's Encrypt 证书。
- Login 作为站内相对路径（`/console`，DQ-0007 建议）是否可接受？还是要求独立域名？
- 站内是否出现 **TCoin / Credit**（已实现，未定价）：**作为"平台治理能力"介绍**，还是完全不出现在官网？
- console 双语是否**必须与站点同批交付**（SD-5 一句话带来的是整个控制台文案的 i18n 工作量）？

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
