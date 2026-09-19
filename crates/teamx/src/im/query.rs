//! IM read-only queries (P1): five self-scoped read faces over the team's
//! state, for bound IM users arriving through the gateway.
//!
//! RPC-layer auth: every query resolves the IM identity through the binding
//! table (`resolve_member`) — unbound / revoked identities are rejected
//! before any data is read. Queries are strictly self-scoped: `tasks`
//! returns only the caller's assignments, `questions` only those targeted
//! at the caller; `goal` / `status` / `discussions` expose team-shared
//! surfaces any active member can already read. Every query is audited to
//! the ledger with `source: im:<platform>` (same style as the notifier).

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use crate::db;
use super::binding;

/// The five read-only kinds exposed to bound IM users.
pub const KINDS: &[&str] = &["status", "goal", "tasks", "discussions", "questions"];

/// Clamp a caller-provided limit into a sane range.
fn clamp_limit(raw: i64) -> i64 {
    raw.clamp(1, 50)
}

/// Resolve + execute one read-only query. `platform` / `im_user_id` come from
/// the gateway; identity is bound server-side (never trusted from the wire).
pub fn query(
    conn: &mut Connection,
    platform: &str,
    im_user_id: &str,
    team_id: &str,
    kind: &str,
    limit: i64,
) -> Result<Value, String> {
    // RPC-layer auth: only an active binding may query. `resolve_member`
    // refreshes last_seen_at on success (same side effect as the gateway's
    // other calls) and returns None for unbound/revoked identities.
    let ident = binding::resolve_member(conn, platform, im_user_id, team_id)?
        .ok_or_else(|| "im.query: no active binding for this IM identity".to_string())?;
    let member_id = ident["member_id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let member_name = ident["display_name"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let limit = clamp_limit(limit);

    let data = match kind {
        "status" => status(conn, team_id, &member_id)?,
        "goal" => goal(conn, team_id)?,
        "tasks" => tasks(conn, team_id, &member_id)?,
        "discussions" => discussions(conn, team_id, limit)?,
        "questions" => questions(conn, team_id, &member_id)?,
        other => {
            return Err(format!(
                "im.query: unknown kind `{other}` (expected one of {})",
                KINDS.join("/")
            ))
        }
    };

    // Ledger audit: every IM query lands with source=im:<platform> so the
    // audit trail distinguishes IM-originated reads from CLI reads.
    let (mid, tid) = (member_id.clone(), team_id.to_string());
    let payload = json!({
        "source": format!("im:{platform}"),
        "im_user": im_user_id,
        "kind": kind,
        "member_id": member_id,
        "limit": limit,
    });
    db::with_write(conn, |tx| {
        crate::events::emit(tx, &tid, Some(&mid), "im.query", Some(&payload))
    })
    .map_err(|e: rusqlite::Error| e.to_string())?;

    Ok(json!({
        "ok": true,
        "kind": kind,
        "member": {
            "member_id": member_id,
            "display_name": member_name,
            "team_id": team_id,
        },
        "data": data,
    }))
}

/// Team snapshot: latest goal state + the caller's own open counts.
fn status(conn: &Connection, team_id: &str, member_id: &str) -> Result<Value, String> {
    let goal: Option<(String, String)> = conn
        .query_row(
            "SELECT title, state FROM goals WHERE team_id = ?1 ORDER BY updated_at DESC LIMIT 1",
            params![team_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let my_open_tasks: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM taskx WHERE team_id = ?1 AND assignee_member_id = ?2
              AND state NOT IN ('done','verified')",
            params![team_id, member_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let my_open_questions: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM questions WHERE team_id = ?1 AND target_member_id = ?2
              AND state = 'open'",
            params![team_id, member_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "goal": goal.map(|(title, state)| json!({ "title": title, "state": state })),
        "my_open_tasks": my_open_tasks,
        "my_open_questions": my_open_questions,
    }))
}

/// The team's latest goal (title/body/state) — a shared surface.
fn goal(conn: &Connection, team_id: &str) -> Result<Value, String> {
    let goal = conn
        .query_row(
            "SELECT title, body, state, updated_at FROM goals WHERE team_id = ?1
              ORDER BY updated_at DESC LIMIT 1",
            params![team_id],
            |r| {
                Ok(json!({
                    "title": r.get::<_, String>(0)?,
                    "body": r.get::<_, Option<String>>(1)?,
                    "state": r.get::<_, String>(2)?,
                    "updated_at": r.get::<_, String>(3)?,
                }))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?;
    Ok(json!({ "goal": goal }))
}

/// Tasks assigned to the caller only (never the whole team's board).
fn tasks(conn: &Connection, team_id: &str, member_id: &str) -> Result<Value, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, title, state, priority, updated_at FROM taskx
              WHERE team_id = ?1 AND assignee_member_id = ?2
              ORDER BY updated_at DESC LIMIT 20",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![team_id, member_id], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "title": r.get::<_, String>(1)?,
                "state": r.get::<_, String>(2)?,
                "priority": r.get::<_, String>(3)?,
                "updated_at": r.get::<_, String>(4)?,
            }))
        })
        .map_err(|e| e.to_string())?;
    let tasks: Vec<Value> = rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| e.to_string())?;
    Ok(json!({ "tasks": tasks }))
}

