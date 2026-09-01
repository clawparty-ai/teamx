# The teamx Methodology: From Team Definition to a Shared Goal

> Bilingual: this is the English version; 中文版见 [teamx-methodology.cn.md](26-teamx-methodology.cn.md)

---

## Opening: Three Humble Lieutenants Can Match One Brilliant Strategist

> 三个臭裨将，顶个诸葛亮。
>
> *Three humble lieutenants can match one brilliant strategist.*

teamx is built on a simple belief: **an ordinary model, multiplied by collaboration, can outperform a single brilliant model working alone.**

This is not hyperbole. Today's most expensive commercial LLM has formidable single-agent intelligence — but it is still just "one person": it works on one thing at a time, sees only what is in front of it, has no teammates to divide labor with, no reviewer to challenge it, and no one to take over when it gets stuck. A team is different: members can explore different directions in parallel, review each other, own different modules, and hand off work when someone hits a dead end.

teamx is a collaboration tool built for exactly this kind of **AI-native organization**. It organizes multiple independent AI sessions (in opencode, each session driven by a language model) into a team with division of labor, a shared goal, and a working rhythm. You can assemble such a team from multiple *free* opencode LLMs — with sound division of labor, collaboration, and review, this "team of lieutenants" can match, or even exceed, what an expensive commercial LLM can do alone.

teamx makes "getting expensive results from cheap models" possible — and it all starts with one small, declarative team definition file: **`TEAM.md`**.

---

## 1. Why TEAM.md?

### From ad-hoc commands to a team contract

Without teamx, assembling a multi-AI collaboration team means a chain of one-off commands: create a team, set a goal, add members, approve joins, assign roles... every step is manual, and the team structure lives only in memory and chat logs.

teamx turns this into a single statement: **team structure is declarative configuration, not one-off commands.**

Place a `.teamx/TEAM.md` in your repository that describes:

- who the team is (name, background);
- where it is going (goals);
- who is on it and what each person does (member profiles);
- what documents it produces and how they flow (optional: document contracts).

Then `teamx team create` **automatically** reads it: creates the team, sets the goal, issues invitation letters, and generates a per-member `AGENTS.md` plus working directories. The whole process is reproducible, auditable, and hand-off-able — any new member who reads `TEAM.md` immediately understands what the team is doing and who owns what.

### Built for AI-native organizations

An AI-native organization differs from a human one in a key way: **members can be added on demand, work in parallel, and require no onboarding time** — give a new member an accurate "job description" and they are productive immediately. TEAM.md is the source of those job descriptions:

- **Team level**: background and goals, so every member understands *why*;
- **Member level**: role, duties, skills, and outputs, so every member knows *what I do and what I deliver*;
- **Document level** (optional): which documents the team maintains, who owns them, who approves them, so knowledge production has a process too.

---

## 2. What TEAM.md Looks Like

TEAM.md is a Markdown file at **`.teamx/TEAM.md`** in the project root. It uses a loose Markdown-style parser, and both Chinese and English field names are accepted.

A complete example:

```markdown
# 企业数字化平台

## 背景
围绕团队目标构建企业数字化平台：支持任务分派、跨网络隧道、活动/成本分析。

## 目标
- 8 月底交付 v1.0：团队协作与活动分析
- 支持跨网络 reverse tunnel

## 成员
### owner
- 姓名: 企业数字化平台
- 角色: owner
- 分工: 架构设计、目标定义、代码审查
- 技能: Rust, TypeScript, 系统设计
- 输出: 架构文档、核心代码

### 小明
- 姓名: 小明
- 角色: contributor
- 分工: 前端开发、测试
- 技能: React, TypeScript
- 输出: 看板组件、测试用例

### 小红
- 姓名: 小红
- 角色: reviewer
- 分工: 代码审查、质量保障
- 技能: Rust, 代码评审
- 输出: 审查报告
```

### Field reference

| Section | Field | Meaning | Aliases (CN/EN) |
|---|---|---|---|
| `# Title` | — | Team name (first `# ` heading) | — |
| `## 背景` | — | Free-form team background | `背景` / `background` |
| `## 目标` | — | Goal list (`- ` items); becomes the team goal | `目标` / `goals` |
| `## 成员` | `### <key>` | One member profile; `key` is the member dir name | — |
| | `姓名` | Display name | `姓名`/`名字`/`name`/`显示名` |
| | `角色` | Role key (owner/contributor/reviewer/…) | `角色`/`role` |
| | `分工` | Duties / role description | `分工`/`职责`/`description`/`duties` |
| | `技能` | Skills list (comma separated) | `技能`/`skills` |
| | `输出` | Deliverables list (comma separated) | `输出`/`产出`/`outputs`/`deliverables` |

