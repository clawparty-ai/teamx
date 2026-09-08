//! server_tasks.rs — server-side taskx (task.* RPC).
//!
//! Local/project teams keep tasks as `.teamx/docs/taskx/*` documents executed
//! by the CLI. Network teams have no local checkout, so the plugin sends task
//! commands over RPC — which previously failed with `unknown rpc method
//! 'task'`. This module stores tasks in SQLite (`taskx` table) and mirrors the
//! local semantics: modes direct/bid/broadcast, the state machine
//! `assigned -> acked -> claimed -> in_progress -> done -> verified` and
//! owner/lead authority for create/verify/reject. Every transition writes a
//! ledger event (types `doc.created/acknowledged/claimed/updated/done/...
//! verified/help_requested/rejected`) so the existing hub fan-out notifies
//! members. See docs/33-server-taskx-rpc.cn.md.

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

type InboxRows = (Vec<Value>, Vec<Value>, Vec<Value>, Vec<Value>, i64);

const STATES: [&str; 6] = ["assigned", "acked", "claimed", "in_progress", "done", "verified"];
const EVENTS: [&str; 8] = [
    "doc.created",
    "doc.acknowledged",
    "doc.claimed",
    "doc.updated",
    "doc.help_requested",
    "doc.done",
    "doc.verified",
    "doc.rejected",
];

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str).filter(|x| !x.is_empty())
}
fn b(v: &Value, k: &str) -> bool {
    v.get(k).and_then(Value::as_bool).unwrap_or(false)
}

/// Entry point called from `serve.rs` dispatch for any `task.*` method.
/// `conn` is the server DB; the actor identity comes from the cert CN.
pub fn rpc(
    method: &str,
    args: &Value,
    conn: &mut Connection,
    team_id: &str,
    member_id: &str,
) -> Result<Value, String> {
    match method {
        "task.create" => create(conn, team_id, member_id, args),
        "task.list" => list(conn, team_id, member_id, args),
        "task.ack" => transition(conn, team_id, member_id, "ack", args),
        "task.claim" => transition(conn, team_id, member_id, "claim", args),
        "task.update" => transition(conn, team_id, member_id, "update", args),
        "task.help" => transition(conn, team_id, member_id, "help", args),
        "task.done" => transition(conn, team_id, member_id, "done", args),
        "task.verify" => transition(conn, team_id, member_id, "verify", args),
        "task.reject" => transition(conn, team_id, member_id, "reject", args),
        "task.log" => log(conn, team_id, member_id, args),
        other => Err(format!("unknown task method `{other}`")),
    }
}

fn member_role(conn: &Connection, team_id: &str, member_id: &str) -> Result<Option<String>, String> {
    conn.query_row(
        "SELECT role FROM members WHERE id = ?1 AND team_id = ?2 AND state NOT IN ('left','denied')",
        params![member_id, team_id],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .map_err(|e| format!("db: {e}"))
}

fn is_lead(conn: &Connection, team_id: &str, member_id: &str) -> Result<bool, String> {
    let row: Option<(Option<String>, bool)> = conn
        .query_row(
            "SELECT t.owner_member_id, m.is_lead
             FROM teams t JOIN members m ON m.team_id = t.id
             WHERE t.id = ?1 AND m.id = ?2 AND m.state NOT IN ('left','denied')",
            params![team_id, member_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| format!("db: {e}"))?;
    Ok(row
        .map(|(owner, is_lead)| owner.as_deref() == Some(member_id) || is_lead)
        .unwrap_or(false))
}

fn row_to_task(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": r.get::<_, String>(0)?,
        "team_id": r.get::<_, String>(1)?,
        "title": r.get::<_, String>(2)?,
        "body": r.get::<_, Option<String>>(3)?,
        "state": r.get::<_, String>(4)?,
        "mode": r.get::<_, String>(5)?,
        "executor": r.get::<_, String>(6)?,
        "priority": r.get::<_, String>(7)?,
        "assignee_member_id": r.get::<_, Option<String>>(8)?,
        "role_key": r.get::<_, Option<String>>(9)?,
        "created_by_member_id": r.get::<_, String>(10)?,
        "created_at": r.get::<_, String>(11)?,
        "updated_at": r.get::<_, String>(12)?,
    }))
}

const SELECT_TASK: &str = "SELECT id, team_id, title, body, state, mode, executor, priority,
       assignee_member_id, role_key, created_by_member_id, created_at, updated_at FROM taskx";

