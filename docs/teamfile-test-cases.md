# teamx TEAM.md 团队初始化 — 测试案例（Test Cases）

约定：
- 每条用例给出自动化落点（`cargo test` 的测试名 / `tests/teamfile-test.sh` 的步骤 / 手工）。
- CLI 用例：`export TEAMX_HOME=$(mktemp -d)`；在临时项目目录下放置 `.teamx/TEAM.md`。

## A. 单元测试（`cargo test` — teamfile.rs）

| ID | 名称 | 步骤 | 预期 | 落点 |
|---|---|---|---|---|
| TF-001 | 完整 TEAM.md 解析 | 含标题/背景/目标/3 成员（owner+contributor+reviewer） | TeamFile 字段齐全：team_name、background、goals、members[3]（各含 name/role/desc/skills/outputs） | `teamfile::tests::parse_full_team_file` |
| TF-002 | 中文章节/字段 | 用中文 `姓名/角色/分工/技能/输出` | 正确解析 | 同上（中英双语字段） |
| TF-003 | 缺失成员小节 | 无 `## 成员` 章节 | members 为空数组，不报错 | `teamfile::tests::no_members_section_ok` |
| TF-004 | 缺失标题 | 文件首行不是 `# ` | team_name 为空，不 panic | `teamfile::tests::missing_title_ok` |
| TF-005 | 空文件/不存在 | 空字符串 / 路径不存在 | 返回 Err（调用方告警降级） | `teamfile::tests::empty_file_errors` |
| TF-006 | 成员字段缺失 | 成员小节只有 `### 小明` 无字段行 | display_name=key，其余 None/空 | `teamfile::tests::member_minimal` |
| TF-007 | 目标多行 | `## 目标` 下列出 3 个 `- ` 项 | goals 为 3 元素数组 | `parse_full_team_file` |

## B. CLI 集成测试（`tests/teamfile-test.sh`）

| ID | 名称 | 步骤 | 预期 |
|---|---|---|---|
| TF-101 | 无 TEAM.md 保持原行为 | 空项目 `teamx team create "T" --session s:owner` | 原创建流程，无 members 目录生成 |
| TF-102 | 有 TEAM.md 自动初始化 | 项目含 `.teamx/TEAM.md`（2 成员），`team create` | 输出含 goal_id + 每成员 letter 路径；`.teamx/members/<name>/` 目录存在 |
| TF-103 | 成员 AGENTS.md 生成 | 检查 `.teamx/members/小明/AGENTS.md` | 内容含角色/分工/技能/输出；若项目根有 AGENTS.md 则含其内容 |
| TF-104 | letter 双输出 | 检查打印输出 + `.teamx/members/小明/invitation.letter` 文件 | 打印含 `teamx-inv:v1:`；文件内容一致 |
| TF-105 | letter 可导入 | 用生成的 letter 执行 `teamx team import <letter> --name 小明 --session s:xiaoming` | 导入成功，座位 pending，member_id 匹配 |
| TF-106 | 项目根 AGENTS.md 融合 | 项目根放 AGENTS.md + TEAM.md，创建后检查成员 AGENTS.md | 成员 AGENTS.md 包含项目根 AGENTS.md 内容 |
| TF-107 | 无效 TEAM.md 降级 | TEAM.md 为空/格式错，`team create` | 不阻塞，创建成功，输出告警 |
| TF-108 | owner 成员处理 | TEAM.md 含 owner 成员 | owner 不再单独签发 letter（owner 已有会话），或按配置处理 |

## C. 回归（并入 `tests/run-all.sh`）

| ID | 名称 | 步骤 | 预期 |
|---|---|---|---|
| TF-201 | 现有套件不回归 | `./tests/run-all.sh`（含 smoke/cli/concurrency/network/tunnel） | 全绿（无 TEAM.md 场景不受影响） |

## D. 手工验收

| ID | 名称 | 步骤 | 预期 |
|---|---|---|---|
| TF-301 | 真实初始化演示 | 项目放 TEAM.md（3 成员），`team create`，观察输出与目录 | 目录/letter/AGENTS.md 齐全；letter 可分发给成员导入 |
