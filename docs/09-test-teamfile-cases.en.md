# teamx TEAM.md Team Initialization — Test Cases

Conventions:
- Each case gives its automation target (a `cargo test` test name / steps in `tests/teamfile-test.sh` / manual).
- CLI cases: `export TEAMX_HOME=$(mktemp -d)`; place `.teamx/TEAM.md` inside a temporary project directory.

## A. Unit Tests (`cargo test` — teamfile.rs)

| ID | Name | Steps | Expected | Target |
|---|---|---|---|---|
| TF-001 | Full TEAM.md parsing | a file containing title/background/goals/3 members (owner+contributor+reviewer) | TeamFile fields complete: team_name, background, goals, members[3] (each with name/role/desc/skills/outputs) | `teamfile::tests::parse_full_team_file` |
| TF-002 | Chinese sections/fields | using the Chinese labels `姓名/角色/分工/技能/输出` | Parsed correctly | Same as above (bilingual CN/EN fields) |
| TF-003 | Missing members section | no `## 成员` section | members is an empty array, no error | `teamfile::tests::no_members_section_ok` |
| TF-004 | Missing title | the file's first line is not `# ` | team_name is empty, no panic | `teamfile::tests::missing_title_ok` |
| TF-005 | Empty file / nonexistent path | empty string / path does not exist | Returns Err (caller warns and degrades) | `teamfile::tests::empty_file_errors` |
| TF-006 | Member fields missing | member subsection contains only `### 小明` with no field lines | display_name=key, everything else None/empty | `teamfile::tests::member_minimal` |
| TF-007 | Multi-line goals | 3 `- ` items listed under `## 目标` | goals is a 3-element array | `parse_full_team_file` |

## B. CLI Integration Tests (`tests/teamfile-test.sh`)

| ID | Name | Steps | Expected |
|---|---|---|---|
| TF-101 | No TEAM.md keeps original behavior | empty project, `teamx team create "T" --session s:owner` | Original creation flow, no members directories generated |
| TF-102 | Auto-init with TEAM.md | project contains `.teamx/TEAM.md` (2 members); `team create` | Output includes goal_id + each member's letter path; `.teamx/members/<name>/` directories exist |
| TF-103 | Member AGENTS.md generated | inspect `.teamx/members/小明/AGENTS.md` | Content includes role/duties/skills/outputs; if the project root has an AGENTS.md, its content is included |
| TF-104 | Letter dual output | check the printed output + the `.teamx/members/小明/invitation.letter` file | Print contains `teamx-inv:v1:`; file content identical |
| TF-105 | Letter importable | run `teamx team import <letter> --name 小明 --session s:xiaoming` with the generated letter | Import succeeds, seat pending, member_id matches |
| TF-106 | Project-root AGENTS.md merge | put AGENTS.md + TEAM.md at the project root; after creating, inspect the member AGENTS.md | The member AGENTS.md contains the project-root AGENTS.md content |
| TF-107 | Invalid TEAM.md degrades gracefully | TEAM.md empty/malformed; `team create` | Not blocked, creation succeeds, a warning is printed |
| TF-108 | Owner member handling | TEAM.md contains an owner member | No separate letter issued for the owner (the owner already has a session), or handled per configuration |

## C. Regression (merged into `tests/run-all.sh`)

| ID | Name | Steps | Expected |
|---|---|---|---|
| TF-201 | Existing suite without regression | `./tests/run-all.sh` (including smoke/cli/concurrency/network/tunnel) | All green (scenarios without TEAM.md unaffected) |

## D. Manual Acceptance

| ID | Name | Steps | Expected |
|---|---|---|---|
| TF-301 | Real initialization demo | place a TEAM.md (3 members) in a project; `team create`; observe the output and directories | Directories/letters/AGENTS.md all present; letters can be handed to members for import |