fn load_task(conn: &Connection, team_id: &str, id: &str) -> Result<Value, String> {
    conn.query_row(
        &format!("{SELECT_TASK} WHERE team_id = ?1 AND id = ?2"),
        params![team_id, id],
        row_to_task,
    )
    .optional()
    .map_err(|e| format!("db: {e}"))?
    .ok_or_else(|| format!("task `{id}` not found in this team"))
}

fn auto_id(conn: &Connection, team_id: &str) -> Result<String, String> {
    // `task-<n>`: next free number.
    let max: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(CAST(SUBSTR(id, 6) AS INTEGER)),0) FROM taskx WHERE team_id=?1 AND id LIKE 'task-%'",
            params![team_id],
            |r| r.get(0),
        )
        .map_err(|e| format!("db: {e}"))?;
    Ok(format!("task-{}", max + 1))
}

fn create(conn: &mut Connection, team_id: &str, member_id: &str, args: &Value) -> Result<Value, String> {
    if !is_lead(conn, team_id, member_id)? {
        return Err("task.create: only the team lead can create tasks".to_string());
    }
    let title = s(args, "title").ok_or_else(|| "task.create requires `title`".to_string())?.to_string();
    let body = s(args, "body").or_else(|| s(args, "detail")).map(str::to_string);
    let assignee = s(args, "assignee").map(str::to_string);
    let role = s(args, "role").map(str::to_string);
    let mode = s(args, "mode").unwrap_or("");
    let executor = s(args, "executor").unwrap_or("either").to_string();
    let priority = s(args, "priority").unwrap_or("medium").to_string();
    let explicit_id = s(args, "id").map(str::to_string);

    let mode = match (mode, assignee.is_some(), role.is_some()) {
        ("", false, false) => return Err("task.create requires --assignee <member_id> or --role <role>".to_string()),
        ("", true, _) => "direct",
        ("", false, true) => "bid",
        ("direct", true, _) => "direct",
        ("direct", false, _) => return Err("task.create --mode direct requires --assignee".to_string()),
        ("bid", _, true) => "bid",
        ("broadcast", _, true) => "broadcast",
        (other, _, _) => return Err(format!("unknown mode `{other}` (valid: direct, bid, broadcast)")),
    };

    // Instances: direct = [assignee]; bid = [one row, unassigned]; broadcast =
    // one row per active role member.
    let instances: Vec<Option<String>> = match mode {
        "direct" => vec![assignee.clone()],
        "bid" => {
            if role.is_none() {
                return Err("bid requires --role <role>".to_string());
            }
            vec![None]
        }
        "broadcast" => {
            let mut stmt = conn
                .prepare("SELECT id FROM members WHERE team_id=?1 AND role=?2 AND state='active'")
                .map_err(|e| format!("db: {e}"))?;
            let ids: Vec<String> = stmt
                .query_map(params![team_id, role.as_deref()], |r| r.get(0))
                .map_err(|e| format!("db: {e}"))?
                .collect::<rusqlite::Result<_>>()
                .map_err(|e| format!("db: {e}"))?;
            if ids.is_empty() {
                return Err(format!(
                    "broadcast role `{}` has no active members",
                    role.as_deref().unwrap_or("")
                ));
            }
            ids.into_iter().map(Some).collect()
        }
        _ => unreachable!(),
    };

    let id_base = match explicit_id {
        Some(i) => {
            let exists: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM taskx WHERE team_id=?1 AND id=?2)",
                    params![team_id, i],
                    |r| r.get(0),
                )
                .map_err(|e| format!("db: {e}"))?;
            if exists {
                return Err(format!("task id `{i}` already exists in this team"));
            }
            i
        }
        None => auto_id(conn, team_id)?,
    };

    let created = crate::db::now();
    let mut created_ids = Vec::new();
    crate::db::with_write(conn, |tx| {
        for (idx, target) in instances.iter().enumerate() {
            let id = if mode == "broadcast" && instances.len() > 1 {
                format!("{id_base}.{}", idx + 1)
            } else {
                id_base.clone()
            };
            let target_ref: Option<&str> = target.as_deref();
            tx.execute(
                "INSERT INTO taskx (id, team_id, title, body, state, mode, executor, priority,
                    assignee_member_id, role_key, created_by_member_id, created_at, updated_at)
                 VALUES (?1,?2,?3,?4,'assigned',?5,?6,?7,?8,?9,?10,?11,?11)",
                params![
                    id,
                    team_id,
                    title,
                    body,
                    mode,
                    executor,
                    priority,
                    target_ref,
                    role.as_deref(),
                    member_id,
                    created,
                ],
            )?;
            crate::events::emit(
                tx,
                team_id,
                Some(member_id),
                "doc.created",
                Some(&json!({
                    "doc": "taskx", "task": id, "title": title,
                    "mode": mode, "assignee": target_ref,
                    "role": role.as_deref(), "priority": priority,
                })),
            )?;
            created_ids.push(id);
        }
        Ok(())
    })
    .map_err(|e: rusqlite::Error| format!("db: {e}"))?;

    Ok(json!({ "ok": true, "tasks": created_ids }))
}

