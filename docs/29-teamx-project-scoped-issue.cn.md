# teamx 多会话证书混乱问题：根因与解决路线

> 状态：已修复（设计 v3：项目级 teamx 空间）
> 相关提交：`80281bf` feat(scope): teamx 空间改为项目级 `<cwd>/.teamx`
> 目标读者：维护者 / 遇到「import 后无法连接 server / 证书错乱」的用户

---

## 1. 现象

- 成员在自己的 opencode 中执行 `/team import <邀请函>` 后，**无法连接 teamx server**；
- 多个 opencode 会话之间出现**证书/身份混乱**：可能以错误的成员身份连接、或连到已吊销/已销毁团队的邀请；
- 仅「重启 opencode」并不能稳定解决问题（旧设计下选择证书的启发式不确定）。

## 2. 根因（旧设计缺陷）

旧设计把「机器级身份」放在全局 `~/.teamx/`，并让插件按模块级常量解析服务器地址，导致 6 个缺陷：

1. **身份材料全局化**：`team import` 把邀请函与 mTLS 证书写入全局
   `~/.teamx/letters/<invitation_id>/`，与具体项目无关；同一台机器所有项目/会话共用一份证书池。

2. **插件模块级常量**：`TEAMX_SERVER_URL` 在插件模块加载时一次性解析
   （`env || 扫全局 letters || ""`）。import 发生在插件加载之后 → 常量早已固定 → 插件仍处于
   **本地模式**，后续调用全走本地 DB，连不上服务器；必须重启 opencode 才能进入网络模式。

3. **同 host 多 letter 时证书选择不确定**：`letterMtls()` / `readStoredLetter()` 采用
   「readdir 顺序第一个 host 匹配」即返回，而非「最新导入优先」。同一服务器多份邀请函时
   （如旧团队已吊销邀请 + 新邀请），可能选中旧/吊销证书。

4. **URL 与证书可能来自不同 letter**：`discoverServerUrl()`（mtime 最新）与 `letterMtls()`
   （readdir 首匹配）启发式不一致 → 用 A 的地址、B 的证书 → 身份错配。

5. **全局 current.json 指针**：每次 import 覆盖机器级 `~/.teamx/letters/current.json`，
   一个会话的导入影响所有会话。

6. **服务端只认证书 CN**：网络模式下身份 = 证书 CN 的 member_id，`session` 参数被忽略；
   一台机器所有会话 = 同一成员，无法按会话区分身份。再导入新邀请也不会干净切换身份，
   旧 letter 残留会参与选择。

外加一个流程缺口：

7. **已配置服务器时 import 不落盘**：设置了 `TEAMX_SERVER_URL` 后，`team import` 走 RPC 到
   服务器认领席位，本地不保存证书 → 后续 RPC 无 mTLS 材料可用。

## 3. 解决路线（设计 v3：项目级 teamx 空间）

核心原则：**每个 opencode 实例 = 一个 workspace = 一个 `<cwd>/.teamx/` 空间，一项目一身份**。

| 数据 | 旧（全局） | 新（项目级） |
|---|---|---|
| 邀请函 + mTLS 证书 | `~/.teamx/letters/` | `<cwd>/.teamx/letters/` |
| 本地 DB | `~/.teamx/teamx.db` | `<cwd>/.teamx/teamx.db` |
| instance.json | 全局 | `<cwd>/.teamx/` |
| serve.json / tunnels.json | 全局 | `<cwd>/.teamx/` |
| server URL 发现 | 扫全局 letters | 扫 `<cwd>/.teamx/letters/` |

落地要点：

1. **Rust CLI**：`db::teamx_home()` 默认 `<cwd>/.teamx`（`TEAMX_HOME` 显式覆盖）。
   本地 DB、CA、letters、instance.json 全部随 workspace。
2. **插件**：
   - `TEAMX_SERVER_URL` / `TEAMX_HOME` 从模块常量改为函数按 cwd 解析
     （`serverUrlFor(cwd)` / `teamxDir(cwd)`）；
   - 所有 `teamx_*` 工具把 `context.directory` 传给 `runCli`，按 workspace 解析服务器 URL 与 mTLS 证书；
   - `index.ts` 建立 session→directory 映射；后台任务（digest 轮询 / WS / presence / activity）
     按会话目录解析；
   - `tunnels.json` / `serve.json` 按 workspace 存放。
3. **import 强制本地**：`team import` / `git setup` 走本地 CLI（`forceLocal`），确保邀请函 +
   mTLS 证书写入 `<cwd>/.teamx/letters/`（即便已配置服务器地址）。
4. **服务器部署**：`teamx serve`（systemd）固定 `TEAMX_HOME`（保留既有 CA）；服务器侧 CLI
   命令同样固定 `TEAMX_HOME`，保证 CA/DB 一致。
5. **清理**：旧全局 `~/.teamx/letters/` 移到备份目录，不再使用；一项目一身份，
   消除跨会话证书选择不确定性。

## 4. 验证

- `cargo build` / `cargo test -p teamx` 133 全绿；插件 `tsc` + `bun run build` 通过；
  协议资产测试 `tests/protocol-assets.test.mjs` ALL PASS；
- 本地二进制（`~/.local/bin/teamx`）与全局插件（`~/.config/opencode/plugins/teamx.js`）已更新；
- 云服务器新二进制已安装（服务器源码编译），systemd 固定 `TEAMX_HOME`，`/health` mTLS 200；
- 端到端：邀请函预置于 `<项目>/.teamx/letters/<id>/`，`team.import` RPC 认领席位成功 →
  成员 active + co-lead；用户重启 opencode 后正常连接。
