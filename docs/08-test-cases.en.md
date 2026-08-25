# teamx V1 Test Cases

Conventions:
- Each case gives its automation target (a `cargo test` test name / steps in `tests/*.sh` / manual); cases without an automation target are manual cases.
- CLI cases are denoted `teamx <cmd>`; before running, `export TEAMX_DB=$(mktemp ...).db`.

## A. Unit Tests (`cargo test`)

| ID | Name | Steps | Expected | Target |
|---|---|---|---|---|
| TC-001 | Team happy path | forming→share_goal→active→blocked→resumed→close_goal→completed→archive | Correct state at each step | `state::tests::team_happy_path` |
| TC-002 | Team illegal transitions rejected | close_goal while forming / archive again while archived, etc. | Returns Err | `state::tests::team_illegal_transitions_rejected` |
| TC-003 | Team neutral actions | publish start/progress/decision/refine/achieved while active | State stays active; these actions do not lift the block while blocked | `team_neutral_actions_keep_active` |
| TC-004 | Member happy path | pending→approve→active→ask→waiting→respond→active→idle→active | States correct | `member_happy_path` |
| TC-005 | Member leave | leave→left allowed from pending/active/waiting/idle alike; after left/denied, approve/ask no longer permitted | Correct + rejected | `member_leave_from_any_state` |
| TC-006 | Member role switch | set_role is neutral for active/idle; choosing a role while pending activates the member | States correct | `member_role_set_is_neutral` |
| TC-007 | Goal happy path | proposed→shared→in_progress→blocked→in_progress→achieved→closed | States correct | `goal_happy_path` |
| TC-008 | Goal refine flow | shared/in_progress→refine→refining→start→in_progress | States correct | `goal_refine_flow` |
| TC-009 | Goal illegal transitions rejected | any action after closed, skipping intermediate states, etc. | Returns Err | `goal_illegal_transitions_rejected` |
| TC-010 | State string round-trip | from_str/as_str are inverses for every state; unknown strings yield None | Passes | `state_string_roundtrip` |
| TC-011 | Per-team seq independent and monotonic | two teams write interleaved, each getting 1..3 | Each monotonic, no mutual interference | `events::tests::seq_is_monotonic_and_independent_per_team` |
| TC-012 | Cursor exclusive read | list(after=N) returns only entries with seq>N | Correct | `list_after_cursor_is_exclusive` |
| TC-013 | Cursor advancement | cursor_for defaults to 0; reads are correct after set_cursor; overridable | Correct | `cursor_advance_is_idempotent` |
| TC-014 | payload JSON round-trip | Chinese strings/numbers/boolean objects stored and fetched through the ledger | Equal | `payload_roundtrips_through_json` |

## B. CLI Smoke · Happy Path (`tests/smoke.sh`)

| ID | Name | Steps | Expected |
|---|---|---|---|
| TC-101 | Init | `teamx init` (twice) | Idempotent, ok:true |
| TC-102 | Create team | `teamx team create "Test Team" --session inst:alice` | Returns team id + invite_token, state=forming |
| TC-103 | Join | `teamx team join <token> --name Bob --session inst:bob` | Pending, prompted to wait for approval |
| TC-104 | Approve | owner `teamx team approve <member_id>` | Member active |
| TC-105 | Pick role | `teamx role set contributor --session inst:bob` | role=contributor |
| TC-106 | Set goal | owner `teamx goal set "Ship the MVP" --body ...` | goal proposed |
| TC-107 | Share goal | owner `teamx goal share` | goal shared, team active |
| TC-108 | Report progress | `teamx publish progress --data '{"message":"..."}'` | goal in_progress, event recorded in the ledger |
| TC-109 | Ask / answer | owner `ask` → Bob `respond` | Bob waiting→active, question answered |
| TC-110 | Report done | `teamx publish achieved` | goal achieved |
| TC-111 | Close goal | owner `teamx goal close` | goal closed, team completed |
| TC-112 | Final-state sync | member `teamx sync` | Sees team.completed and all other events |
| TC-113 | Ledger ordering | `teamx events --team <id>` | seq strictly increasing |

## C. CLI Boundary / Negative (`tests/cli-test.sh`)