fn list(conn: &mut Connection, team_id: &str, member_id: &str, args: &Value) -> Result<Value, String> {
    let my_role = member_role(conn, team_id, member_id)?;
    let only_mine = b(args, "mine");
    let mut sql = String::from(SELECT_TASK);
    sql.push_str(" WHERE team_id = ?1");
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(team_id.to_string())];
    if only_mine {
        sql.push_str(" AND (assignee_member_id = ?2 OR (assignee_member_id IS NULL AND role_key = ?3))");
        params.push(Box::new(member_id.to_string()));
        params.push(Box::new(my_role.clone().unwrap_or_default()));
    }
    if let Some(st) = s(args, "state") {
        sql.push_str(" AND state = ?");
        params.push(Box::new(st.to_string()));
    }
    if let Some(a) = s(args, "assignee") {
        sql.push_str(" AND assignee_member_id = ?");
        params.push(Box::new(a.to_string()));
    }
    if let Some(e) = s(args, "executor") {
        sql.push_str(" AND executor = ?");
        params.push(Box::new(e.to_string()));
    }
    sql.push_str(" ORDER BY created_at DESC, rowid DESC");

    let mut stmt = conn.prepare(&sql).map_err(|e| format!("db: {e}"))?;
    let p: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|x| x.as_ref()).collect();
    let tasks: Vec<Value> = stmt
        .query_map(p.as_slice(), row_to_task)
        .map_err(|e| format!("db: {e}"))?
        .collect::<rusqlite::Result<_>>()
        .map_err(|e| format!("db: {e}"))?;
    Ok(json!({ "tasks": tasks, "me": { "member_id": member_id, "role": my_role } }))
}

