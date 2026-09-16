# dsh 插件工具对齐——opencode 侧差距核查与补齐（task-9）

> 状态：本 PR（feat/dsh-plugin-tool-alignment）补齐全部缺口；本文为缺口表存档。

## 口径

以 opencode-plugin/src/tools.ts（65 个 teamx_* 工具）为基准，对比 dsh-plugin/src/tools.ts（27 个），
差异 = opencode 有而 dsh 无的 38 个工具。同名工具的参数/语义两端一致（同一套 teamx CLI/RPC），
故缺口为**纯增量补齐**，不改动既有 27 个工具。

## 缺口表（38 个，本 PR 全部补齐）

| 组 | 工具 | dsh 实现 |
|---|---|---|
| Tasks (10) | task_create / ack / claim / update / help / done / verify / reject / list / log | runCli（network=RPC / local=spawn，同 runCli 既有路由）；task/git 已加入 cliArgsToRpc 的 GROUPED + slots |
| Git (11) | git_create / delete / list / grant / permissions | runCli（RPC）；git.create/delete/grant/permissions slots 已加 |
| Git (6, local) | git_clone / pull / push / setup / commit / commit_push | **runCliLocal**（新增导出）：强制本地 spawn，不走 RPC——与 opencode txLocal 语义一致（本地 checkout/git config 不得发到服务端） |
| Serve (4) | serve_start / status / stop / token | 新模块 serve-lifecycle.ts（node:child_process 移植 opencode serve.ts）；token 按 opencode 同款标注 deprecated（mTLS I1 取代） |
| Console (3) | console / console_status / console_stop | serve-lifecycle.ts（移植 opencode console.ts：token 捕获/端口回退/浏览器打开） |
| Tunnels (6) | tunnel_list / status / close / direct | runCli（RPC）；tunnel.close 同步清除持久化记录 |
| Tunnels (2) | tunnel_expose / forward | **tunnel.ts 移植**（510 行 WS 反向隧道运行时，`ws` 包替代 Bun WebSocket；mtlsFor 改异步 await；tunnels-store.ts 持久化重启恢复） |
| Misc (4) | inbox / loopx_report / team_promote_lead / team_demote_lead | runCli（inbox 默认 --ack；loopx 回退 session 绑定项目） |

## 路由口径（dsh runCli / runCliLocal / 生命周期模块）

- **runCli（网络=RPC）**：task.*（taskx 文档由服务端管理，与 opencode tx() 相同）、
  git create/delete/list/grant/permissions、tunnel list/status/close/direct、team inbox/promote-lead/demote-lead、
  loopx report。`task`/`git` 已加入 client.ts 的 GROUPED 集 + slots 表（此前缺失会把位置参数静默丢弃）。
- **runCliLocal（强制本地，新增）**：git clone/pull/push/setup/commit/commit-push —— 触及本地
  checkout / git config / mTLS letter，语义上不得发往服务端（对齐 opencode txLocal）。
- **生命周期模块**：serve/console 子进程管理（本地 spawn），无 CLI/RPC。

## dsh 侧适配差异（有意为之）

1. **tunnels-store 存储位置**：opencode 按 workspace（`<cwd>/.teamx/`）持久化隧道；dsh 实例是
   per-host 的，统一存 `<TEAMX_HOME>/tunnels.json`、`forwards.json`。
2. **mtlsFor 异步**：dsh 的 mtlsFor 返回 Promise，tunnel.ts 的 connect/forward 内 await。
3. **WebSocket**：`ws` npm 包（dsh 既有依赖）替代 Bun 全局 WebSocket；事件式 API（.on('message')）
   与 mTLS options（{ca, cert, key, rejectUnauthorized}）对齐 dsh ws.ts 既有写法。
4. **子进程**：node:child_process spawn 替代 Bun.spawn；serve detached+unref，console 捕获 stderr token。
5. **task_create no_push / task_list mine / git_setup local / inbox ack / console open**：dsh 工具参数
   目前为 string 型，布尔语义用 "1"/"true"/"false" 字符串约定（与 dsh 既有工具的参数风格一致）。

## 验证

- `tsc --noEmit`：新增文件 0 错误（仅存量的 @deepseek-ai/* peer-dep TS2307 基线，合并前既如此）。
- 行为面：所有新工具均为 CLI/RPC 透传或 opencode 已验证逻辑的忠实移植；runCliLocal 路由与
  opencode txLocal 一致；tunnel 运行时逐行移植（wire format 不变）。

## 后续（非本 PR）

- tunnel 运行时的真机 E2E（expose→forward 回环）建议在两台节点间实测一次。
- task-7 遗留的 `taskx_task_*` 系列如 owner 侧有不同命名偏好，可统一改；工具名已与 opencode 对齐为
  `teamx_task_*`。
