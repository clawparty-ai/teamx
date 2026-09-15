---
status: in_progress
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

（待 grill 收敛）

## Current Frontier

DQ-0001 … DQ-0006（全部待答）

## Remaining Branches

（待补）
