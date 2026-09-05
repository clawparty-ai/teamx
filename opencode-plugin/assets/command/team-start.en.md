---
description: Step-by-step wizard: interactively define your team and produce a buildable .teamx/TEAM.md
agent: teamx
---

Run the protocol below for $ARGUMENTS. Use the user's current language throughout.

User arguments: $ARGUMENTS

# Team Definition Wizard (enhanced team start)

Help the user/customer **define a team step by step**, producing a **`.teamx/TEAM.md`** that `/team create` can consume to create the team. Be a guide: one screen per step, explain in plain words before asking, offer recommendations/options. Do not dump jargon.

## 0. Startup

1. Best-effort `teamx_sync` to detect context: not in a team yet → run as a **pre-team wizard** (write to Git only, no ledger events); already in a team → led by a team lead (owner/co-lead), may improve the existing contract.
2. Read whether `.teamx/TEAM.md` exists:
   - exists → start from it as **prefill**; only ask about what's still open/changing; at the end ask **Update (incremental merge) / Overwrite**.
   - absent → fresh definition.
3. Detect the user's language; write in it; keep terms plain.
4. Record: by default write `docs/design/<team-slug>-team.md` (Design Session Record shape, resumable with `--resume`); `--no-record` skips the process record (TEAM.md is still written). With `--resume`, read that record and continue.

## 1. Wizard steps (one screen each; "skip / later" allowed; list gaps on the finish screen)

**① Team name**
- Plain ask: "Give the team a name (one word/short phrase) plus one sentence: what is this team / why does it exist."
- Output: `# <name>` and a `## 背景` draft.

**② Background**
- Plain ask: "In 2–3 sentences: who it serves, what problem it solves, roughly how you'll collaborate."
- Output: `## 背景`.

**③ Goals**
- Plain ask: "Write ≤3, ideally verifiable goals (what result, roughly when)."
- Output: `## 目标` list. If >3, help merge/cut to the 3 most important.

**④ Roles**
- Plain ask: "What roles does this team need?"
- Offer a **template quick pick** (table below) and **plain-language role cards**; allow "use template / add-remove / from scratch".

| Template | Suggested roles (plain) |
|---|---|
| Software delivery | owner(lead) + tech-lead + contributor(dev) + reviewer(gate) |
| Content / research | owner + research-lead + searcher + kb-engineer + extractor |
| General collaboration | owner + contributor(doers) + reviewer(verifier) |
| From scratch | none; user defines |

**⑤ Member assignment**
- Plain ask: "Who fills each role now?" Enter per person: `name / role / duties / skills / outputs`.
- key (`### key`): suggest pinyin/English/username; note it becomes a directory name.
- At least one member must be role **owner**.
- Output: profiles under `## 成员`.

**⑥ Doc contracts (optional)**
- Plain ask: "Do you want fixed doc types + flows, e.g. a 'proposal' going  draft → review → approved → done?"
- No → skip; Yes → define per type `title/purpose/template/creators/owner/approvers/states(default draft→review→approved→done)`.
- Output: `## 文档`.

**⑦ Preview + validate**
- Assemble the full `.teamx/TEAM.md` and show a preview.
- Run:
  ```
  teamx teamfile validate
  ```
  Explain each `error` in plain words and guide the fix (e.g., "no owner yet" → back to step ⑤); treat `warnings` as hints.
- Only allow save when validation has no errors.

## 2. Save & handoff (finish screen)

1. Confirm writing `.teamx/TEAM.md`.
2. If it existed before → ask **Update (incremental merge, show diff) / Overwrite**.
3. Finish screen shows:
   - summary ("your team contract is ready": name / goal count / member count);
   - gap list (warnings like "no owner yet" / "goals empty");
   - a copyable one-liner (**not executed automatically**):
     ```
     /team create <Team Name>
     ```
   - if `--then-create` was given: **after explicit confirmation**, run the create once (default: never).
4. Unless `--no-record`, write `docs/design/<team-slug>-team.md` as completed.

## 3. Plain-language glossary (only on confusion; don't paste all at once)

| Term | Plain |
|---|---|
| team | a group working toward one goal |
| goal | the current result the team is after (one at a time) |
| role | a division of work, e.g. "developer", "gatekeeper" |
| member | one seat/person in the team |
| owner | team lead (approves members, sets goals, verifies) |
| co-lead | backup lead; most management actions also allowed |
| contributor | the doer / deliverer |
| reviewer | the gatekeeper / verifier |
| TEAM.md | the team's "charter"; `team create` reads it to generate roles, AGENTS and member dirs |
| letter | the credential that lets someone join the team |
| goal states | proposed → shared → in_progress → achieved → closed |

## 4. Principles

- Never auto-create the team or auto-invite people; only produce TEAM.md + the record.
- Don't override settled answers; ask when ambiguous.
- Put every decision in the matching TEAM.md section; note consequential/hard-to-reverse choices in the record, but don't auto-write ADRs.
- Keep language plain; when a term is unavoidable, give a one-line plain explanation.

Related: `docs/36-team-start-wizard.cn.md` (design); `docs/23-manual-grill-with-docs-usage.md`; `docs/27-teamx-methodology.cn.md`.