| ID | Name | Steps | Expected |
|---|---|---|---|
| TC-201 | Bad token | `team join bogus-token` | Rejected |
| TC-202 | Duplicate join | same session joins the same team a second time | Rejected |
| TC-203 | Unauthorized approval | non-owner approve/deny | Rejected |
| TC-204 | Unauthorized goal operations | non-owner share/close goal, assigning roles on someone else's behalf | Rejected |
| TC-205 | Approving a non-pending member | deny an already-active member | Rejected |
| TC-206 | Denial flow | second member joins, then owner denies | Member denied, sync refused |
| TC-207 | Unknown role | `role set wizard` | Rejected with a pointer to the catalog |
| TC-208 | Illegal publish type | `publish teleport` | Rejected, valid types listed |
| TC-209 | Publish before a goal is set | brand-new team directly `publish progress` | Rejected, prompted to set a goal first |
| TC-210 | Self ask/answer | owner asks themselves | Rejected |
| TC-211 | Answer by a non-target | someone other than the target responds | Rejected |
| TC-212 | Repeat answer / unknown id | respond again after already answered / respond to a nonexistent id | Rejected |
| TC-213 | Multi-team disambiguation | same session joins two teams; status/publish without `--team` | Rejected with the team list listed; succeeds with `--team` |
| TC-214 | Cursor semantics | sync advances→empty; `--no-advance` does not advance (new events visible both times); normal sync after advancing leaves it empty | See the case |
| TC-215 | events requires --team | `events` without `--team` | Rejected |
| TC-216 | Leave | leave again after leaving; member state=left | First succeeds, second rejected |
| TC-217 | Join forbidden after completion | new join on a completed team | Rejected |
| TC-218 | loopx unbound / unavailable | non-member loopx report; nonexistent project directory | Clear message; the closed loop is unaffected |

## D. Concurrency (partially merged into C)

| ID | Name | Steps | Expected |
|---|---|---|---|
| TC-301 | Seq stays monotonic under concurrent writes | 5 sessions × 3 parallel publishes (`tests/concurrency.sh`) | 15 events with strictly increasing, unique seq |

## D2. Three-Member Collaboration (`tests/three-member.sh`, equivalent to demo-team)

| ID | Name | Steps | Expected |
|---|---|---|---|
| TC-401 | Three-person closed loop | owner creates team+goal; contributor/reviewer each join+apply for a role (remaining pending); owner approves both+shares the goal; contributor produces a design progress; reviewer syncs, sees it, and produces a review progress; owner asks→contributor responds; owner broadcasts a decision; contributor reports achieved; owner closes+archives | Roles=contributor/owner/reviewer all active; final states archived/closed; the event chain contains all key types |

## E. Plugin Registration (build-time probing)

| ID | Name | Steps | Expected |
|---|---|---|---|
| TC-401 | Plugin typecheck | `bunx tsc --noEmit` | 0 errors |
| TC-402 | Plugin bundling | `bun run build` | dist/teamx.js generated |
| TC-403 | Agent registration | `opencode agent list` | `teamx (all)` appears |
| TC-404 | Command registration | GET `/command` after opencode serve | List contains `Team` |
| TC-405 | Tool registration | GET `/experimental/tool/ids` | Contains all 17 `teamx_*` |

## F. Manual Acceptance (two real opencode windows)

| ID | Name | Steps | Expected |
|---|---|---|---|
| TM-01 | Full closed loop | Window A `/Team create team "Demo" ...`; Window B `/Team join <token> ...`; A approves and shares the goal; B picks a role, reports progress, asks a clarifying question, reports completion; A verifies the closure | Both windows consistent, events traceable, team completed |
| TM-02 | Multi-member roles | 3 windows: owner + contributor + observer | Observer watches read-only; owner broadcasts are picked up by every member's sync |
| TM-03 | loopx linkage | member binds a loopx project, `teamx_loopx_report` is published, owner sync shows loopx.progress | Event contains goal_state/gate/next_todo/quota |
| TM-04 | Three-person collaboration | 3 windows: owner + contributor + reviewer, walking the full flow of `docs/13-demo-team.md` | Final states archived/closed; design-plan.md + review-plan.md produced |
| TM-05 | Model-level acceptance (headless) | `tests/acceptance.sh`: `opencode run --agent teamx` lets a real model create the team | Ledger contains team.created/goal.set, team name correct |
