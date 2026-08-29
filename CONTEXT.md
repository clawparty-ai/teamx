# Teamx Collaboration

Teamx is a human-led collaboration context in which people and their AI collaborators work toward a shared goal, settle design questions, and preserve the resulting knowledge.

## Team Work

**Team**:
A group of people and their AI collaborators working toward one shared goal under an owner.
_Avoid_: Agent swarm, worker pool

**Owner**:
The team member accountable for membership decisions, final design decisions, and acceptance of the shared goal.
_Avoid_: Coordinator, lead agent

**Member**:
An approved human participant working with an AI collaborator as part of a team.
_Avoid_: Agent, worker

**Goal**:
The shared outcome that gives a team its purpose and defines when its work may be accepted.
_Avoid_: Task, prompt

## Deliberation

**Clarification**:
A targeted request for a named member to remove ambiguity from ongoing work.
_Avoid_: Design Question, interview question

**Design Session**:
A deliberate, owner-initiated deliberation that resolves a design tree through successive rounds of human decisions.
_Avoid_: Chat, clarification thread

**Design Tree**:
The dependency structure connecting unresolved design questions to the decisions that enable further questions.
_Avoid_: Task tree, conversation history

**Frontier**:
The complete set of design questions whose prerequisites have been settled and can therefore be decided in the current round.
_Avoid_: Backlog, open questions

**Design Question**:
An unresolved choice on a design tree that requires an owner decision after relevant facts and trade-offs are presented.
_Avoid_: Clarification, task

**Fact Request**:
A directed investigation assigned to a member to provide evidence for design questions without settling them.
_Avoid_: Design Question, decision

**Fact Report**:
Evidence returned by a member in response to a fact request for the owner to evaluate during deliberation.
_Avoid_: Answer, Decision Record

**Shared Understanding**:
The owner-confirmed state in which the design tree is exhausted and the agreed artifacts reflect every settled decision.
_Avoid_: Empty frontier, session ended

## Communication and Knowledge

**Broadcast**:
A team-visible informational message that does not by itself establish a durable design decision.
_Avoid_: Decision, Decision Record

**Decision Record**:
The durable statement of an accepted design choice and the rationale that settled it.
_Avoid_: Broadcast, status update

**Superseded Decision**:
A previously accepted decision replaced by a later explicit decision while remaining visible in the team's history.
_Avoid_: Deleted decision, edited decision

**Artifact**:
A versioned repository document that preserves team knowledge or a work product.
_Avoid_: Event payload, database record

**Design Session Record**:
The working artifact that preserves a design session's topic, settled decisions, current frontier, and remaining branches so deliberation can resume.
_Avoid_: Architecture Decision Record, chat transcript

**Glossary**:
The repository document that defines Teamx's canonical domain language.
_Avoid_: Specification, implementation guide

**Architecture Decision Record**:
An artifact that preserves a consequential, non-obvious architectural trade-off and why it was accepted.
_Avoid_: Design document, meeting note

## Protocol

**Deliberation Protocol**:
The host-neutral rules governing design trees, decision rounds, fact gathering, knowledge capture, and completion confirmation.
_Avoid_: Plugin prompt, command help

**Host Adapter**:
A host-specific representation of the deliberation protocol that preserves its semantics without becoming an independent source of truth.
_Avoid_: Protocol copy, alternate workflow
