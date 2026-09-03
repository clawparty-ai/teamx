---
description: teamx 打开成员 Web 控制台（teamx console，自动开浏览器）
agent: teamx
---

用户要打开 Teamx Console（成员本地只读 Web 工作台）。调用 teamx_console（port 默认 9528；如已在运行会直接重新打开现有 URL）。

启动成功后将 URL（`https://127.0.0.1:9528/?token=…`）展示给用户，并提示：
- 控制台只读显示当前工作空间（.teamx）里的团队信息，与 teamx CLI 一致；
- Cloud 面板使用前需先 `teamx tunnel forward <name> --local-port <port>` 把云节点映射到本地，再添加 `http://127.0.0.1:<port>` 节点即可 iframe 内嵌。
