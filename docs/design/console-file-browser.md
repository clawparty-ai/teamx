---
status: settled
protocol_version: 2
owner: caishu
created_at: 2026-09-15
updated_at: 2026-09-15
---

# Member Console 「File」一级菜单：工作区文件浏览与预览

## Context

member console（`teamx console`，user-console）目前有 5 个一级菜单：
`Members / My Tasks / Events / Discussions / Documents`（`console_html.rs:27-33`）。

用户需要一个 **「File」一级菜单**，浏览 **workspace 中本地 git repo 的文件**，并支持**文件预览**。

现状可复用的资产：

| 资产 | 位置 | 能力 |
|---|---|---|
| `teamx fs` 文件服务 | `crates/teamx/src/fs.rs` | `GET /api/list`、`/api/read`（1 MiB 上限）、`/api/download`、`POST /api/upload`；仅 loopback；拒绝 `.teamx`/`.git`；路径/符号链接逃逸防护 |
| `#fsOverlay` 浏览器 | `console_html.rs:111-127, 434-479, 672-802` | 面包屑 + 文件列表 + 预览；经 `/api/cloud/{id}/...` 代理到 `fs-*` 隧道 |
| `docs`/`doc` 面板 | `console_data.rs:313-375, 413-444` | 读 `.teamx/docs/`；单文档 + 列表的模式可借鉴 |
| `mdRender` | `console_html.rs:757-789` | 迷你 markdown 渲染（链接白名单，XSS 安全） |

约束：console 与工作区**同机**（Local 模式直读文件系统）；Network 模式下 console 仍是工作区级的。

- grill discussion: `dsc_18d55ee86a6404d11de92d`
- 共识规则: majority（分歧记录 dissent）
- leader: caishu
- 议题消息: `#1801`…`#1806`（DQ-0001…DQ-0006）

## Design Questions（Frontier）

### DQ-0001 浏览根与作用域
「本地 git repo 的文件浏览」的根是什么？
- **A. workspace 根整树**（opencode 会话 cwd，含各 repo），repo 只是其中的目录
- **B. 只列 git repo**（扫 `.git`），进入 repo 后再浏览
- **C. 复用 `teamx fs`**（浏览远端成员工作区）

倾向 A + 顶部 repo 快捷过滤（既看得到 repo，也不丢掉非 repo 文件）。

### DQ-0002 数据源：本地直读 vs 经 fs 服务
- **A. 新后端直读本地**（console 与工作区同机）：新增 `files`/`file` panel，走 `/api/panels/{id}`，无需隧道
- **B. 复用 `#fsOverlay`**：经 `teamx fs` + tunnel，链路长、依赖隧道在线

倾向 A（快、简单、无隧道依赖）；`#fsOverlay` 保留给「看**别人**机器」的场景。

### DQ-0003 安全边界
- 拒绝目录：`.teamx/`（含 mTLS 私钥）、`.git/`？还是 `.git` 只读展示？
- 是否拒绝常见密钥/凭据文件（`*.pem`、`*.key`、`.env`、`id_rsa`）？
- 路径逃逸 / 符号链接逃逸 / 绝对路径注入
- **只读**（不提供写/删），与 fs 服务的 upload 分离
- 是否允许浏览 workspace 之外？否。

### DQ-0004 预览能力与上限
- 类型：纯文本 / 代码 / markdown（复用 `mdRender`）/ 图片（内联）/ 二进制（仅下载）
- 代码是否要**语法高亮**？（引入前端库 vs 纯 `<pre>`）
- 大小上限：读盘上限、预览上限、截断提示
- 行数上限（避免超长文件卡死浏览器）

### DQ-0005 与现有 `#fsOverlay` 的关系
- 抽公共组件（一份 JS 两处用）vs 各写各的
- 菜单命名：`File`（用户指定）——是否用 `Files`？面板 id `files`？

### DQ-0006 repo 发现与排序
- 扫 `.git` 的深度与排除（`node_modules/`、`target/`、`vendor/`）
- 排序：最近修改 / 字母 / git 状态（分支、dirty）
- 是否显示 git 元信息（当前分支、未提交变更数）

## Settled Decisions

（Consensus Mode：每题一条；结论消息见 discussion `dsc_18d55ee86a6404d11de92d`）

