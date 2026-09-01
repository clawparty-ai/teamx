//! doc_flow.rs — declarative document lifecycle engine (T3).
//!
//! Implements the runtime half of the TEAM.md `## 文档` contract
//! (docs/05-design-teamfile-docs.cn.md, §5.2 + §6):
//!
//!   * `DocMeta` — `.meta.json` mirror of a doc instance's state + audit trail;
//!   * `can_create` / `can_advance` — permission checks against the declared
//!     `创建者` / `审批者` / `所有者` roles;
//!   * `validate_transition` — the *dynamic state machine*: a `from -> to`
//!     move must follow the declared `状态流` chain (unless backward/reopen);
//!   * `load_spec` — read the `.teamx/docs/_spec/<key>.json` contract snapshots
//!     produced by bootstrap (T2); the snapshots themselves are written by
//!     `bootstrap_from_teamfile` in `commands.rs`.
//!
//! All checks are pure functions over `DocSpec`: a failed check means the
//! caller must NOT write an event or mutate `.meta.json` (design §6.4).

// T3 builds the engine + tests; T4 wires it to CLI/agent flows. The module
// is exercised by its unit tests today, so silence unused-code for the APIs
// that the next milestone will call from `commands.rs`.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use crate::teamfile::DocSpec;

// ---------------------------------------------------------------------------
// DocMeta — `.meta.json` for one doc instance
// ---------------------------------------------------------------------------

/// The `.meta.json` of a single document instance.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DocMeta {
    /// Document type key (must match a `_spec/<key>.json` spec).
    pub doc: String,
    /// Instance id (e.g. `001-order-flow`).
    pub id: String,
    /// Current state (one of the spec's declared `states`).
    pub state: String,
    /// Owning role of this instance (from the spec's `owner`).
    pub owner: String,
    pub created_at: String,
    pub updated_at: String,
    /// Transition history (oldest first); event_seq ties to the team ledger.
    pub history: Vec<MetaStep>,
    /// Optional task semantics (used by the built-in `taskx` type): the member
    /// this task is assigned to, whether it needs a human executor, and priority.
    /// Absent for ordinary document types.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
}

/// One state transition recorded in `.meta.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MetaStep {
    pub state: String,
    pub by: String,
    pub at: String,
    pub event_seq: i64,
}

impl DocMeta {
    /// Path of the `.meta.json` for a doc instance under `.teamx/docs/`.
    pub fn meta_path(docs_root: &Path, doc_key: &str, id: &str) -> PathBuf {
        docs_root.join(doc_key).join(format!("{id}.meta.json"))
    }

    pub fn load(path: &Path) -> Result<DocMeta, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| format!("serialize meta: {e}"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
        }
        // Atomic replace (CR-022 M2): write a temp file next to the target,
        // then rename, so a crash mid-write cannot leave a half-written `.meta.json`.
        let tmp = path.with_extension("meta.json.tmp");
        std::fs::write(&tmp, &text).map_err(|e| format!("write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("rename {}: {e}", path.display()))
    }
}

// ---------------------------------------------------------------------------
// Spec loading — from the T2 `_spec/<key>.json` snapshots
// ---------------------------------------------------------------------------

/// Load a doc contract from `.teamx/docs/_spec/<key>.json`.
///
/// The built-in `taskx` type has no required on-disk spec: if the file is
/// absent (or the docs root itself is missing), we fall back to
/// [`builtin_taskx_spec`], so `teamx task` works out of the box without a
/// TEAM.md `## 文档` declaration. A team may still override `taskx` by writing
/// its own `_spec/taskx.json` (e.g. to adjust the state flow or reactions).
pub fn load_spec(docs_root: &Path, doc_key: &str) -> Result<DocSpec, String> {
    let path = docs_root.join("_spec").join(format!("{doc_key}.json"));
    if !path.exists() && doc_key == BUILTIN_TASKX {
        return Ok(builtin_taskx_spec());
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))
}

/// The built-in task document key.
pub const BUILTIN_TASKX: &str = "taskx";