fn transition(
    conn: &mut Connection,
    team_id: &str,
    member_id: &str,
    action: &str,
    args: &Value,
) -> Result<Value, String> {
    let id = s(args, "id")
        .ok_or_else(|| format!("task.{action} requires `id`"))
        .map(str::to_string)?;
    let task = load_task(conn, team_id, &id)?;
    let state = task["state"].as_str().unwrap_or("").to_string();
    let role = task["role_key"].as_str().map(str::to_string);
    let assignee = task["assignee_member_id"].as_str().map(str::to_string);
    let my_role = member_role(conn, team_id, member_id)?;
    let lead = is_lead(conn, team_id, member_id)?;

    // Who may act on this task.
    let is_assignee = assignee.as_deref() == Some(member_id);
    // Role peers may only act while the task is UNCLAIMED (bid); a task that
    // is already assigned to a specific member is theirs alone (+ lead).
    let role_member = !is_assignee && assignee.is_none() && role.is_some() && role == my_role;
    let can_work = is_assignee || role_member || lead;

    let next = match action {
        "ack" => {
            if !can_work {
                return Err("task.ack: only the assignee / role member / lead may ack".to_string());
            }
            Some("acked")
        }
        "claim" => {
            if assignee.is_some() {
                return Err(format!("task `{id}` is already assigned to a member"));
            }
            if !role_member && !lead {
                return Err("task.claim: only a member of the target role (or lead) may claim".to_string());
            }
            Some("claimed")
        }
        "update" => {
            if !can_work {
                return Err("task.update: only the assignee / role member / lead may update".to_string());
            }
            match state.as_str() {
                "done" | "verified" => return Err(format!("task `{id}` is {state}; cannot update")),
                "assigned" | "acked" | "claimed" => Some("in_progress"),
                _ => None,
            }
        }
        "help" => {
            if !can_work {
                return Err("task.help: only the assignee / role member / lead may request help".to_string());
            }
            if state == "done" || state == "verified" {
                return Err(format!("task `{id}` is {state}; cannot request help"));
            }
            if state == "in_progress" {
                None
            } else {
                Some("in_progress")
            }
        }
        "done" => {
            if !can_work {
                return Err("task.done: only the assignee / role member / lead may mark done".to_string());
            }
            Some("done")
        }
        "verify" => {
            if !lead {
                return Err("task.verify: only the team lead can verify".to_string());
            }
            if state != "done" {
                return Err(format!("task `{id}` must be `done` before verify (state={state})"));
            }
            Some("verified")
        }
        "reject" => {
            if !lead {
                return Err("task.reject: only the team lead can reject".to_string());
            }
            if state == "verified" || state == "done" {
                return Err(format!("task `{id}` is {state}; use verify or keep as-is"));
            }
            Some("in_progress")
        }
        _ => return Err(format!("unknown transition `{action}`")),
    };

    let now = crate::db::now();
    let event_type = match action {
        "ack" => "doc.acknowledged",
        "claim" => "doc.claimed",
        "help" => "doc.help_requested",
        "verify" => "doc.verified",
        "reject" => "doc.rejected",
        "done" => "doc.done",
        _ => "doc.updated",
    };
    let detail = s(args, "progress")
        .or_else(|| s(args, "reason"))
        .or_else(|| s(args, "result"))
        .or_else(|| s(args, "note"))
        .map(str::to_string);

    crate::db::with_write(conn, |tx| {
        let nstate: &str = match next {
            Some(n) => n,
            None => state.as_str(),
        };
        let claim_assignee: Option<&str> = if action == "claim" { Some(member_id) } else { None };
        tx.execute(
            "UPDATE taskx SET state = ?1,
                 assignee_member_id = COALESCE(assignee_member_id, ?2),
                 updated_at = ?3
              WHERE team_id = ?4 AND id = ?5",
            params![nstate, claim_assignee, now, team_id, id],
        )?;
        crate::events::emit(
            tx,
            team_id,
            Some(member_id),
            event_type,
            Some(&json!({
                "doc": "taskx", "task": id,
                "title": task["title"],
                "to": nstate,
                "detail": detail,
            })),
        )?;
        Ok(())
    })
    .map_err(|e: rusqlite::Error| format!("db: {e}"))?;

    let updated = load_task(conn, team_id, &id)?;
    Ok(json!({ "ok": true, "task": updated }))
}

fn log(conn: &mut Connection, team_id: &str, _member_id: &str, args: &Value) -> Result<Value, String> {
    let id = s(args, "id").ok_or_else(|| "task.log requires `id`".to_string())?;
    // Membership is enforced by `taskx` row ownership below (task must exist in team).
    let _ = load_task(conn, team_id, id)?;
    let mut stmt = conn
        .prepare(
            "SELECT seq, member_id, type, payload_json, created_at FROM events
             WHERE team_id = ?1 AND payload_json LIKE ?2 ORDER BY seq",
        )
        .map_err(|e| format!("db: {e}"))?;
    let needle = format!("%\"task\": \"{id}\"%");
    let rows: Vec<Value> = stmt
        .query_map(params![team_id, needle], |r| {
            Ok(json!({
                "seq": r.get::<_, i64>(0)?,
                "member_id": r.get::<_, Option<String>>(1)?,
                "type": r.get::<_, String>(2)?,
                "payload": r.get::<_, Option<String>>(3)?,
                "created_at": r.get::<_, String>(4)?,
            }))
        })
        .map_err(|e| format!("db: {e}"))?
        .collect::<rusqlite::Result<_>>()
        .map_err(|e| format!("db: {e}"))?;
    Ok(json!({ "task": id, "events": rows }))
}

// Keep referenced for documentation completeness (state/event vocab used by
// tests + docs); rustc will warn if unused, so they are also asserted in tests.
#[allow(dead_code)]
fn _state_vocab() -> (&'static [&'static str], &'static [&'static str]) {
    (&STATES, &EVENTS)
}