- **DQ-0001 浏览根与作用域**：**A + 顶部 repo 快捷过滤**；浏览根**锚定 opencode 会话 cwd**（可复现），cwd 之外的兄弟目录默认不展开（防整盘漫游）。— rationale: console 与工作区同机、用户心智是「这台机器上我有哪些文件」；dissent: 无；unanimity: **unanimous**；discussion: `#1801`（结论 `#1851`）。

- **DQ-0002 数据源**：**A 本地直读**（新增 `files`/`file` panel 走 `/api/panels/{id}`）；B 仅留给 `#fsOverlay` 看远端机器。**硬要求**：后端必须**直接调用 `fs.rs` 的同一个路径校验函数**（不复制），否则两套校验必然漂移。— rationale: 同机直读零额外信任边界、隧道多一跳+一层鉴权+依赖在线；dissent: 无；unanimity: **unanimous**；discussion: `#1802`（结论 `#1853`）。

- **DQ-0003 安全边界**：① `.teamx/` 必拒（mTLS 私钥/letters）；② `.git/` **只读展示**但 `objects`/`hooks` 等**折叠不可展开**；③ 密钥文件（`*.pem`/`*.key`/`.env`/`id_rsa`/`**/letters/`）**仅元数据** + `sensitive` 标记、拒绝预览；④ 路径/符号链接/绝对路径注入**复用 fs.rs 防护**；⑤ **只读**无写端点；⑥ workspace 外拒绝；⑦ markdown 预览**必须走 `mdRender` 白名单**。— rationale: .teamx 泄露即身份失陷、误点密钥风险 > 排查价值、mdRender 已审过 XSS 面；dissent: 无；unanimity: **unanimous**；discussion: `#1803`（结论 `#1854`）。
  - **⚠️ 修订 1（owner，2026-09-15）**：① 改为 —— **`.teamx` 可浏览**，但凭据类条目在任意深度隐藏；其余（`docs/`、`TEAM.md`、`instance.json`、`console.json`、`nudge-cursor`、`repos/`）正常浏览与预览。理由：`.teamx` 里存着团队文档与 `TEAM.md`，一律拒绝会让「文档浏览」这一核心诉求落空。
  - **⚠️ 修订 2（owner，2026-09-15）**：③ 由「仅元数据 + `sensitive` 标记」**升级为「一律隐藏、不可浏览」**，且规则**全局生效**（不再只作用于 `.teamx`）。新增 `fs::is_private()`：
    - 私有目录：`.ssh`、`.aws`、`.gnupg`、`.docker`、`.kube`、`secrets`、`.secrets`、`credentials`
    - 精确名：`.npmrc`、`.pypirc`、`.netrc`、`.git-credentials`、`.htpasswd`、`.pgpass`、`auth.json`、`kubeconfig`、`console-server.json`、`forwards.json`、`id_rsa`、`id_ed25519`
    - 前缀：`.env*`、`id_*`、`credentials*`、`secrets*`、`service-account*`
    - 后缀：`.pem`、`.key`、`.crt`、`.cer`、`.p12`、`.pfx`、`.jks`、`.keystore`、`.kdbx`、`.ovpn`、`.token`、`.secret`、`.letter`、`.db`、`.db-wal`、`.db-shm`、`.sqlite`、`.sqlite3`
    - `.git/config` 一并折叠（remote URL 可能内嵌 token）
    - 理由：owner 要求「所有隐私/私有信息都不可浏览」；宁可漏列一行，不可泄露一个密钥。

