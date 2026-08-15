# @teamx/opencode-plugin

opencode 插件：`/Team` 协作 agent + `teamx_*` 工具族。配合 `teamx` Rust CLI（见仓库根目录）使用。

## 结构

- `src/index.ts` — Plugin 入口（`tool:` 注册 + `event` hook 成员活动镜像）
- `src/tools.ts` — 17 个 `teamx_*` 工具定义
- `src/client.ts` — 统一 CLI 调用层（V2 换 HTTP 的唯一接缝）+ 成员缓存
- `assets/agent/teamx.md` — teamx agent 定义
- `assets/command/Team.md` — `/Team` 命令

## 构建

```bash
bun install && bun run build   # 产出 dist/teamx.js
```

## 安装

推荐用仓库根目录 `./install.sh`（会同时安装 Rust CLI + 三件套 + 按 opencode 版本 pin `@opencode-ai/plugin`）。也可手动：

```bash
cp dist/teamx.js        ~/.config/opencode/plugins/teamx.js
cp assets/agent/teamx.md   ~/.config/opencode/agent/teamx.md
cp assets/command/Team.md  ~/.config/opencode/command/Team.md
```

详见仓库根 `docs/` 与 `README.md`。
