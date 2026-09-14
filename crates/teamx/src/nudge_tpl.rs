//! Custom nudge templates (Issue #15): owner/lead-configurable message text
//! for the server's four nudge kinds, stored per team.
//!
//! - Storage: `nudge_templates(team_id, kind, template, updated_by,
//!   updated_at)` — converged-style `CREATE TABLE IF NOT EXISTS` (brand-new
//!   table, no legacy data; keeps user_version untouched per team口径).
//! - Kinds (closed enum, extension = code change + review):
//!   `idle_reminder` / `lead_reminder` / `claim_reminder` / `task_nudge`.
//! - Rendering is **pure placeholder substitution**: `{name}` tokens are
//!   replaced from the caller-supplied vars; if any `{...}` token survives
//!   (unknown placeholder) the render FAILS and the caller falls back to the
//!   built-in default — a broken template can never corrupt a nudge.
//! - set/reset are owner/lead-only (enforced by the command layer) and are
//!   audited to the ledger as `nudge.template`.

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::db;
use crate::events;

/// The four configurable nudge kinds (closed enum).
pub const KINDS: &[&str] = &["idle_reminder", "lead_reminder", "claim_reminder", "task_nudge"];

/// Max template length in characters.
pub const MAX_TEMPLATE_LEN: usize = 2000;

pub fn is_valid_kind(kind: &str) -> bool {
    KINDS.contains(&kind)
}

/// The configured template for `(team_id, kind)`, if any.
pub fn get(conn: &Connection, team_id: &str, kind: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT template FROM nudge_templates WHERE team_id = ?1 AND kind = ?2",
        params![team_id, kind],
        |r| r.get(0),
    )
    .optional()
}

/// Validate + upsert a template and audit `nudge.template (action=set)`.
pub fn set(
    conn: &mut Connection,
    team_id: &str,
    kind: &str,
    template: &str,
    updated_by: &str,
) -> Result<(), String> {
    if !is_valid_kind(kind) {
        return Err(format!(
            "nudge set: unknown kind `{kind}` (expected one of {})",
            KINDS.join("/")
        ));
    }
    if template.chars().count() > MAX_TEMPLATE_LEN {
        return Err(format!(
            "nudge set: template exceeds {} characters (got {})",
            MAX_TEMPLATE_LEN,
            template.chars().count()
        ));
    }
    if template.trim().is_empty() {
        return Err("nudge set: template must not be empty (use reset to fall back)".to_string());
    }
    let now = db::now();
    let kind_s = kind.to_string();
    let tpl = template.to_string();
    let by = updated_by.to_string();
    let tid = team_id.to_string();
    db::with_write(conn, |tx| {
        tx.execute(
            "INSERT INTO nudge_templates (team_id, kind, template, updated_by, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(team_id, kind) DO UPDATE SET
               template = excluded.template,
               updated_by = excluded.updated_by,
               updated_at = excluded.updated_at",
            params![tid, kind_s, tpl, by, now],
        )?;
        events::emit(
            tx,
            &tid,
            Some(updated_by),
            "nudge.template",
            Some(&json!({ "action": "set", "kind": kind_s, "updated_by": by })),
        )?;
        Ok(())
    })
    .map_err(|e: rusqlite::Error| e.to_string())
}

/// Delete the template for `(team_id, kind)`; returns true when a row was
/// removed. Audits `nudge.template (action=reset)`.
pub fn reset(conn: &mut Connection, team_id: &str, kind: &str, updated_by: &str) -> Result<bool, String> {
    if !is_valid_kind(kind) {
        return Err(format!(
            "nudge reset: unknown kind `{kind}` (expected one of {})",
            KINDS.join("/")
        ));
    }
    let kind_s = kind.to_string();
    let by = updated_by.to_string();
    let tid = team_id.to_string();
    let deleted = db::with_write(conn, |tx| {
        let n = tx.execute(
            "DELETE FROM nudge_templates WHERE team_id = ?1 AND kind = ?2",
            params![tid, kind_s],
        )?;
        events::emit(
            tx,
            &tid,
            Some(updated_by),
            "nudge.template",
            Some(&json!({ "action": "reset", "kind": kind_s, "updated_by": by })),
        )?;
        Ok(n > 0)
    })
    .map_err(|e: rusqlite::Error| e.to_string())?;
    Ok(deleted)
}

