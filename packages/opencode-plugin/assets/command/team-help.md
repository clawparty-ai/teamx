---
description: teamx 帮助（列出全部子命令与扁平别名）
agent: teamx
---

列出 teamx 的全部子命令与扁平别名命令，简要说明用途。额外参数: $ARGUMENTS

角色命令说明（重点）：
- 固定角色：owner/observer/supervisor/contributor/subtask-implementer/reviewer，任意成员可直接 `role set <role>` 选择。
- 自定义角色：`role propose <key> <label> [desc]` 提议自己的 job role（需与固定角色不同名）→ owner `role approve <key>` 审批（自动授予提议者）或 `role deny <key>` 拒绝 → owner 可用 `role update <key> --description ...` 修改任何角色描述。

在子命令列表之后，输出一段 teamx 协作流程的 ASCII 图（放在 ```text 代码块中），帮助理解工作原理。参照下方「参考风格」输出：横向流水线、owner/member 双泳道，状态用 [ 方括号 ] 标注 team/goal 状态机，箭头 ──► 串联主流程，用 │ ▼ ▲ 表示分支与流向，行尾可附中文注释。按当前真实团队状态替换图中对应 [ 状态 ] 标记（可先调用 teamx_status 确认当前状态），并在最后一行输出「● 当前阶段」标注当前阶段含义与下一步建议。保持对齐、单行不超过 80 字符。

参考风格（图中 `[状态]` 为示意，请按实际状态替换；● 行要按真实状态更新）：

```text
                    teamx 协作流程图 (Team Collaboration)
                    ═════════════════════════════════════════════

   owner:  [create_team] ──► [set_goal] ──► [share_goal] ──► [close_goal] ──► [archive]
           (forming)          (proposed)    (in_progress)    (achieved)       (archived)
              │                  │               │    ▲            │               │
              ▼                  ▼               ▼    │            ▼               ▼
   member:  invite_token      join         [active]◄──┘        [achieved]      [completed]
             分享令牌        [pending]          │ 协作循环       (member 报告)   (owner 验证)
                                             ▼
                              progress / ask / respond / decision / update
                              blocked / resumed / refine

   ● 当前阶段: forming（招募中）—— 目标尚未共享，下一步: /team goal share
```
