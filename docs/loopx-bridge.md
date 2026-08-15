# teamx ↔ loopx 桥接（V1）

目标：**不重造 loopx 的轮子**。teamx 只做一层薄桥接：读取 loopx 的阶段性进度，把它作为 `loopx.progress` 事件发布进团队账本，让 team owner 通过 `teamx_sync` 就能看到各成员的 loopx 阶段进展。

## 原理

- loopx 是另一个独立的状态内核（Python CLI，长期任务的 goal/gate/todo/quota 控制面），teamx 不复制它的状态模型。
- member 在 `teamx team join` 时可选绑定 `--loopx_project <dir>`；绑定的目录即该 member 用 loopx 管理长任务的项目。
- `teamx loopx report <project>` 在 `<project>` 目录下执行 `loopx status --format json`，尽力抽取紧凑摘要，然后写入 `loopx.progress` 事件。
- owner 侧：每个回合 `teamx_sync` 会把该事件作为新事件返回，owner agent 据此广播团队进展。

## LoopxDigest 结构

```json
{
  "project": "/path/to/project",
  "available": true,
  "error": null,
  "goal_state": "active",
  "gate": "await owner decision",
  "next_todo": "implement auth",
  "quota": "eligible=yes",
  "raw": { "...": "原始 loopx status JSON" }
}
```

- `available=false` 时带 `error` 说明（loopx 未安装 / 项目未连接 / 非 JSON），**不写入事件**，返回 `{ok:false, note:"loopx unavailable; teamx core loop is unaffected"}` —— 不影响 teamx 自身闭环。
- 字段抽取是 best-effort：兼容 `active_goal_state`/`goal_state`/`state`、`gate`/`user_gate`、`next_todo`、`quota`/`quota_should_run` 等键；嵌套对象取其 `state/text/title/summary` 子字段或拼接标量。

## 插件侧

`teamx_loopx_report` 工具：
- 传 `project`：直接对该项目执行 `loopx report`。
- 不传 `project`：从当前会话绑定的 `loopx_project`（join 时设置）读取；未绑定则返回指引提示。

## 边界

- V1 只做**按需读取**（agent 调用 `teamx_loopx_report` 时执行 `loopx status`），不做文件监听、不做心跳轮询。
- loopx 的进度只读进 teamx 账本；teamx 不会向 loopx 写任何状态。
- loopx 字段 schema 变化不影响 teamx 闭环：抽取失败只会产生 `error` 提示，原始 JSON 保留在 `raw`。