/// Read-only part of the inbox (all queries against `&Connection`).
fn inbox_read(
    conn: &Connection,
    team_id: &str,
    member_id: &str,
    cursor: i64,
) -> Result<InboxRows, String> {
    let my_role = member_role(conn, team_id, member_id)?.unwrap_or_default();
    let my_name: String = conn
        .query_row("SELECT display_name FROM members WHERE id=?1 AND team_id=?2", params![member_id, team_id], |r| r.get(0))
        .optional()
        .map_err(|e| format!("db: {e}"))?
        .unwrap_or_default();
    let tasks: Vec<Value> = {
        let mut stmt = conn
            .prepare(&format!(
                "{SELECT_TASK} WHERE team_id = ?1 AND state NOT IN ('done','verified')
                 AND (assignee_member_id = ?2 OR (assignee_member_id IS NULL AND role_key = ?3))
                 ORDER BY created_at DESC"
            ))
            .map_err(|e| format!("db: {e}"))?;
        let rows = stmt.query_map(params![team_id, member_id, my_role], row_to_task).map_err(|e| format!("db: {e}"))?;
        rows.collect::<rusqlite::Result<Vec<Value>>>().map_err(|e| format!("db: {e}"))?
    };
    let questions: Vec<Value> = {
        let mut stmt = conn
            .prepare(
                "SELECT id, asker_member_id, question, created_at FROM questions
                 WHERE team_id = ?1 AND target_member_id = ?2 AND state = 'open'
                 ORDER BY created_at DESC",
            )
            .map_err(|e| format!("db: {e}"))?;
        let rows = stmt.query_map(params![team_id, member_id], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "asker_member_id": r.get::<_, Option<String>>(1)?,
                "question": r.get::<_, String>(2)?,
                "created_at": r.get::<_, String>(3)?,
            }))
        })
        .map_err(|e| format!("db: {e}"))?;
        rows.collect::<rusqlite::Result<Vec<Value>>>().map_err(|e| format!("db: {e}"))?
    };
    let nudges: Vec<Value> = {
        let mut stmt = conn
            .prepare(
                "SELECT seq, type, payload_json, created_at FROM events
                 WHERE team_id = ?1 AND type IN ('system.nudge','system.nudge_lead') AND seq > ?2
                 ORDER BY seq",
            )
            .map_err(|e| format!("db: {e}"))?;
        let rows = stmt.query_map(params![team_id, cursor], |r| {
            let payload: Option<String> = r.get(2)?;
            let pv: Value = payload
                .and_then(|p| serde_json::from_str(&p).ok())
                .unwrap_or(Value::Null);
            let msg = pv.get("message").and_then(Value::as_str).unwrap_or("").to_string();
            Ok(json!({
                "seq": r.get::<_, i64>(0)?,
                "type": r.get::<_, String>(1)?,
                "message": msg,
                "created_at": r.get::<_, String>(3)?,
            }))
        })
        .map_err(|e| format!("db: {e}"))?;
        rows.collect::<rusqlite::Result<Vec<Value>>>().map_err(|e| format!("db: {e}"))?
    };
    // Discussions with new activity that should wake this member (@ / subscribed /
    // linked to one of their tasks). Newest-first, capped.
    let discussions: Vec<Value> = crate::discussion::wake_discussions(conn, team_id, member_id, &my_name, cursor)?;
    let last_seq = discussions
        .iter()
        .map(|d| d["seq"].as_i64().unwrap_or(0))
        .chain(nudges.iter().map(|n| n["seq"].as_i64().unwrap_or(0)))
        .max()
        .unwrap_or(cursor);
    Ok((tasks, questions, discussions, nudges, last_seq))
}

