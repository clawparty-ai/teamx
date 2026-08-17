---
description: teamx 离开团队
agent: teamx
---

用户要离开 teamx 团队。先 teamx_sync + teamx_status 确认当前团队，再调用 teamx_leave。离开后该会话不再是团队成员，成员缓存失效。注意：**owner 不能离开**（没有所有权转移机制），owner 如需退出请用 `/team destroy`。额外参数: $ARGUMENTS
