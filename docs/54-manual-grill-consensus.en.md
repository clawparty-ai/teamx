# Manual: Grill Consensus Mode (team consensus design interview)

> For teams that must decide **together** (everyone gives a view, then converge on a
> consensus) instead of a single person deciding.
> Protocol: grill-with-docs **v2 · Consensus Mode** (`protocols/grill-with-docs.md`, §3A).
> Audience: the human leader who runs the interview, and team members who answer.

---

## 1. What it is

Upgrades grill-doc from *single-owner answers* to **all members answer → converge on a
consensus**:

- **1 grill session = 1 discussion thread**
- The leader posts **one top-level message per Design Question** (`DQ-xxxx`).
- Members answer under that message with **reply**: answer + rationale + dissent.
- The leader synthesizes and posts **`✅ 结论 DQ-xxxx`** as a reply to the same message,
  and writes the conclusion into the **Design Session Record**.
- One question at a time, until the Frontier is empty and the leader confirms consensus.

> Scope: grill clarifies requirements/decisions. It does **not** write code, commit, or
> push.

---

## 2. Prerequisites

- You are in a teamx team (grill can run pre-team, but Consensus Mode needs a discussion,
  hence a team + server).
- The client has the `teamx_discussion` tool (opencode/dsh plugin) or the CLI
  `teamx discussion` (network mode).
- The client is on **grill protocol v2** (`/team-grill` reports v2).

---

## 3. Roles

| Role | Who | Responsibility |
|---|---|---|
| **leader** | the human initiator | create the thread, build the Design Tree, ask per question, synthesize conclusions, **sole** writer of record/CONTEXT/ADR, confirm Shared Understanding |
| **member** | team members | answer each DQ via `discussion.reply` (answer + rationale + dissent) |

> Single writer: only the leader edits the record; members contribute via discussion
> replies only.

---

## 4. Running a Consensus Mode grill

### 4.1 Create the discussion thread (leader)

```bash
teamx_discussion action=create title="<topic> design grill (Consensus Mode)" body="<session notes: leader, consensus rule, structure>"
# returns a discussion_id, e.g. dsc_xxxxxxxx
```

### 4.2 Create the Design Session Record (leader)

Create `docs/design/<slug>.md` from the template in `protocols/grill-with-docs.md`
(Context, Design Tree, Frontier). Only the leader edits this file.

### 4.3 Per question (serial)

**① Leader posts the question**

```bash
teamx_discussion action=say id=<discussion_id> body="❓ **DQ-0001** - **<title>**: <decision, trade-offs, choices>

➡️ <recommended answer and why>

All members: answer with discussion.reply (reply_to=this message). Window: all answered or N minutes."
```

**② Wake all members**

```bash
teamx_publish type=update assignee=<member_id> data={"message":"【grill DQ-0001】reply to <discussion_id>#<msg_id> …"}
# or teamx_ask member_id=<id> question="…"
```

**③ Members answer**

```bash
teamx_discussion action=reply id=<discussion_id> message_id=<DQ msg id> body="Answer: …; Rationale: …; Dissent: …"
```

**④ Leader synthesizes** a conclusion reply (reply_to the same question message):

```bash
teamx_discussion action=reply id=<discussion_id> message_id=<DQ msg id> body="✅ 结论 DQ-0001
- consensus: <decision>
- rationale: …
- dissent: …
- unanimity: unanimous | majority | leader-decides"
```

**⑤ Leader writes the record** (`Settled Decisions`):

```markdown
- DQ-0001 <title>: <consensus> — rationale: …; dissent: …;
  unanimity: majority; discussion: <discussion_id>#<msg_id>
```

**⑥ Next question**: go back to ① for DQ-0002.

---

## 5. Completion

- Every `DQ-*` has a `✅ 结论` reply and a matching record entry;
- the Frontier is empty;
- the leader explicitly confirms **Shared Understanding**;
- set record `status: completed`;
- `discussion.close <id>`;
- (optional) `teamx_publish type=decision` pointing at the record.

---

## 6. Rules

- Declare the **consensus rule** at start: `unanimous` | `majority` | `leader-decides`.
- Always record **dissent**, even when a decision carries.
- **Answer window**: all answered or timeout; absent = abstain.
- **Append-only discussion**: never edit a question message; the conclusion is double-written
  (reply + record).
- **Facts vs. trade-offs**: facts use Fact Requests (`FR-*`); Consensus Mode settles
  subjective trade-offs only.
- **One thread per session**: close only at completion.

---

## 7. FAQ

**Why double-write the conclusion (reply + file)?** The ledger is append-only and cannot
hold the evolving authoritative answer; the record file is the editable authority, the
discussion is the deliberation trail.

**Can one session use multiple discussions?** Yes, but the leader must maintain a
DQ→thread map; a single thread is the default and simpler.

**Can a regular member edit the record?** No — single writer: only the leader edits
record/CONTEXT/ADR.

**Member lacks the `teamx_discussion` tool?** Update the client (§2); otherwise use the
CLI `teamx discussion reply` (network mode). Having the leader re-post on their behalf is
discouraged (it breaks "members post themselves").

**Can high-risk actions (merge/revoke/destroy/goal.close) go through IM?** That is the IM
access topic, not grill itself; grill only produces decision documents.