/// The built-in `taskx` document contract — the executable spec behind
/// `teamx task`. Unlike TEAM.md-declared doc types, this one always exists, so
/// task assignment works on any team without declaring a `## 文档` section.
///
/// State flow: `assigned -> acked -> claimed -> in_progress -> done -> verified`.
/// `claimed` may be skipped (small tasks go straight to in_progress). A
/// `help_requested` event is a notification-only interrupt that leaves the
/// state unchanged (task stays in_progress); the lead responds by updating the
/// doc. Reject/reopen are backward moves.
pub fn builtin_taskx_spec() -> DocSpec {
    use crate::teamfile::DocReaction;
    DocSpec {
        key: BUILTIN_TASKX.to_string(),
        title: "任务".to_string(),
        purpose: Some("team lead 派发任务，成员以文档为中心完成并提交".to_string()),
        template: vec![
            "目标".to_string(),
            "验收标准".to_string(),
            "assignee".to_string(),
            "executor".to_string(),
            "priority".to_string(),
            "进展".to_string(),
            "结果".to_string(),
        ],
        creators: Vec::new(), // anyone may create (lead role enforced by command layer)
        owner: "lead".to_string(),
        // `owner` = the team owner role key, `lead` = the built-in lead label
        // used by reactions; both may advance task state.
        approvers: vec!["lead".to_string(), "owner".to_string()],
        states: vec![
            "assigned".to_string(),
            "acked".to_string(),
            "claimed".to_string(),
            "in_progress".to_string(),
            "done".to_string(),
            "verified".to_string(),
        ],
        reactions: vec![
            DocReaction {
                on: "created".to_string(),
                to_role: None, // assignee-directed notification handled by cmd_task
                action: "查看新任务并开始".to_string(),
            },
            DocReaction {
                on: "done".to_string(),
                to_role: Some("lead".to_string()),
                action: "验收已完成的任务".to_string(),
            },
            DocReaction {
                on: "help_requested".to_string(),
                to_role: Some("lead".to_string()),
                action: "查看成员的求助请求并回应".to_string(),
            },
        ],
    }
}

// ---------------------------------------------------------------------------
// Declarative permission + transition checks (design §6.4)
// ---------------------------------------------------------------------------

/// Can `role` create this document?
/// `创建者` empty = anyone; otherwise the role must be listed.
pub fn can_create(spec: &DocSpec, role: &str) -> bool {
    spec.creators.is_empty() || spec.creators.iter().any(|c| c == role)
}

/// Can `role` advance this document's state?
/// Allowed if the role is the `所有者` or listed in `审批者`.
pub fn can_advance(spec: &DocSpec, role: &str) -> bool {
    role == spec.owner || spec.approvers.iter().any(|a| a == role)
}

/// Validate a state transition `from -> to` against the declared `状态流`.
///
/// * Both states must exist in the chain;
/// * `forward = true` (normal events like `doc.reviewed`/`doc.approved`):
///   `to` must come strictly after `from` in the chain;
/// * `backward = true` (reject/reopen events): allowed only for reject/reopen
///   semantics — `to` must not be a *forward* jump past `from` unless the team
///   declared it that way; we allow any move that is not the identity, leaving
///   the exact reject policy to the reaction rules.
pub fn validate_transition(spec: &DocSpec, from: &str, to: &str, backward: bool) -> Result<(), String> {
    let states = &spec.states;
    let fi = states
        .iter()
        .position(|s| s == from)
        .ok_or_else(|| format!("state `{from}` is not in declared flow {states:?}"))?;
    let ti = states
        .iter()
        .position(|s| s == to)
        .ok_or_else(|| format!("state `{to}` is not in declared flow {states:?}"))?;
    if fi == ti {
        return Err(format!("no-op transition `{from} -> {to}`"));
    }
    if backward {
        // Reject/reopen: allow moving backward OR forward to a review state;
        // forbid jumping straight to a terminal state unless declared adjacent.
        return Ok(());
    }
    if ti > fi {
        Ok(())
    } else {
        Err(format!(
            "illegal transition `{from} -> {to}`: forward flow is {states:?}"
        ))
    }
}

/// The result of applying a lifecycle event to a doc instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocEventKind {
    /// `doc.created` — new instance.
    Created,
    /// `doc.updated` — content change, state unchanged.
    Updated,
    /// `doc.reviewed` / `doc.approved` / `doc.closed` — forward state move.
    Forward,
    /// `doc.rejected` / `doc.reopened` — backward / reopen move.
    Backward,
}

/// Classify a doc lifecycle event name into its transition semantics.
/// Unknown event names are treated as `Forward` (best-effort) — the strict
/// validation lives in `validate_transition`.
pub fn classify_event(event: &str) -> DocEventKind {
    match event {
        "doc.created" => DocEventKind::Created,
        "doc.updated" => DocEventKind::Updated,
        "doc.rejected" | "doc.reopened" => DocEventKind::Backward,
        // Task-lifecycle events: forward moves along the taskx state flow.
        // `doc.help_requested` is a notification-only interrupt — it does not
        // advance state (handled by cmd_task before apply_event), so here it
        // classifies as Forward but callers skip the transition for it.
        "doc.claimed" | "doc.acknowledged" | "doc.done" | "doc.verified" | "doc.help_requested" => {
            DocEventKind::Forward
        }
        _ => DocEventKind::Forward,
    }
}