### Advanced: document contracts (`## 文档`, optional)

Teams don't just write code — they usually maintain requirements, designs, review reports, and so on. TEAM.md can **declare the lifecycle of these documents** with the `## 文档` section: each has a title, purpose, template, creator, owner, approver, a state flow, and reactions to change.

```markdown
## 文档
### 需求说明书
- 标题: 需求说明书
- 用途: 记录团队要交付什么
- 模板: 背景, 用户故事, 验收标准
- 创建者: [pm]
- 所有者: pm
- 审批者: [owner]
- 状态流: draft -> review -> approved -> done
- 变更响应:
  - on created: 通知 pm 起草
  - on approved: 通知 owner 已定稿
```

With this, document production is no longer "write it when you remember" — it becomes a **knowledge pipeline** with an owner, approvals, and state transitions.

---

## 3. Methodology: Four Principles Behind TEAM.md

TEAM.md is not an ordinary config file. It encodes four judgments about AI-native collaboration.

### Principle 1: Contracts over ad-hoc commands

> Team structure should be declarative and reproducible, not scattered across command lines and chat history.

Ad-hoc commands solve "now"; contracts solve "later." TEAM.md makes the team definition a versionable asset: it enters Git, evolves with the repository, and the whole team can be rebuilt from scratch at any time. This also makes "switching to a cheaper model" viable — the team's structure is unchanged; only the underlying model driving it changes.

### Principle 2: Personas before execution

> Define who does what and what they deliver, *then* start working.

teamx uses the **shared-goal model**, not the classic multi-agent pattern of "decompose the task and hand slices to isolated agents." In a shared-goal team, every member is an independent collaborator, not a tool being assigned work. The member profile in TEAM.md (role/duties/skills/outputs) is the vehicle for this persona:

- **Role**: the member's identity (owner sets direction, contributor does the work, reviewer gates quality);
- **Duties**: the concrete responsibility boundary, avoiding overlap and gaps;
- **Skills**: what the member is good at, for task assignment and mutual backup;
- **Outputs**: what the member delivers, so "done" has a concrete, reviewable artifact.

### Principle 3: Documents have lifecycles

> A document is not a final snapshot; it is a living contract with an owner, approvers, and a state flow.

In an AI-native organization, documents often come before code (requirements, design, review). Without an owner and a process, they become orphaned "wrote once, never touched again" files. TEAM.md's document contract turns documents into **declarative state machines**: who creates, who approves, how states flow, who gets notified — making knowledge production itself a managed pipeline.

### Principle 4: Files are durable knowledge

> The teamx ledger stores collaboration events; files (TEAM.md, AGENTS.md, design docs) are the reusable knowledge.