/// Recent team discussions (shared surface), newest first, trimmed to
/// `{number, title, messages, closed, task_id}` per thread.
fn discussions(conn: &Connection, team_id: &str, limit: i64) -> Result<Value, String> {
    let listed = crate::discussion::list_for_team(conn, team_id, true)?;
    let mut out: Vec<Value> = listed["discussions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .take(limit as usize)
        .map(|t| {
            json!({
                "number": t.get("number").cloned().unwrap_or(json!(0)),
                "title": t.get("title").cloned().unwrap_or(json!("")),
                "messages": t.get("messages").cloned().unwrap_or(json!(0)),
                "closed": t.get("closed").cloned().unwrap_or(json!(false)),
                "task_id": t.get("task_id").cloned().unwrap_or(json!("")),
                // design/007：brainstorm 线程暴露 mode/phase（IM 只读查询面可见）
                "mode": t.get("mode").cloned().unwrap_or(Value::Null),
                "phase": t.get("phase").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    out.truncate(limit as usize);
    Ok(json!({ "discussions": out }))
}

/// Open questions targeted at the caller (with asker display name).
fn questions(conn: &Connection, team_id: &str, member_id: &str) -> Result<Value, String> {
    let mut stmt = conn
        .prepare(
            "SELECT q.id, q.question, q.created_at, COALESCE(m.display_name, q.asker_member_id)
               FROM questions q LEFT JOIN members m ON m.id = q.asker_member_id AND m.team_id = q.team_id
              WHERE q.team_id = ?1 AND q.target_member_id = ?2 AND q.state = 'open'
              ORDER BY q.created_at DESC LIMIT 10",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![team_id, member_id], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "question": r.get::<_, String>(1)?,
                "created_at": r.get::<_, String>(2)?,
                "asked_by": r.get::<_, String>(3)?,
            }))
        })
        .map_err(|e| e.to_string())?;
    let questions: Vec<Value> =
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| e.to_string())?;
    Ok(json!({ "questions": questions }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        db::migrate(&conn).unwrap();
        conn
    }

    /// One team, an owner + a plain member. Returns `(conn, team_id)`.
    fn seed() -> (Connection, String) {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO teams (id, name, owner_member_id, goal_id, state, invite_token, created_at, updated_at)
             VALUES ('t1','T','m-owner',NULL,'active','tok','now','now')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO members (id, team_id, session_key, display_name, role, state, is_lead, joined_at)
             VALUES ('m-owner','t1','s-owner','Owner','owner','active',1,'now'),
                    ('m-worker','t1','s-worker','Worker','contributor','active',0,'now')",
            [],
        )
        .unwrap();
        (conn, "t1".to_string())
    }

    /// Bind `im_user` to `member` via the binding table directly (the create/
    /// redeem path is covered by binding.rs tests; here we only need rows).
    fn bind(conn: &Connection, im_user: &str, member_id: &str) {
        conn.execute(
            "INSERT INTO im_bindings (platform, im_user_id, member_id, team_id, bound_at, bound_by)
             VALUES ('feishu', ?1, ?2, 't1', 'now', 'test')",
            params![im_user, member_id],
        )
        .unwrap();
    }

    fn add_goal(conn: &Connection, title: &str, state: &str) {
        conn.execute(
            "INSERT INTO goals (id, team_id, title, state, created_at, updated_at)
             VALUES ('g1','t1',?1,?2,'now','now')",
            params![title, state],
        )
        .unwrap();
    }

    fn add_task(conn: &Connection, id: &str, assignee: &str, state: &str) {
        conn.execute(
            "INSERT INTO taskx (id, team_id, title, state, created_by_member_id, created_at, updated_at)
             VALUES (?1,'t1',?1,?2,'m-owner','now','now');",
            params![id, state],
        )
        .unwrap();
        conn.execute(
            "UPDATE taskx SET assignee_member_id = ?1 WHERE id = ?2",
            params![assignee, id],
        )
        .unwrap();
    }

    fn add_question(conn: &Connection, id: &str, asker: &str, target: &str, state: &str) {
        conn.execute(
            "INSERT INTO questions (id, team_id, asker_member_id, target_member_id, question, state, created_at)
             VALUES (?1,'t1',?2,?3,'please answer',?4,'now')",
            params![id, asker, target, state],
        )
        .unwrap();
    }

    #[test]
    fn unbound_identity_is_rejected() {
        let (mut conn, team) = seed();
        let err = query(&mut conn, "feishu", "nobody", &team, "status", 10).unwrap_err();
        assert!(err.contains("no active binding"), "{err}");
    }

    #[test]
    fn unknown_kind_is_rejected() {
        let (mut conn, team) = seed();
        bind(&conn, "u1", "m-worker");
        let err = query(&mut conn, "feishu", "u1", &team, "secrets", 10).unwrap_err();
        assert!(err.contains("unknown kind"), "{err}");
    }

    #[test]
    fn status_reports_goal_and_self_counts() {
        let (mut conn, team) = seed();
        bind(&conn, "u1", "m-worker");
        add_goal(&conn, "Ship it", "in_progress");
        add_task(&conn, "task-1", "m-worker", "assigned");
        add_task(&conn, "task-2", "m-owner", "assigned");
        add_question(&conn, "q1", "m-owner", "m-worker", "open");

        let v = query(&mut conn, "feishu", "u1", &team, "status", 10).unwrap();
        assert_eq!(v["data"]["goal"]["state"], json!("in_progress"));
        assert_eq!(v["data"]["my_open_tasks"], json!(1));
        assert_eq!(v["data"]["my_open_questions"], json!(1));
        assert_eq!(v["member"]["member_id"], json!("m-worker"));
    }

    #[test]
    fn tasks_are_scoped_to_the_caller() {
        let (mut conn, team) = seed();
        bind(&conn, "u1", "m-worker");
        add_task(&conn, "task-1", "m-worker", "assigned");
        add_task(&conn, "task-2", "m-owner", "assigned");

        let v = query(&mut conn, "feishu", "u1", &team, "tasks", 10).unwrap();
        let list = v["data"]["tasks"].as_array().unwrap();
        assert_eq!(list.len(), 1, "only the caller's tasks: {v}");
        assert_eq!(list[0]["id"], json!("task-1"));
    }

    #[test]
    fn questions_only_target_the_caller() {
        let (mut conn, team) = seed();
        bind(&conn, "u1", "m-worker");
        add_question(&conn, "q1", "m-owner", "m-worker", "open");
        add_question(&conn, "q2", "m-owner", "m-owner", "open");
        add_question(&conn, "q3", "m-owner", "m-worker", "answered");

        let v = query(&mut conn, "feishu", "u1", &team, "questions", 10).unwrap();
        let list = v["data"]["questions"].as_array().unwrap();
        assert_eq!(list.len(), 1, "only open questions targeting the caller: {v}");
        assert_eq!(list[0]["id"], json!("q1"));
        assert_eq!(list[0]["asked_by"], json!("Owner"));
    }

    #[test]
    fn discussions_use_the_team_shared_surface() {
        let (mut conn, team) = seed();
        bind(&conn, "u1", "m-worker");
        // One discussion thread, derived from ledger events.
        let did = "dsc_test1";
        db::with_write(&mut conn, |tx| {
            crate::events::emit(
                tx,
                "t1",
                Some("m-owner"),
                "discussion.created",
                Some(&json!({ "discussion_id": did, "title": "Design", "task_id": "" })),
            )?;
            crate::events::emit(
                tx,
                "t1",
                Some("m-owner"),
                "discussion.message",
                Some(&json!({ "discussion_id": did, "message_id": format!("{did}#1") })),
            )?;
            Ok(())
        })
        .unwrap();

        let v = query(&mut conn, "feishu", "u1", &team, "discussions", 10).unwrap();
        let list = v["data"]["discussions"].as_array().unwrap();
        assert_eq!(list.len(), 1, "{v}");
        assert_eq!(list[0]["title"], json!("Design"));
        assert_eq!(list[0]["messages"], json!(1));
    }

    #[test]
    fn every_query_is_audited_with_im_source() {
        let (mut conn, team) = seed();
        bind(&conn, "u1", "m-worker");
        add_goal(&conn, "Ship it", "in_progress");
        query(&mut conn, "feishu", "u1", &team, "status", 10).unwrap();
        query(&mut conn, "feishu", "u1", &team, "tasks", 10).unwrap();

        let rows: Vec<(String, String)> = conn
            .prepare("SELECT type, payload_json FROM events WHERE type = 'im.query'")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(rows.len(), 2, "one audit event per query");
        for (t, p) in rows {
            assert_eq!(t, "im.query");
            let payload: Value = serde_json::from_str(&p).unwrap();
            assert_eq!(payload["source"], json!("im:feishu"));
            assert_eq!(payload["member_id"], json!("m-worker"));
        }
    }

    #[test]
    fn limit_is_clamped() {
        let (mut conn, team) = seed();
        bind(&conn, "u1", "m-worker");
        // Out-of-range limits must not panic; they clamp into [1, 50].
        for raw in [0, -5, 999] {
            let v = query(&mut conn, "feishu", "u1", &team, "discussions", raw).unwrap();
            assert_eq!(v["ok"], json!(true));
        }
    }
}
