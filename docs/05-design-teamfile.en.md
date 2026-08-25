# teamx TEAM.md-Driven Team Initialization (Design)

> Status: **in design** (main branch)
> Intended readers: implementers, team lead (owner), team members
> Related: `crates/teamx/src/commands.rs` (cmd_team_create/cmd_team_invite), `crates/teamx/src/pki.rs` (letter issuance), `opencode-plugin/src/serve.ts` (built-in serve), opencode's `AGENTS.md` convention

---

## 0. Goal

Provide a **`TEAM.md`** file inside the project repository describing the project's background, goals, team members, and each member's role/responsibilities/skills/work outputs. When a team is created:

1. If `.teamx/TEAM.md` exists, **read and parse it automatically**;
2. Based on the file contents, **initialize the project automatically**:
   - Create the team (team name, goal);
   - Start the plugin's built-in `teamx serve` (network-mode server);
   - Issue an **invitation letter** for each member;
   - Generate a dedicated **AGENTS.md** per member (merging the project-root `AGENTS.md` with that member's description in TEAM.md);
   - Create a working directory **`.teamx/members/[member-name]/`** for each member.

---

## 1. Core Decisions (confirmed)

| Decision point | Conclusion |
|---|---|
| Trigger timing | Detect `.teamx/TEAM.md` at `team create`; **how other commands reuse TEAM.md is left as TODO** (owner to design separately) |
| Working directory | Use **`.teamx/members/[member-name]/`** directly (no workspace subdirectory introduced) |
| Member AGENTS.md | **Merge project-root `AGENTS.md` + that member's description from TEAM.md**, generating a member-specific AGENTS.md |
| Letter handling | **Both save to file and print** (save to `.teamx/members/[name]/invitation.letter`, and also print via CLI output) |
| Starting serve | Reuse the plugin's `serveStart` (serve.json already recorded inside the `.teamx` directory); CLI side prints a hint |

---

## 2. TEAM.md File Format

File location: **project root `.teamx/TEAM.md`**.

```markdown
# Enterprise Digital Platform

## Background
Build a team-collaboration platform around the Team Goal: task assignment, reverse tunnel, activity/cost analysis.

## Goals
- Deliver v1.0 by end of August: team collaboration and activity analysis
- Support cross-network reverse tunnel

## Members
### owner
- Name: Enterprise Digital Platform
- Role: owner
- Responsibilities: architecture design, goal definition, code review
- Skills: Rust, TypeScript, system design
- Outputs: architecture docs, core code

### Xiaoming
- Name: Xiaoming
- Role: contributor
- Responsibilities: frontend development, testing
- Skills: React, TypeScript
- Outputs: kanban components, test cases

### Xiaohong
- Name: Xiaohong
- Role: reviewer
- Responsibilities: code review, quality assurance
- Skills: Rust, code review
- Outputs: review reports
```

### 2.1 Parsing Rules

- **Team name**: the `# heading` (the first `# ` line).
- **Background**: body of the `## 背景` section.
- **Goals**: list items of the `## 目标` section (lines starting `- `), joined into the goal body (or the first item becomes the goal title).
- **Members**: each `### <key>` subsection under the `## 成员` section. Within a subsection, `- field: value` lines:
  - `姓名` / `name` → display_name
  - `角色` / `role` → role key (contributor/reviewer/observer/…)
  - `分工` / `description` → role description
  - `技能` / `skills` → skill list (used in AGENTS.md)
  - `输出` / `outputs` → work outputs (used in AGENTS.md)
- Lenient parsing: field names support both Chinese and English (`姓名`/`name`, `角色`/`role`, `分工`/`description`, etc.); missing fields may be empty.

### 2.2 Data Model

```rust
pub struct TeamFile {
    pub team_name: String,
    pub background: Option<String>,
    pub goals: Vec<String>,
    pub members: Vec<MemberProfile>,
}

pub struct MemberProfile {
    pub key: String,          // `### key` (directory name)
    pub display_name: String, // name
    pub role: Option<String>, // role key
    pub description: Option<String>, // responsibilities
    pub skills: Vec<String>,  // skills
    pub outputs: Vec<String>, // outputs
}
```

---

## 3. `team create` Flow Enhancement

When `teamx team create <name> [--session S]` detects that `.teamx/TEAM.md` exists:

```
1. Parse TEAM.md → TeamFile
2. Create the team (reuse existing cmd_team_create; team name = TEAM.md title, or overridden by CLI name)
3. Auto goal set:
   - title = team name (or TEAM.md's first goal)
   - body = background + all goals
4. Start built-in teamx serve (hint / plugin automatic)
5. Iterate over each member:
   a. Issue invitation letter (reuse cmd_team_invite; role/desc from TEAM.md)
      - Save to .teamx/members/[member-name]/invitation.letter
      - Print the letter to CLI output
   b. Generate member-specific AGENTS.md:
      - Read project-root AGENTS.md (if present)
      - Merge that member's role/responsibilities/skills/outputs from TEAM.md
      - Write .teamx/members/[member-name]/AGENTS.md
   c. Create working directory: .teamx/members/[member-name]/ (directory already exists)
6. Output: team id, goal id, each member's (name, role, letter path)
```

### 3.1 Member AGENTS.md Content (merged)

```
# AGENTS.md — <display_name> (<role>)

## From project-root AGENTS.md
<contents of the project-root AGENTS.md (if present)>

## Team Role
- Role: <role>
- Responsibilities: <description>
- Skills: <skills>
- Work outputs: <outputs>

## Team Context
- Team: <team_name>
- Member directory: .teamx/members/<name>/
- Working style: sync progress via teamx tools, review team events
```

---

## 4. Directory Structure (generated result)

```
<project>/
├── .teamx/
│   ├── TEAM.md                    # team definition (source file, user-maintained)
│   ├── serve.json                 # embedded serve record (plugin already has it)
│   ├── teamx.db                   # local DB (for serve)
│   └── members/
│       ├── 小明/
│       │   ├── AGENTS.md          # Xiaoming's agent instructions (merged)
│       │   └── invitation.letter  # Xiaoming's invitation letter (pending import)
│       └── 小红/
│           ├── AGENTS.md
│           └── invitation.letter
└── AGENTS.md                      # project-root AGENTS.md (if present, merge source)
```

---

## 5. Implementation Modules

| Module | Responsibility |
|---|---|
| `crates/teamx/src/teamfile.rs` | TEAM.md parser (`parse_team_file(path) -> Result<TeamFile>`) + unit tests |
| `crates/teamx/src/commands.rs` | `cmd_team_create` integration: detect TEAM.md → parse via teamfile → generate goal/letters/AGENTS.md/directories |
| `crates/teamx/src/cli.rs` | `team create` takes no new arguments (auto-detect); extended output (letters/agents paths) |
| Plugin (optional) | `serveStart` exists; auto-start after creation (later TODO) |

---

## 6. Implementation Milestones

| Phase | Content | Acceptance |
|---|---|---|
| **T1** | `teamfile.rs`: parser + unit tests | Valid/invalid TEAM.md both handled correctly |
| **T2** | `cmd_team_create` integration: detect TEAM.md → generate goal + letters + AGENTS.md + directories | After creation `.teamx/members/*` complete |
| **T3** | CLI output: print each member's letter + file paths | Command output complete |
| **T4** | End-to-end test (smoke) | `tests/run-all.sh` all green |

---

## 7. Open Questions / Risks

| # | Question | Status |
|---|---|---|
| Q1 | How other commands reuse TEAM.md | **TODO** (owner brainstorming) |
| Q2 | Should letters be cleaned from members directories after import? | No cleanup for now (kept for audit) |
| Q3 | Must member roles be built-in roles? | Custom role keys allowed (reusing the role propose flow), written directly at creation |
| R1 | TEAM.md parsing tolerance | Missing sections/fields handled leniently, never blocking creation |
| R2 | Chinese filename directories | UTF-8 directory names supported (macOS/Linux) |
