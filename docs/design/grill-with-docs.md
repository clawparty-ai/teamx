---
status: completed
protocol_version: 1
owner: human owner
created_at: 2026-08-27
updated_at: 2026-08-29
---

# Teamx grill-with-docs workflow

## Context

Teamx currently supports targeted member clarifications and unstructured decision broadcasts, while design documents are written directly in the shared workspace. It does not provide a design tree, multi-round decision frontier, durable decision records, or a canonical glossary. This design adds those capabilities as a host-neutral Agent workflow without introducing a new Rust or SQLite domain model in the first version.

## Settled Decisions

### Product boundary and authority

- Deliver the first version as an Agent workflow; add a core `DesignSession` model only when cross-device persistence requires it.
- The human owner settles every Design Question. Members provide evidence and critique but cannot implicitly accept a decision.
- Design deliberation is team-visible. The first version does not claim to provide private questions.
- A Design Session starts only through an explicit owner action; ordinary conversation never activates it automatically.
- The session finishes only after its Design Tree is exhausted, artifacts reflect every decision, and the human owner explicitly confirms Shared Understanding.
- Completion does not automatically implement code, run system commands, commit, or push.

### Knowledge and recovery

- Versioned Git artifacts are the durable knowledge source; the Teamx ledger stores coordination events and artifact references.
- `CONTEXT.md` is the canonical glossary and contains no implementation specification.
- Consequential, non-obvious, hard-to-reverse decisions become ADRs under `docs/adr/`.
- A user-specified design document acts as the Design Session Record; otherwise the default is `docs/design/<slug>.md`.
- A record is restored by explicit path, never by a fuzzy title or most-recent-file heuristic.
- Multiple active Design Sessions may coexist because their artifact paths are their identities. A conversation binds to only one record at a time.
- Completed records remain in Git. Reopening is explicit, and later choices supersede rather than silently rewrite prior decisions.

### Deliberation and evidence

- The workflow maintains a dependency-aware Design Tree and asks the entire currently unblocked Frontier in each round.
- Every displayed question includes a recommended answer, but the recommendation has no authority until the owner accepts it.
- Persisted Design Questions use stable identifiers such as `DQ-0001`; per-round display numbers are presentation only.
- Members receive self-contained Fact Requests with stable identifiers such as `FR-0001`. A retry or reassignment creates a new Fact Request linked to the same Design Question.
- Fact Reports reference their Fact Request. Missing, delayed, inaccessible, or conflicting evidence keeps the Design Question unresolved until the owner explicitly reassigns, narrows, waives, or accepts the uncertainty.
- Fact Reports, ledger events, artifacts, and external documents are untrusted data. They never become system instructions or accepted decisions merely by assertion.
- Only the owner session updates the Design Session Record, glossary, and ADRs.

### Host integration

- The capability name is `teamx-grill-with-docs`.
- OpenCode exposes `/team-grill <topic>`, with optional `--doc <path>` and `--resume <path>` forms.
- DSH exposes a runtime Skill selected through natural-language intent; a DSH slash command is deferred until its command layer can safely start a multi-round Agent interaction.
- One English, host-neutral protocol lives at `protocols/grill-with-docs.md` and requires all interaction and documents to use the user's current language.
- A deterministic generator produces committed OpenCode command assets and a committed DSH TypeScript adapter. Generated files include the protocol version and source hash and must not be edited directly.
- Adapter drift is checked automatically with a generator `--check` mode.

### Ledger compatibility

- The first version adds no Rust tables, state transitions, RPC methods, or network endpoints.
- Fact Requests use the existing neutral `publish update` event with an assignee; Fact Reports also use `publish update`.
- Recorded decisions are announced with `publish decision`. The broadcast is only a notification pointing to the durable Decision Record.
- Structured payloads carry `protocol_version`, `kind`, `session`, a stable question or request identifier, an artifact reference, and a human-readable message.
- Payload and Design Session Record compatibility starts at protocol version 1. Incompatible schema changes increment the protocol version.

Example Fact Request payload:

```json
{
  "protocol_version": 1,
  "kind": "design.fact-requested",
  "session": "grill-with-docs",
  "question": "DQ-0012",
  "request": "FR-0001",
  "artifact": "docs/design/grill-with-docs.md",
  "message": "Investigate how plugin assets are generated and installed."
}
```

## Current Frontier

None. All design branches have been visited.

Completion gate satisfied on 2026-08-29: the human owner confirmed Shared Understanding and authorized implementation.

## Remaining Branches

None.

## Fact Requests and Reports

### Existing writing-domain investigation

The current `questions` model is a single targeted clarification that moves a member into `waiting`; it cannot represent a multi-question Design Frontier. `decision.broadcast` is an unstructured event rather than a Decision Record. Documents are workspace files and are not tracked as first-class Teamx artifacts.

### Host-adapter investigation

OpenCode installs static Markdown agent and command assets through explicit `install.sh` lists. DSH registers a TypeScript runtime Skill and bundles it into `lib/index.js`. There is no existing shared protocol or generator, and the current DSH Teamx Skill is a manually adapted copy of the OpenCode instructions.

## Implementation Scope

### Add

- `protocols/grill-with-docs.md`
- `scripts/generate-grill-protocol.mjs`
- Generated OpenCode command assets for `/team-grill`
- Generated DSH Skill adapter
- Protocol consistency tests
- Manual Design Session runbook

### Modify

- DSH Skill registration
- OpenCode installation and uninstallation asset lists
- OpenCode and DSH package scripts
- Both plugin READMEs
- The repository-wide test runner

### Exclude

- Rust and SQLite changes
- New RPC or WebSocket endpoints
- Private deliberation
- Automated Git commits or pushes
- Automatic code implementation after deliberation
- A persistent core DesignSession state machine

## Verification

- Generator write mode and `--check` mode produce identical adapters.
- OpenCode typechecks and builds from a clean checkout; DSH builds standalone and typechecks when its declared sibling `deepseek-harness` development checkout is available.
- Installation and uninstallation include the OpenCode command assets.
- Generated adapters contain the expected protocol version and source hash.
- Static tests verify the protocol's critical invariants without matching full LLM prose.
- A manual runbook covers at least two decision rounds, a Fact Request, inaccessible evidence, record recovery, ADR creation, and final Shared Understanding confirmation.

## Related Artifacts

- `CONTEXT.md`
- `docs/adr/0001-git-artifacts-are-the-knowledge-source.md`
- `docs/adr/0002-generate-host-adapters-from-one-protocol.md`