- **DQ-0004 预览能力与上限**：文本/代码 `<pre>` 等宽 + 行号；markdown 复用 `mdRender`；图片内联 PNG/JPG/GIF（**SVG 仅下载**）；二进制仅下载；**不引入语法高亮库**（自包含单二进制 + CSP 面）；上限 **1 MiB 读盘 / ~5000 行 / 图片 5 MiB**，超限截断提示下载。— rationale: 等宽+行号已覆盖看代码、highlight.js 增体积与攻击面、SVG 当 HTML 渲染即 XSS；dissent: 无；unanimity: **unanimous**；discussion: `#1804`（结论 `#1855`）。
  - **⚠️ 修订 1（owner，2026-09-17）**：预览能力整体放宽，**四项同时改**：
    1. **引入 highlight.js**（v11.9.0 common build，36 语言）内嵌进 `console_html.rs` 的 `HLJS_JS`/`HLJS_CSS` 常量——仍是**自包含单二进制、零外部请求**，只是体积 +120 KB。后端 `console_data::highlight_lang()` 按扩展名给出语言 id（`lang` 字段），前端由 `MutationObserver` 自动高亮任何进入 DOM 的 `pre code`（文件预览 + 事件/讨论/文档里的 markdown 围栏一并受益）；代码预览改为「行号 gutter 列 + 代码列」两栏，行号不再随横向滚动跑掉。
    2. **HTML 改为 `<iframe>` 预览**（原为当文本显示源码）。安全靠**沙箱**而非禁用：iframe 带 `sandbox="allow-scripts"`（**不含 `allow-same-origin`**），且 `/api/file` 对 HTML 响应加 `Content-Security-Policy: sandbox allow-scripts`（直接新标签打开同样受保护）。脚本可跑，但处于不透明源，读不到 console 的 cookie（本身也 `HttpOnly`）也读不回同源 `/api/*` 响应。
    3. **视频（mp4/m4v/webm/mov/ogv）在线播放**：`/api/file` 新增**单区间 `Range` 支持**（206 + `Content-Range` + `Accept-Ranges: bytes`，越界 416），按需 seek 读取而非整文件入内存，`<video controls preload="metadata">` 因此可拖动进度条。
    4. **SVG 放开预览**（原「仅下载」）：`<img>` 中的 SVG **不会执行脚本**，故与位图共用预览路径；同时 `/api/file` 对 SVG 响应加 `Content-Security-Policy: sandbox`（不含 `allow-scripts`），使得「直接导航到该 SVG」这一路径也不执行脚本。图片一律改走 `/api/file` 原始端点（不再 base64 data URI），上限由 5 MiB 提到 32 MiB；图片扩展名扩到 png/jpg/jpeg/gif/webp/bmp/ico/avif/svg。
    - 顺带清理：删除 `console_data.rs` / `console_html.rs` 中各一处**不可达的第二个 `if ext == "pdf"` 分支**（及其唯一调用者 `find_pdf_text`）——PDF 早已在前一个分支全部 return，旧「PDF 文本抽取预览」路径是死代码。

- **DQ-0005 与 `#fsOverlay` 的关系**：**分层抽取** —— 后端复用 fs.rs 函数；前端渲染/预览 JS 抽共享函数（移动非重写，约半天）；**数据获取层不共享**（File→`/api/panels/{id}`，fsOverlay→`/api/cloud/{id}`）。命名：菜单 `File`、`data-view=files`、panel id `files`。— rationale: 避免本地/远端语义耦合进公共组件；dissent: 无；unanimity: **unanimous**；discussion: `#1805`（结论 `#1856`）。

- **DQ-0006 repo 发现与排序**：cwd 为根扫 `.git`、**深度 3**，排除 `node_modules`/`target`/`vendor`/`dist`/`.teamx`；**repo 数量上限**（超限列前 N + 提示）；默认**最近修改**排序（**git HEAD 提交时间优先、目录 mtime 兜底**，口径写进文档），可切字母序；显示**当前分支**（读 `.git/HEAD`，symref 解析，detached 显示短 sha）；dirty **异步懒加载**、不作排序键。— rationale: 深度 3 覆盖 submodule、分支零成本、dirty 需逐 repo 调用不宜同步；dissent: 无；unanimity: **unanimous**；discussion: `#1806`（结论 `#1857`）。

## Current Frontier

（无。DQ-0001…0006 全部收敛，均为 unanimous。）

## Remaining Branches

- **实现任务**（建议拆 3 片）：
  1. 后端 `files`/`file` panel + 复用 fs.rs 校验（含安全用例）
  2. 前端 File 菜单 + 渲染函数抽取（File / fsOverlay 共用）
  3. repo 发现/排序/分支显示 + repo 数上限
- **无关但顺带发现的 footgun**：`teamx worker --opencode-url` 默认 `http://127.0.0.1:4199`，多工作区节点极易 attach 到错误的 opencode（本次 caicai 即因此看到 powernano 团队）。建议改为必填或从 `TEAMX_HOME` 推导，并记入 docs。
- **grill 参与度**：本轮 2/5 在线（weiye-mini、caicai）；weiye 离线未参与，可后补 review。