/// Apply one lifecycle event to a `DocMeta`: validate permission + transition,
/// then produce the *next* meta. Pure — does not touch disk or the ledger; the
/// caller persists the returned meta and writes the event (so a failed check
/// has zero side effects, per design §6.4).
///
/// * `actor_role` — used for the permission checks (`can_create`/`can_advance`).
/// * `actor_label` — human-readable identity recorded in the audit trail
///   (`MetaStep.by`); prefer a member display name / id over the bare role so
///   that two members sharing a role remain distinguishable (CR-022 L1).
///
/// Returns the updated `DocMeta` on success.
pub fn apply_event(
    spec: &DocSpec,
    meta: &DocMeta,
    actor_role: &str,
    actor_label: &str,
    event: &str,
    to: Option<&str>,
    seq: i64,
) -> Result<DocMeta, String> {
    // Single timestamp for the whole event so `updated_at` and the step's `at`
    // agree (CR-022 G4).
    let now = crate::events::db_now();
    let kind = classify_event(event);
    match kind {
        DocEventKind::Created => {
            // Creation: actor must be allowed to create; doc must start at the
            // first declared state (or the explicitly requested `to`).
            if !can_create(spec, actor_role) {
                return Err(format!(
                    "role `{actor_role}` may not create doc `{}` (creators: {:?})",
                    spec.key, spec.creators
                ));
            }
            let first = spec.states.first().ok_or_else(|| {
                format!("doc `{}` has no declared states", spec.key)
            })?;
            let state = to.unwrap_or(first).to_string();
            if !spec.states.contains(&state) {
                return Err(format!(
                    "state `{state}` is not in declared flow {:?}",
                    spec.states
                ));
            }
            let mut next = meta.clone();
            next.state = state.clone();
            next.owner = spec.owner.clone();
            next.updated_at = now.clone();
            next.history.push(MetaStep {
                state,
                by: actor_label.to_string(),
                at: now,
                event_seq: seq,
            });
            Ok(next)
        }
        DocEventKind::Updated => {
            // Content change: any member may update; state unchanged.
            let mut next = meta.clone();
            next.updated_at = now.clone();
            next.history.push(MetaStep {
                state: meta.state.clone(),
                by: actor_label.to_string(),
                at: now,
                event_seq: seq,
            });
            Ok(next)
        }
        DocEventKind::Forward => {
            if !can_advance(spec, actor_role) {
                return Err(format!(
                    "role `{actor_role}` may not advance doc `{}` (owner: {}, approvers: {:?})",
                    spec.key, spec.owner, spec.approvers
                ));
            }
            let to = to.ok_or_else(|| "forward event requires target state".to_string())?;
            validate_transition(spec, &meta.state, to, false)?;
            let mut next = meta.clone();
            next.state = to.to_string();
            next.updated_at = now.clone();
            next.history.push(MetaStep {
                state: to.to_string(),
                by: actor_label.to_string(),
                at: now,
                event_seq: seq,
            });
            Ok(next)
        }
        DocEventKind::Backward => {
            if !can_advance(spec, actor_role) {
                return Err(format!(
                    "role `{actor_role}` may not reject/reopen doc `{}` (owner: {}, approvers: {:?})",
                    spec.key, spec.owner, spec.approvers
                ));
            }
            let to = to.ok_or_else(|| "backward event requires target state".to_string())?;
            validate_transition(spec, &meta.state, to, true)?;
            let mut next = meta.clone();
            next.state = to.to_string();
            next.updated_at = now.clone();
            next.history.push(MetaStep {
                state: to.to_string(),
                by: actor_label.to_string(),
                at: now,
                event_seq: seq,
            });
            Ok(next)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::teamfile::parse_team_file_text;

    fn spec_for(text: &str, key: &str) -> DocSpec {
        let tf = parse_team_file_text(text).unwrap();
        tf.docs
            .into_iter()
            .find(|d| d.key == key)
            .expect("doc key present")
    }

    const TEAM: &str = r#"# T
## 文档
### requirements
- 标题: 需求文档
- 创建者: [pm]
- 所有者: pm
- 审批者: [reviewer, owner]
- 状态流: draft -> review -> approved -> done

### issue
- 创建者: []
- 所有者: team-lead
- 状态流: opened -> triaged -> fixing -> closed
"#;

    #[test]
    fn can_create_respects_creators() {
        let spec = spec_for(TEAM, "requirements");
        assert!(can_create(&spec, "pm"));
        assert!(!can_create(&spec, "ui-dev"));
        // empty creators = anyone
        let issue = spec_for(TEAM, "issue");
        assert!(can_create(&issue, "anyone"));
        assert!(can_create(&issue, "qa"));
    }

    #[test]
    fn can_advance_respects_owner_and_approvers() {
        let spec = spec_for(TEAM, "requirements");
        assert!(can_advance(&spec, "pm")); // owner
        assert!(can_advance(&spec, "reviewer"));
        assert!(can_advance(&spec, "owner")); // role literally named owner
        assert!(!can_advance(&spec, "ui-dev"));
        assert!(!can_advance(&spec, "pm2"));
    }

    #[test]
    fn validate_transition_forward_and_backward() {
        let spec = spec_for(TEAM, "requirements");
        assert!(validate_transition(&spec, "draft", "review", false).is_ok());
        assert!(validate_transition(&spec, "review", "approved", false).is_ok());
        assert!(validate_transition(&spec, "approved", "done", false).is_ok());
        // skipping is allowed forward (approved -> done directly is adjacent, but
        // draft -> done is a jump — still forward, allowed by chain order)
        assert!(validate_transition(&spec, "draft", "approved", false).is_ok());
        // backward without the flag is rejected
        assert!(validate_transition(&spec, "approved", "draft", false).is_err());
        // backward flag allows it
        assert!(validate_transition(&spec, "approved", "draft", true).is_ok());
        // unknown states
        assert!(validate_transition(&spec, "draft", "closed", false).is_err());
        assert!(validate_transition(&spec, "bogus", "review", false).is_err());
        // no-op
        assert!(validate_transition(&spec, "draft", "draft", false).is_err());
    }

    #[test]
    fn apply_created_permissions() {
        let spec = spec_for(TEAM, "requirements");
        let meta = DocMeta {
            doc: "requirements".into(),
            id: "001".into(),
            ..Default::default()
        };
        // allowed creator
        let next = apply_event(&spec, &meta, "pm", "pm", "doc.created", None, 1).unwrap();
        assert_eq!(next.state, "draft"); // starts at first declared state
        assert_eq!(next.owner, "pm");
        assert_eq!(next.history.len(), 1);
        assert_eq!(next.history[0].by, "pm");
        assert_eq!(next.history[0].event_seq, 1);
        // disallowed creator
        let err = apply_event(&spec, &meta, "ui-dev", "ui-dev", "doc.created", None, 2).unwrap_err();
        assert!(err.contains("may not create"), "{err}");
        // explicit initial state
        let spec2 = spec_for(TEAM, "requirements");
        let next = apply_event(&spec2, &meta, "pm", "pm", "doc.created", Some("review"), 3).unwrap();
        assert_eq!(next.state, "review");
        // state not in flow
        let err = apply_event(&spec, &meta, "pm", "pm", "doc.created", Some("bogus"), 4).unwrap_err();
        assert!(err.contains("not in declared flow"), "{err}");
    }

    #[test]
    fn apply_forward_requires_permission_and_valid_move() {
        let spec = spec_for(TEAM, "requirements");
        let meta = DocMeta {
            doc: "requirements".into(),
            id: "001".into(),
            state: "draft".into(),
            owner: "pm".into(),
            ..Default::default()
        };
        // reviewer (approver) can move forward
        let next = apply_event(&spec, &meta, "reviewer", "reviewer", "doc.reviewed", Some("review"), 10).unwrap();
        assert_eq!(next.state, "review");
        // ui-dev (not approver) cannot
        let err = apply_event(&spec, &meta, "ui-dev", "ui-dev", "doc.reviewed", Some("review"), 11).unwrap_err();
        assert!(err.contains("may not advance"), "{err}");
        // backward move with forward event rejected
        let meta_approved = DocMeta {
            state: "approved".into(),
            ..meta.clone()
        };
        let err = apply_event(&spec, &meta_approved, "pm", "pm", "doc.approved", Some("draft"), 12).unwrap_err();
        assert!(err.contains("illegal transition"), "{err}");
    }

    #[test]
    fn apply_backward_allows_reject() {
        let spec = spec_for(TEAM, "requirements");
        let meta = DocMeta {
            doc: "requirements".into(),
            id: "001".into(),
            state: "review".into(),
            owner: "pm".into(),
            ..Default::default()
        };
        let next = apply_event(&spec, &meta, "reviewer", "reviewer", "doc.rejected", Some("draft"), 20).unwrap();
        assert_eq!(next.state, "draft");
        assert_eq!(next.history.len(), 1);
        // reopen from done back to review
        let meta2 = DocMeta {
            state: "done".into(),
            ..meta.clone()
        };
        let next2 = apply_event(&spec, &meta2, "pm", "pm", "doc.reopened", Some("review"), 21).unwrap();
        assert_eq!(next2.state, "review");
    }

    #[test]
    fn apply_updated_keeps_state() {
        let spec = spec_for(TEAM, "requirements");
        let meta = DocMeta {
            doc: "requirements".into(),
            id: "001".into(),
            state: "draft".into(),
            owner: "pm".into(),
            ..Default::default()
        };
        let next = apply_event(&spec, &meta, "anyone", "anyone", "doc.updated", None, 30).unwrap();
        assert_eq!(next.state, "draft"); // unchanged
        assert_eq!(next.history.len(), 1);
        assert_eq!(next.history[0].by, "anyone");
    }

    #[test]
    fn meta_roundtrip_via_disk() {
        let dir = std::env::temp_dir().join(format!("teamx-docflow-{}", std::process::id()));
        let p = DocMeta::meta_path(&dir, "requirements", "001");
        let meta = DocMeta {
            doc: "requirements".into(),
            id: "001".into(),
            state: "approved".into(),
            owner: "pm".into(),
            created_at: "now".into(),
            updated_at: "now".into(),
            history: vec![MetaStep {
                state: "approved".into(),
                by: "reviewer".into(),
                at: "now".into(),
                event_seq: 5,
            }],
            assignee: None,
            executor: None,
            priority: None,
        };
        meta.save(&p).unwrap();
        let loaded = DocMeta::load(&p).unwrap();
        assert_eq!(loaded, meta);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn spec_roundtrip_via_disk() {
        let dir = std::env::temp_dir().join(format!("teamx-docflow-spec-{}", std::process::id()));
        let _ = std::fs::create_dir_all(dir.join("_spec"));
        let spec = spec_for(TEAM, "requirements");
        let sp = dir.join("_spec").join("requirements.json");
        let text = serde_json::to_string_pretty(&spec).unwrap();
        std::fs::write(&sp, &text).unwrap();
        let loaded = load_spec(&dir, "requirements").unwrap();
        assert_eq!(loaded, spec);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------------------------------------------------------------------
    // Built-in taskx spec
    // ---------------------------------------------------------------------

    #[test]
    fn builtin_taskx_spec_loads_without_disk_file() {
        // No `_spec/taskx.json` on disk anywhere -> falls back to the built-in.
        let dir = std::env::temp_dir().join(format!("teamx-taskx-none-{}", std::process::id()));
        let spec = load_spec(&dir, BUILTIN_TASKX).expect("builtin taskx spec always loads");
        assert_eq!(spec.key, "taskx");
        assert_eq!(spec.states, vec!["assigned", "acked", "claimed", "in_progress", "done", "verified"]);
        assert_eq!(spec.approvers, vec!["lead", "owner"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn taskx_state_machine_advances_and_rejects() {
        let spec = builtin_taskx_spec();
        let meta = DocMeta {
            doc: "taskx".into(),
            id: "t1".into(),
            state: "assigned".into(),
            owner: "lead".into(),
            assignee: Some("m-1".into()),
            ..Default::default()
        };
        // assigned -> acked -> claimed -> in_progress -> done -> verified
        let m1 = apply_event(&spec, &meta, "lead", "m-1", "doc.acknowledged", Some("acked"), 1).unwrap();
        assert_eq!(m1.state, "acked");
        let m2 = apply_event(&spec, &m1, "lead", "m-1", "doc.claimed", Some("claimed"), 2).unwrap();
        assert_eq!(m2.state, "claimed");
        let m3 = apply_event(&spec, &m2, "lead", "m-1", "doc.updated", None, 3).unwrap();
        assert_eq!(m3.state, "claimed"); // updated keeps state
        let m4 = apply_event(&spec, &m3, "lead", "m-1", "doc.done", Some("done"), 4).unwrap();
        assert_eq!(m4.state, "done");
        let m5 = apply_event(&spec, &m4, "owner", "m-0", "doc.verified", Some("verified"), 5).unwrap();
        assert_eq!(m5.state, "verified");
        // forward jumps are allowed by the chain (assigned -> verified directly)
        let jump = apply_event(&spec, &meta, "owner", "m-0", "doc.verified", Some("verified"), 6).unwrap();
        assert_eq!(jump.state, "verified");
        // backward: reject done -> assigned
        let m6 = apply_event(&spec, &m4, "lead", "m-0", "doc.rejected", Some("assigned"), 7).unwrap();
        assert_eq!(m6.state, "assigned");
    }
}
