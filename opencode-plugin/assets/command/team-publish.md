---
description: teamx 汇报/广播（progress/decision/update/blocked/resumed/achieved/refine；可选 --assignee 定向分派）
agent: teamx
---

用户要发布 teamx 事件。事件类型: $1，可选数据: $2。调用 teamx_publish（type=$1, data=$2 或 {"message":...}）。合法类型：progress / decision / update / blocked / resumed / achieved / refine / start / activity。

**定向分派（assignee）**：当 owner 要把"某件事"明确分派给某个成员执行时，传 assignee（member id）：
- `teamx_publish {type: "decision", data: {message: "请完成 X"}, assignee: "<member_id>"}`
- 带 assignee 的事件会在 payload 标记 `assignee_member_id`/`assignee_name`，**只有该成员会自动执行**；其他成员收到的是普通广播，不自动执行。
- 不带 assignee 的 publish 是普通广播（信息性），不触发任何成员自动执行。
