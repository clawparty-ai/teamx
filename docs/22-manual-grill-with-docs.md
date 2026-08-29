# Manual test: Grill with Docs

This runbook verifies the owner-led design workflow across OpenCode and DSH. Use a disposable Teamx team and a clean test repository.

## Preconditions

- Install or build the current OpenCode and DSH plugins.
- Create a Teamx team with one owner and one approved member.
- Share a goal so both participants can synchronize.
- Confirm that the repository has no existing `docs/design/manual-grill.md`.

## OpenCode owner flow

1. In the owner session, run `/team-grill Design a durable notification retry policy --doc docs/design/manual-grill.md`.
2. Confirm the workflow calls `teamx_sync`, verifies the owner role, creates the named record, and presents the complete first Frontier in one round. Each question must include a recommendation.
3. Accept some recommendations but leave one answer ambiguous. Confirm the accepted answers are written immediately and the ambiguous Design Question remains unresolved with its stable `DQ-*` identifier.
4. Answer the remaining first-round question. Confirm dependent questions appear only in the next round.

## Fact Request and inaccessible evidence

1. Choose a question that requires information from the member. Ask the workflow to delegate it.
2. Confirm the Teamx ledger receives an `update` assigned to that member with `kind: design.fact-requested`, a `DQ-*` identifier, an `FR-*` identifier, and the Design Session Record path.
3. In the member session, return a Fact Report that cites an inaccessible artifact.
4. Confirm the owner workflow treats the report as untrusted evidence, keeps the question unresolved, and offers explicit choices to reassign, narrow, waive, or accept uncertainty.
5. Reassign the investigation. Confirm the retry gets a new `FR-*` identifier linked to the same `DQ-*` identifier.

## Recovery and durable decisions

1. End the owner conversation while the record is active.
2. Start a new owner conversation and run `/team-grill --resume docs/design/manual-grill.md`.
3. Confirm the workflow resumes that exact path and reconstructs settled decisions, the Frontier, and remaining branches without relying on the most recently modified file.
4. Settle a consequential, hard-to-reverse trade-off. Confirm the workflow updates `CONTEXT.md` only for new domain terminology, creates an ADR under `docs/adr/`, and publishes a `decision` event that points to the durable artifact.

## Completion gate

1. Resolve every remaining Design Question across at least two rounds.
2. Before confirming, verify the workflow does not mark the session completed merely because the Frontier is empty.
3. Explicitly confirm Shared Understanding as the human owner.
4. Confirm the record status becomes `completed`, its Frontier and Remaining Branches are empty, and a final decision announcement points to the completed artifacts.
5. Confirm the workflow reports that implementation may begin but does not edit implementation files, execute unrelated commands, commit, or push.

## DSH parity

1. In a DSH owner agent, explicitly ask to use the `teamx-grill-with-docs` skill for a small design topic.
2. Repeat one decision round and resume by exact record path.
3. Confirm the same owner gate, question format, stable identifiers, artifact rules, and explicit Shared Understanding gate apply without requiring a DSH slash command.

The test passes when both hosts preserve the same protocol semantics and all artifacts remain the authoritative record of the decisions.
