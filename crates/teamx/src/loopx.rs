//! Thin bridge to the `loopx` CLI (a separate, long-running-work state kernel).
//!
//! teamx does not re-implement loopx. It shells out to `loopx status --format json`
//! and extracts a compact stage-progress digest that can be published to the team
//! ledger as a `loopx.progress` event for the owner to sync.

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LoopxDigest {
    pub project: String,
    pub available: bool,
    /// human-readable reason when loopx is unavailable
    pub error: Option<String>,
    pub goal_state: Option<String>,
    pub gate: Option<String>,
    pub next_todo: Option<String>,
    pub quota: Option<String>,
    pub raw: Option<serde_json::Value>,
}

/// Best-effort extraction of stable-ish loopx status fields.
fn extract(digest: &mut LoopxDigest, v: &serde_json::Value) {
    if let Some(obj) = v.as_object() {
        // try several plausible paths across loopx status shapes
        if digest.goal_state.is_none() {
            for key in ["active_goal_state", "goal_state", "state"] {
                if let Some(s) = compact(obj.get(key)) {
                    digest.goal_state = Some(s);
                    break;
                }
            }
        }
        if digest.gate.is_none() {
            for key in ["gate", "user_gate", "current_gate", "top_gate"] {
                if let Some(s) = compact(obj.get(key)) {
                    digest.gate = Some(s);
                    break;
                }
            }
        }
        if digest.next_todo.is_none() {
            for key in ["next_todo", "next_todo_text", "top_todo"] {
                if let Some(s) = compact(obj.get(key)) {
                    digest.next_todo = Some(s);
                    break;
                }
            }
        }
        if digest.quota.is_none() {
            for key in ["quota", "quota_should_run", "interaction_contract"] {
                if let Some(s) = compact(obj.get(key)) {
                    digest.quota = Some(s);
                    break;
                }
            }
        }
    }
}

/// Produce a compact string from a field value: string as-is; nested object ->
/// a short joined summary of its scalar fields; boolean/number -> text.
fn compact(v: Option<&serde_json::Value>) -> Option<String> {
    match v {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(serde_json::Value::Bool(b)) => Some(if *b { "yes".into() } else { "no".into() }),
        Some(serde_json::Value::Number(n)) => Some(n.to_string()),
        Some(serde_json::Value::Object(o)) => {
            // prefer a few known human-facing sub-keys
            for key in ["state", "text", "title", "summary", "label", "kind"] {
                if let Some(s) = compact(o.get(key)) {
                    return Some(s);
                }
            }
            let parts: Vec<String> = o
                .iter()
                .filter_map(|(k, val)| {
                    compact(Some(val)).map(|c| format!("{k}={c}"))
                })
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(", "))
            }
        }
        _ => None,
    }
}

/// Run `loopx status --format json` in the given project directory.
pub fn loopx_status(project: &Path) -> LoopxDigest {
    let mut digest = LoopxDigest {
        project: project.display().to_string(),
        ..Default::default()
    };
    if !project.is_dir() {
        digest.error = Some(format!(
            "project directory does not exist: {}",
            project.display()
        ));
        return digest;
    }
    let output = Command::new("loopx")
        .arg("status")
        .args(["--format", "json"])
        .current_dir(project)
        .output();
    match output {
        Err(e) => {
            digest.error = Some(format!(
                "loopx CLI unavailable (is it installed on PATH?): {e}"
            ));
        }
        Ok(out) if !out.status.success() => {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            digest.error = Some(format!(
                "loopx status exited with {}: {}",
                out.status,
                if stderr.is_empty() {
                    "no loopx project state in this directory".to_string()
                } else {
                    stderr
                }
            ));
        }
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout);
            match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(v) => {
                    digest.available = true;
                    digest.raw = Some(v.clone());
                    extract(&mut digest, &v);
                }
                Err(e) => {
                    digest.error = Some(format!("loopx status returned non-JSON output: {e}"));
                }
            }
        }
    }
    digest
}
