---
description: teamx 查看当前团队状态
agent: teamx
---

用户要查看 teamx 团队状态。先调用 teamx_sync 拉取最新事件，然后调用 teamx_status 展示完整状态（团队/目标/成员/角色/待答问题）。如果当前 session 属于多个团队，用 --team 指定目标团队。额外参数: $ARGUMENTS

如果发现有待审批的成员（state 为 pending），**只提示、不自动批准**：列出待审批成员并说明 owner 可执行 `approve <member_id>` 或 `deny <member_id>` 决策，绝不擅自调用 teamx_approve / teamx_deny。

展示完状态后，在输出末尾用等宽 ASCII 流程图（放在 ```text 代码块中）直观说明 teamx 团队协作工作原理。不用 mermaid。参照下方「参考风格」输出：横向流水线、owner/member 双泳道，状态用 [ 方括号 ] 标注 team/goal 状态机，箭头 ──► 串联主流程，用 │ ▼ ▲ 表示分支与流向，行尾可附中文注释。按当前真实团队状态替换图中对应 [ 状态 ] 标记，并在最后一行输出「● 当前阶段」标注当前阶段含义与下一步建议。保持对齐、单行不超过 80 字符。

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
