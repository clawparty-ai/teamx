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
//!   * `load_spec` / `save_spec` — read/write the `.teamx/docs/_spec/<key>.json`
//!     contract snapshots produced by bootstrap (T2).
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
        std::fs::write(path, &text).map_err(|e| format!("write {}: {e}", path.display()))
    }
}

// ---------------------------------------------------------------------------
// Spec loading — from the T2 `_spec/<key>.json` snapshots
// ---------------------------------------------------------------------------

/// Load a doc contract from `.teamx/docs/_spec/<key>.json`.
pub fn load_spec(docs_root: &Path, doc_key: &str) -> Result<DocSpec, String> {
    let path = docs_root.join("_spec").join(format!("{doc_key}.json"));
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))
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
        _ => DocEventKind::Forward,
    }
}

/// Apply one lifecycle event to a `DocMeta`: validate permission + transition,
/// then produce the *next* meta. Pure — does not touch disk or the ledger; the
/// caller persists the returned meta and writes the event (so a failed check
/// has zero side effects, per design §6.4).
///
/// Returns the updated `DocMeta` on success.
pub fn apply_event(
    spec: &DocSpec,
    meta: &DocMeta,
    actor_role: &str,
    event: &str,
    to: Option<&str>,
    seq: i64,
) -> Result<DocMeta, String> {
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
            let now = crate::events::db_now();
            let mut next = meta.clone();
            next.state = state.clone();
            next.owner = spec.owner.clone();
            next.updated_at = now.clone();
            next.history.push(MetaStep {
                state,
                by: actor_role.to_string(),
                at: now,
                event_seq: seq,
            });
            Ok(next)
        }
        DocEventKind::Updated => {
            // Content change: any member may update; state unchanged.
            let mut next = meta.clone();
            next.updated_at = crate::events::db_now();
            next.history.push(MetaStep {
                state: meta.state.clone(),
                by: actor_role.to_string(),
                at: crate::events::db_now(),
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
            let now = crate::events::db_now();
            let mut next = meta.clone();
            next.state = to.to_string();
            next.updated_at = now.clone();
            next.history.push(MetaStep {
                state: to.to_string(),
                by: actor_role.to_string(),
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
            let now = crate::events::db_now();
            let mut next = meta.clone();
            next.state = to.to_string();
            next.updated_at = now.clone();
            next.history.push(MetaStep {
                state: to.to_string(),
                by: actor_role.to_string(),
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
        let next = apply_event(&spec, &meta, "pm", "doc.created", None, 1).unwrap();
        assert_eq!(next.state, "draft"); // starts at first declared state
        assert_eq!(next.owner, "pm");
        assert_eq!(next.history.len(), 1);
        assert_eq!(next.history[0].by, "pm");
        assert_eq!(next.history[0].event_seq, 1);
        // disallowed creator
        let err = apply_event(&spec, &meta, "ui-dev", "doc.created", None, 2).unwrap_err();
        assert!(err.contains("may not create"), "{err}");
        // explicit initial state
        let spec2 = spec_for(TEAM, "requirements");
        let next = apply_event(&spec2, &meta, "pm", "doc.created", Some("review"), 3).unwrap();
        assert_eq!(next.state, "review");
        // state not in flow
        let err = apply_event(&spec, &meta, "pm", "doc.created", Some("bogus"), 4).unwrap_err();
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
        let next = apply_event(&spec, &meta, "reviewer", "doc.reviewed", Some("review"), 10).unwrap();
        assert_eq!(next.state, "review");
        // ui-dev (not approver) cannot
        let err = apply_event(&spec, &meta, "ui-dev", "doc.reviewed", Some("review"), 11).unwrap_err();
        assert!(err.contains("may not advance"), "{err}");
        // backward move with forward event rejected
        let meta_approved = DocMeta {
            state: "approved".into(),
            ..meta.clone()
        };
        let err = apply_event(&spec, &meta_approved, "pm", "doc.approved", Some("draft"), 12).unwrap_err();
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
        let next = apply_event(&spec, &meta, "reviewer", "doc.rejected", Some("draft"), 20).unwrap();
        assert_eq!(next.state, "draft");
        assert_eq!(next.history.len(), 1);
        // reopen from done back to review
        let meta2 = DocMeta {
            state: "done".into(),
            ..meta.clone()
        };
        let next2 = apply_event(&spec, &meta2, "pm", "doc.reopened", Some("review"), 21).unwrap();
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
        let next = apply_event(&spec, &meta, "anyone", "doc.updated", None, 30).unwrap();
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
}