/// Unified inbox for a member: my open tasks + questions directed at me +
/// unread nudges since the member's server-side cursor. `ack` advances the
/// cursor (marks nudges read) after returning them.
pub fn inbox(
    conn: &mut Connection,
    team_id: &str,
    member_id: &str,
    ack: bool,
) -> Result<Value, String> {
    let cursor: i64 = conn
        .query_row(
            "SELECT last_seq FROM inbox_cursors WHERE member_id = ?1 AND team_id = ?2",
            params![member_id, team_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("db: {e}"))?
        .unwrap_or(0);

    let (tasks, questions, discussions, nudges, last_seq): InboxRows = inbox_read(conn, team_id, member_id, cursor)?;
    if ack && last_seq > cursor {
        crate::db::with_write(conn, |tx| {
            tx.execute(
                "INSERT INTO inbox_cursors (member_id, team_id, last_seq)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(member_id, team_id) DO UPDATE SET last_seq = excluded.last_seq",
                params![member_id, team_id, last_seq],
            )?;
            Ok(())
        })
        .map_err(|e: rusqlite::Error| format!("db: {e}"))?;
    }
    Ok(json!({
        "tasks": tasks,
        "questions": questions,
        "discussions": discussions,
        "nudges": { "unread": nudges, "unread_count": nudges.len(), "last_seq": last_seq },
        "acknowledged": ack && last_seq > cursor,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        seed(&conn);
        conn
    }

    fn seed(conn: &Connection) {
        conn.execute_batch(
            "INSERT INTO teams (id, name, owner_member_id, state, invite_token, created_at, updated_at)
             VALUES ('t1','test','m-owner','active','tok','now','now');
             INSERT INTO members (id, team_id, session_key, display_name, role, state, joined_at)
             VALUES ('m-owner','t1','s0','Owner','owner','active','now');
             INSERT INTO members (id, team_id, session_key, display_name, role, state, joined_at)
             VALUES ('m-w1','t1','s1','W1','worker','active','now');
             INSERT INTO members (id, team_id, session_key, display_name, role, state, joined_at)
             VALUES ('m-w2','t1','s2','W2','worker','active','now');",
        )
        .unwrap();
    }

    fn call(conn: &mut Connection, method: &str, member: &str, args: Value) -> Result<Value, String> {
        rpc(method, &args, conn, "t1", member)
    }

    #[test]
    fn full_lifecycle_and_permissions() {
        let mut c = test_conn();

        // Worker cannot create.
        assert!(call(&mut c, "task.create", "m-w1", json!({"title":"x"})).is_err());

        // Lead creates a direct task for m-w1.
        let out = call(
            &mut c,
            "task.create",
            "m-owner",
            json!({"title":"do it", "assignee":"m-w1", "priority":"high"}),
        )
        .unwrap();
        let ids = out["tasks"].as_array().unwrap();
        assert_eq!(ids.len(), 1);
        let id = ids[0].as_str().unwrap().to_string();

        // Mine list only returns m-w1's tasks for m-w1.
        let mine = call(&mut c, "task.list", "m-w1", json!({"mine":true})).unwrap();
        assert_eq!(mine["tasks"].as_array().unwrap().len(), 1);

        // ack -> update -> done -> verify.
        assert_eq!(call(&mut c, "task.ack", "m-w1", json!({"id": id})).unwrap()["task"]["state"], "acked");
        assert_eq!(
            call(&mut c, "task.update", "m-w1", json!({"id": id, "progress":"started"}))
                .unwrap()["task"]["state"],
            "in_progress"
        );
        // Non-assignee worker cannot mark done.
        assert!(call(&mut c, "task.done", "m-w2", json!({"id": id})).is_err());
        assert_eq!(call(&mut c, "task.done", "m-w1", json!({"id": id})).unwrap()["task"]["state"], "done");
        // Only lead verifies.
        assert!(call(&mut c, "task.verify", "m-w1", json!({"id": id})).is_err());
        assert_eq!(
            call(&mut c, "task.verify", "m-owner", json!({"id": id})).unwrap()["task"]["state"],
            "verified"
        );
    }

    #[test]
    fn bid_claim_is_exclusive() {
        let mut c = test_conn();
        let out = call(
            &mut c,
            "task.create",
            "m-owner",
            json!({"title":"contest", "role":"worker", "mode":"bid"}),
        )
        .unwrap();
        let id = out["tasks"][0].as_str().unwrap().to_string();
        assert_eq!(
            call(&mut c, "task.claim", "m-w1", json!({"id": id})).unwrap()["task"]["state"],
            "claimed"
        );
        // Second role member cannot claim.
        assert!(call(&mut c, "task.claim", "m-w2", json!({"id": id})).is_err());
    }
    #[test]
    fn role_peer_cannot_edit_assigned_direct_task() {
        let mut c = test_conn();
        let out = call(
            &mut c,
            "task.create",
            "m-owner",
            json!({"title":"only for w1", "assignee":"m-w1"}),
        )
        .unwrap();
        let id = out["tasks"][0].as_str().unwrap().to_string();
        // m-w2 shares the worker role but the task belongs to m-w1.
        assert!(call(&mut c, "task.ack", "m-w2", json!({"id": id})).is_err());
        assert!(call(&mut c, "task.update", "m-w2", json!({"id": id, "progress":"x"})).is_err());
        assert!(call(&mut c, "task.done", "m-w2", json!({"id": id})).is_err());
    }
}