/// Render the configured template for `(team_id, kind)` with pure
/// placeholder substitution.
///
/// Returns `None` when no template is configured OR the render fails (the
/// rendered text still contains a `{...}` token — unknown placeholder), in
/// which case the caller must fall back to the built-in default message.
pub fn render(conn: &Connection, team_id: &str, kind: &str, vars: &Value) -> Option<String> {
    let template = get(conn, team_id, kind).ok()??;
    let mut out = template;
    if let Value::Object(map) = vars {
        for (k, v) in map {
            let token = format!("{{{k}}}");
            let replacement = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            out = out.replace(&token, &replacement);
        }
    }
    // Unknown leftover placeholder → render failure (fallback to default).
    if out.contains('{') && out.contains('}') {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.pragma_update(None, "journal_mode", "WAL").ok();
        db::migrate(&c).unwrap();
        c
    }

    fn seed_team(c: &Connection, id: &str) {
        c.execute(
            "INSERT INTO teams (id, name, owner_member_id, goal_id, state, invite_token, created_at, updated_at)
             VALUES (?1,'T','m1',NULL,'active','tok','now','now')",
            params![id],
        )
        .unwrap();
    }

    #[test]
    fn set_get_reset_roundtrip_and_audit() {
        let mut c = conn();
        seed_team(&c, "t1");
        assert!(get(&c, "t1", "idle_reminder").unwrap().is_none());
        set(&mut c, "t1", "idle_reminder", "自定义 {team_name}", "m1").unwrap();
        assert_eq!(get(&c, "t1", "idle_reminder").unwrap().as_deref(), Some("自定义 {team_name}"));
        // upsert overwrites
        set(&mut c, "t1", "idle_reminder", "v2", "m1").unwrap();
        assert_eq!(get(&c, "t1", "idle_reminder").unwrap().as_deref(), Some("v2"));
        assert!(reset(&mut c, "t1", "idle_reminder", "m1").unwrap());
        assert!(!reset(&mut c, "t1", "idle_reminder", "m1").unwrap(), "second reset deletes nothing");
        assert!(get(&c, "t1", "idle_reminder").unwrap().is_none());
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM events WHERE type='nudge.template'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 4, "set + set + reset x2 audited (no-op reset is still an owner action)");
    }

    #[test]
    fn set_rejects_unknown_kind_empty_and_oversized() {
        let mut c = conn();
        seed_team(&c, "t1");
        assert!(set(&mut c, "t1", "bogus", "x", "m1").is_err(), "unknown kind");
        assert!(set(&mut c, "t1", "idle_reminder", "   ", "m1").is_err(), "empty");
        let big = "x".repeat(MAX_TEMPLATE_LEN + 1);
        assert!(set(&mut c, "t1", "idle_reminder", &big, "m1").is_err(), "oversized");
        assert!(set(&mut c, "t1", "idle_reminder", &"x".repeat(MAX_TEMPLATE_LEN), "m1").is_ok());
    }

    #[test]
    fn render_substitutes_known_placeholders_and_falls_back_on_unknown() {
        let mut c = conn();
        seed_team(&c, "t1");
        // No template → None (caller uses default).
        assert!(render(&c, "t1", "idle_reminder", &json!({"team_name":"T"})).is_none());
        set(&mut c, "t1", "idle_reminder", "团队「{team_name}」目标「{goal_title}」静默 {silent_for_secs}s", "m1").unwrap();
        let out = render(
            &c,
            "t1",
            "idle_reminder",
            &json!({"team_name":"T","goal_title":"G","silent_for_secs":300}),
        )
        .unwrap();
        assert_eq!(out, "团队「T」目标「G」静默 300s");
        // Non-string vars render via JSON (numbers are the common case).
        set(&mut c, "t1", "claim_reminder", "{count} 个待认领：{tasks}", "m1").unwrap();
        assert_eq!(
            render(&c, "t1", "claim_reminder", &json!({"count":2,"tasks":"`a`"})).unwrap(),
            "2 个待认领：`a`"
        );
        // Unknown placeholder survives → render failure → None.
        set(&mut c, "t1", "task_nudge", "你有 {count} 个：{bogus}", "m1").unwrap();
        assert!(render(&c, "t1", "task_nudge", &json!({"count":1})).is_none());
    }
}