After parsing TEAM.md, `team create` generates a per-member `AGENTS.md` (merging the project root `AGENTS.md` with that member's duties/skills/outputs from TEAM.md). Each AI session starts already "knowing who it is, what to do, and what to deliver." Knowledge lives in files, not in a session's transient context.

---

## 4. Organizational Process Assets: Collaboration Is Not Just Results, It's a Depositable Process

The biggest waste in team collaboration is "what was done cannot be reused." From the start, teamx treats collaboration as a **process asset**: it doesn't just preserve final artifacts, it fully preserves the process that produced them, so every collaboration accumulates knowledge for the next one.

### 4.1 The ledger: an auditable collaboration timeline

teamx maintains an **append-only event ledger** for every team. Every significant thing that happens is written as an event:

- **Team lifecycle**: creation, goal set/shared/achieved/closed, state changes;
- **Member dynamics**: join requests, approvals/rejections, role selection and changes, co-lead promotion, leaving;
- **Collaboration content**: progress reports, decision broadcasts, clarifying questions and answers, fact-request reports;
- **Documents and activity**: document state transitions (created/reviewed/approved/...), member session activity heartbeats.

Every event carries a stable `seq`, the originating member, and a timestamp. The ledger is **append-only** — history is never rewritten or deleted, only extended. This means:

- **Auditable**: at any time you can answer "what actually happened in the team, who did what and when";
- **Replayable**: starting from an empty team and replaying events by `seq` rebuilds the full team state;
- **Reviewable**: after a meeting or a goal is reached, the ledger reconstructs the whole decision and execution process.

### 4.2 Artifact files: reusable knowledge assets

Beyond the ledger, teamx deposits collaboration knowledge into **repository files**:

| Asset | Location | Content |
|---|---|---|
| Team contract | `.teamx/TEAM.md` | Team definition: background, goals, member profiles, doc contracts |
| Member job description | `.teamx/members/<key>/AGENTS.md` | Each member's "who I am, what I do, what I deliver" |
| Design session record | `docs/design/<topic>.md` | The complete design decision tree produced by grill-doc |
| Architecture decision records | `docs/adr/*.md` | Major, hard-to-reverse technical decisions and rationale |
| Glossary | `CONTEXT.md` | Short definitions of project-specific terms |
| Invitations/certificates | `.teamx/letters/` | Member admission credentials and mTLS material |

All of these go into Git and are versioned with the repository. **Organizational process assets = event ledger (process) + artifact files (knowledge).** Whether you change members, switch models, or restart a similar project months later, you continue from the previous assets instead of starting from zero.

---

## 5. Cost and Time: Making Collaboration Measurable and Manageable

AI collaboration is not free: tokens cost money, time costs money. teamx believes **unmeasurable collaboration cannot be managed**, so it turns "who, on which goal, spent how much time and how many tokens" into a clear record.

### 5.1 Time records: who worked when

Every teamx ledger event carries a timestamp, and member session activity is periodically mirrored into the ledger (`session.idle` heartbeats). Combined, these produce a complete **member activity timeline**:

- Which member was working in which time window;
- How much time passed from team start to goal achievement;
- When a goal fell silent (no events for a long time) — which is exactly what the server-side nudge reminder is based on.

### 5.2 Token cost: attribution by goal and member

Precise token consumption is provided by each model platform (or opencode's usage statistics). teamx's value is **collaboration attribution**: because every collaboration event is attributed to a specific **goal, member, and time window**, platform usage bills can be **attributed to teams, goals, and members**:

- How many tokens and how much time did this goal cost in total?
- Which member/role consumed the most?
- Which design discussion round was the most expensive? Which direction was "expensive trial and error"?

### 5.3 Management uses

Put these records to work for enterprise-level management:

- **Cost management**: account token spend by goal, member, and time window; identify high-cost, low-output stages;
- **Time planning**: estimate time budgets for subsequent tasks from historical activity timelines; spot long-silent goals and intervene early (nudge);
- **Review**: after a goal is achieved or abandoned, replay the event ledger and artifact files to review decision quality, collaboration efficiency, and cost structure — forming the basis for the next round of improvement.

> **Status note**: the event ledger, activity heartbeats, time records, and nudge reminders are implemented capabilities in teamx today; precise token counting currently relies on model-platform usage statistics, with teamx providing the goal/member/time attribution framework. The two together enable cost management and review.

---

## 6. Creating Your TEAM.md Interactively with grill-doc

Writing TEAM.md by hand is easy; *deciding how to structure the team* is not: how many members? What roles? Where are the duty boundaries? Do we need document contracts? How should document states flow? These are exactly what **grill-doc (Design Session)** excels at.

grill-doc is an owner-led interactive design session: it expands your idea into a *design tree*, asks every decision round by round with a concrete recommendation, and settles the conclusions into durable documents. Using grill-doc to design your TEAM.md gives you not an empty template, but **a team contract that has been thought through, with explicit trade-offs, ready to use**.

### 4.1 Start a session

In the team owner's opencode session:

```text
/team-grill 设计我们团队的 TEAM.md --doc .teamx/TEAM.md
```

Key points:

- `--doc .teamx/TEAM.md` binds the Design Session Record directly to TEAM.md's final location — when the session completes, `TEAM.md` is the artifact;
- grill-doc first syncs team state, verifies the owner identity, then reads facts it can discover safely from the repository;
- without `--doc`, the record defaults to `docs/design/<topic-slug>.md`.

### 4.2 What the design tree asks

grill-doc decomposes "how to write TEAM.md" into a set of dependent design questions (`DQ-*`), roughly covering:

| Design question | TEAM.md section | Example recommendation |
|---|---|---|
| How to describe the team background | `## 背景` | One sentence: why this team exists |
| How to set goals | `## 目标` | ≤ 3 verifiable goals |
| Which roles are needed | `## 成员` | Minimal: owner + contributor + reviewer |
| Who fills each role | `## 成员` | Pick roles first, then assign member keys |
| How to divide duties | `## 成员` duties | By module / by responsibility, avoid overlap |
| What each role delivers | `## 成员` outputs | Reviewable deliverables |
| Whether document contracts are needed | `## 文档` (optional) | Only if the team produces documents |
| How document states flow | `## 文档` states | draft -> review -> approved -> done |

### 4.3 How to answer each round

grill-doc presents the whole current Frontier (all immediately answerable questions) in one round, each with a **recommended answer and rationale**. You can answer item by item, or simply say:

```text
全部按推荐
```

Ambiguous or missing answers leave the question unresolved; the recommendation is never auto-applied. After each round the tree recomputes and dependent questions appear in the next round.

### 4.4 Fact requests

If a question lacks facts (e.g. "who on the team is good at architecture?"), the owner can delegate a Fact Request (`FR-*`) to a member, who returns a Fact Report via teamx. The report is evidence for the owner to evaluate — the decision still belongs to the owner.

### 4.5 Completion gate

The session is complete only when **all** of the following hold:

1. the design tree has no unvisited branch;
2. the Frontier and Remaining Branches are empty;
3. every question raised has an explicit disposition;
4. the Design Session Record agrees with TEAM.md;
5. the human owner explicitly confirms Shared Understanding.

For example:

```text
我确认 Shared Understanding，可以结束设计会话。
```

When complete, `.teamx/TEAM.md` is a fully considered team contract, ready for `teamx team create`.

> **Recommended flow**: run a grill-doc session to design TEAM.md first, then run `teamx team create`. Thinking before building saves more time than building while thinking.

---

## 7. From TEAM.md to a Running Team

When `.teamx/TEAM.md` exists, `teamx team create` performs the whole bootstrap automatically:

```text
teamx team create 你的团队名 --session <session-key>
```

It will:

1. **parse TEAM.md** — team name, background, goals, member profiles (and optional document contracts);
2. **create the team** — set the goal (background + goals);
3. **start serve** — launch the embedded `teamx serve` in network mode;
4. **issue invitation letters for every member** (with the member's role and dedicated certificate);
5. **generate a per-member `AGENTS.md`** (project root AGENTS.md merged with that member's duties/skills/outputs from TEAM.md);
6. **create member working directories** `.teamx/members/[member-name]/`.

After you distribute the invitations and the owner approves the imports, collaboration begins. Throughout, `TEAM.md` is the only team definition you maintain — it is the team's constitution.

---

## 8. FAQ

**Where does TEAM.md live?**
At `.teamx/TEAM.md` in the project root — the default location `team create` checks.

**What if the team membership changes?**
Edit the `## 成员` section of TEAM.md and re-run the relevant initialization. TEAM.md is declarative: editing it edits the team structure.

**Are document contracts required?**
No. The `## 文档` section is optional. If your team moves forward mainly through code and collaboration events, skip it; add it once you need to maintain requirements/design/review documents.

**Can I write it in Chinese or English?**
Both. The parser accepts Chinese and English field aliases (`姓名`/`name`, `角色`/`role`, `分工`/`description`, `技能`/`skills`, `输出`/`outputs` etc.), so teams of any language background are fine.

**Do I have to use grill-doc to create TEAM.md?**
No. You can write it by hand or with any editor. grill-doc's value is in helping you *think it through* — especially trade-off-laden decisions like role split, duty boundaries, and document workflows.

**What is the relationship between TEAM.md and AGENTS.md?**
TEAM.md is the team-level contract; `team create` generates a per-member `AGENTS.md` as the "team contract + personal job description" merge, so each AI session knows who it is, what to do, and what to deliver.

**Are collaboration process and intermediate artifacts preserved?**
Yes. teamx's event ledger is append-only and records every collaboration event (with seq, member, and timestamp) — auditable and replayable; TEAM.md, per-member AGENTS.md, design records, ADRs, and the glossary are versioned in Git. Together they form reusable organizational process assets.

**Can token cost and collaboration time be managed?**
Yes. The event ledger plus member activity heartbeats provide a complete time record (who, when, how long, when silent), with nudge reminders for time-based intervention; token consumption is tracked by the model platform, and teamx provides the goal/member/time attribution framework — supporting cost management, time planning, and review.

---

## Conclusion

Back to the opening: three humble lieutenants can match one brilliant strategist.

teamx lets you organize an "AI team driven by ordinary LLMs" — define the team contract with TEAM.md, share the goal with the shared-goal model, and amplify each member's capability through role division and mutual review. When your team is made of several cheap, available models, what you get is not "one medium model" but **an organization with division of labor, review, and process** — which often goes further than a single expensive point of intelligence.

And it all starts with one interactive design session and one TEAM.md.

```text
/team-grill 设计我们团队的 TEAM.md --doc .teamx/TEAM.md
```
