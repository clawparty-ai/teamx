//! Team discussion (`teamx discussion`).
//!
//! Design: `docs/43-design-discussion.cn.md`. Discussions are **ledger events**
//! (`discussion.created` / `discussion.message` / `discussion.closed`) so they
//! inherit the append-only audit trail, cross-member sync and WS push of the
//! event bus — no separate chat table.
//!
//! Message addressing: every `discussion.message` event gets a stable
//! `message_id = "<discussion_id>#<seq>"`. References carry both the target
//! coordinate (`kind` + id) and a literal `text` snapshot so UIs render the
//! quote without a second lookup, and audit never dangles (events are immutable).

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use crate::cli::DiscussionCmd;
use crate::db;
use crate::events;

/// Event type constants.
pub const EV_CREATED: &str = "discussion.created";
pub const EV_MESSAGE: &str = "discussion.message";
pub const EV_CLOSED: &str = "discussion.closed";

fn member_id_for_session(conn: &Connection, session: &str, team_opt: Option<&str>) -> Result<(String, String, String), String> {
    // Reuse the same resolution as other commands: find the member row for a
    // session key in the (optionally specified) team.
    let team_id: String = match team_opt {
        Some(t) => t.to_string(),
        None => {
            // Single-team sessions resolve automatically.
            let mut stmt = conn
                .prepare("SELECT team_id FROM members WHERE session_key = ?1 AND state NOT IN ('left','denied')")
                .map_err(|e| e.to_string())?;
            let mut rows = stmt
                .query_map([session], |r| r.get::<_, String>(0))
                .map_err(|e| e.to_string())?;
            let mut ids = Vec::new();
            while let Some(id) = rows.next().transpose().map_err(|e| e.to_string())? {
                ids.push(id);
            }
            if ids.len() == 1 {
                ids.pop().unwrap()
            } else if ids.is_empty() {
                return Err(format!("session `{session}` is not a member of any team"));
            } else {
                return Err("session belongs to several teams; pass --team <id>".to_string());
            }
        }
    };
    let member = conn
        .query_row(
            "SELECT id, display_name, state FROM members WHERE session_key = ?1 AND team_id = ?2 AND state NOT IN ('left','denied')",
            params![session, team_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("session `{session}` is not an active member of team {team_id}"))?;
    if member.2 == "pending" {
        return Err(format!("member {} is pending; owner approval required", member.1));
    }
    Ok((member.0, member.1, team_id))
}

/// Resolve a message/thread coordinate: `"<discussion_id>#<seq>"` or a bare seq.
fn parse_message_ref(s: &str) -> Option<(String, i64)> {
    if let Some((d, seq)) = s.split_once('#') {
        let seq: i64 = seq.parse().ok()?;
        if d.is_empty() {
            return None;
        }
        return Some((d.to_string(), seq));
    }
    None
}

/// Validate that a `discussion.message` event exists and return a snapshot of
/// its body (used for reply_to / quote rendering).
fn snapshot_message(conn: &Connection, team_id: &str, message_ref: &str) -> Result<Value, String> {
    let (discussion_id, seq) = parse_message_ref(message_ref)
        .ok_or_else(|| format!("invalid message ref `{message_ref}` (expected <discussion_id>#<seq>)"))?;
    let ev = events::list(conn, team_id, None)
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|e| e.seq == seq && e.r#type == EV_MESSAGE)
        .ok_or_else(|| format!("message `{message_ref}` not found"))?;
    let payload = ev.payload.unwrap_or_else(|| json!({}));
    let pid = payload.get("discussion_id").and_then(|x| x.as_str()).unwrap_or("");
    if pid != discussion_id {
        return Err(format!("message `{message_ref}` is not in discussion `{discussion_id}`"));
    }
    Ok(json!({
        "message_id": message_ref,
        "body": payload.get("body").cloned().unwrap_or_else(|| json!("")),
    }))
}

fn create(conn: &mut Connection, title: &str, body: Option<&str>, task: Option<&str>, session: &str, team_opt: Option<&str>) -> Result<Value, String> {
    let (member_id, display_name, team_id) = member_id_for_session(conn, session, team_opt)?;
    if title.trim().is_empty() {
        return Err("discussion title is required".to_string());
    }
    let discussion_id = format!("dsc_{}", uuid_slug());
    let payload = json!({
        "discussion_id": discussion_id,
        "title": title.trim(),
        "body": body.unwrap_or("").to_string(),
        "author": display_name,
        "task_id": task.unwrap_or(""),
    });
    let seq = db::with_write(conn, |tx| events::emit(tx, &team_id, Some(&member_id), EV_CREATED, Some(&payload)))
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "ok": true,
        "discussion_id": discussion_id,
        "created_seq": seq,
        "team_id": team_id,
        "title": title.trim(),
    }))
}

fn say_or_reply(
    conn: &mut Connection,
    discussion_id: &str,
    body: &str,
    quotes_raw: Option<&str>,
    reply_to: Option<&str>,
    session: &str,
    team_opt: Option<&str>,
) -> Result<Value, String> {
    let (member_id, display_name, team_id) = member_id_for_session(conn, session, team_opt)?;
    if body.trim().is_empty() {
        return Err("message body is required".to_string());
    }
    // Verify the discussion exists and is open.
    let created = events::list(conn, &team_id, None)
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|e| e.r#type == EV_CREATED && e.payload.as_ref().and_then(|p| p.get("discussion_id")).and_then(|x| x.as_str()) == Some(discussion_id));
    if created.is_none() {
        return Err(format!("discussion `{discussion_id}` not found in this team"));
    }
    let closed = events::list(conn, &team_id, None)
        .map_err(|e| e.to_string())?
        .into_iter()
        .any(|e| e.r#type == EV_CLOSED && e.payload.as_ref().and_then(|p| p.get("discussion_id")).and_then(|x| x.as_str()) == Some(discussion_id));
    if closed {
        return Err(format!("discussion `{discussion_id}` is closed"));
    }

    // Normalize quotes: accept a JSON array string; each element must carry a
    // snapshot `text` (server refuses client-fabricated text by regenerating it
    // when the coordinate resolves to a known message/event).
    let mut quotes: Vec<Value> = Vec::new();
    if let Some(qs) = quotes_raw {
        let parsed: Value = serde_json::from_str(qs).map_err(|e| format!("quotes must be a JSON array: {e}"))?;
        let arr = parsed.as_array().ok_or_else(|| "quotes must be a JSON array".to_string())?;
        for q in arr {
            let kind = q.get("kind").and_then(|x| x.as_str()).unwrap_or("message");
            let text = q.get("text").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let mut o = q.clone();
            o["text"] = json!(text);
            quotes.push(json!({
                "kind": kind,
                "ref": o.get("ref").cloned().unwrap_or_else(|| json!("")),
                "text": text,
            }));
        }
    }
    // reply_to convenience: validate + snapshot the target message.
    let mut reply_obj = None;
    if let Some(mref) = reply_to {
        let snap = snapshot_message(conn, &team_id, mref)?;
        reply_obj = Some(json!({
            "message_id": mref,
            "text": snap["body"],
        }));
    }

    let seq = db::with_write(conn, |tx| {
        // Predict the next per-team seq inside the same transaction so the
        // message_id embeds the real seq without a second write.
        let next_seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM events WHERE team_id = ?1",
            [&team_id],
            |r| r.get(0),
        )?;
        let payload = json!({
            "discussion_id": discussion_id,
            "message_id": format!("{discussion_id}#{next_seq}"),
            "author": display_name,
            "body": body.trim(),
            "reply_to": reply_obj,
            "quotes": quotes,
        });
        events::emit(tx, &team_id, Some(&member_id), EV_MESSAGE, Some(&payload))
    })
    .map_err(|e: rusqlite::Error| e.to_string())?;

    Ok(json!({
        "ok": true,
        "discussion_id": discussion_id,
        "message_id": format!("{discussion_id}#{seq}"),
        "seq": seq,
    }))
}

fn list(conn: &Connection, all: bool, session: &str, team_opt: Option<&str>) -> Result<Value, String> {
    let (_, _, team_id) = member_id_for_session(conn, session, team_opt)?;
    let evs = events::list(conn, &team_id, None).map_err(|e| e.to_string())?;
    // Group by discussion_id.
    let mut threads: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    for e in &evs {
        let Some(payload) = &e.payload else { continue };
        let Some(did) = payload.get("discussion_id").and_then(|x| x.as_str()) else { continue };
        let entry = threads.entry(did.to_string()).or_insert_with(|| json!({
            "discussion_id": did,
            "title": "",
            "task_id": "",
            "messages": 0,
            "closed": false,
            "created_at": e.created_at,
        }));
        match e.r#type.as_str() {
            EV_CREATED => {
                if let Some(t) = payload.get("title").and_then(|x| x.as_str()) {
                    entry["title"] = json!(t);
                }
                if let Some(t) = payload.get("task_id").and_then(|x| x.as_str()) {
                    entry["task_id"] = json!(t);
                }
                entry["created_at"] = json!(e.created_at.clone());
            }
            EV_MESSAGE => {
                entry["messages"] = json!(entry["messages"].as_i64().unwrap_or(0) + 1);
            }
            EV_CLOSED => {
                entry["closed"] = json!(true);
            }
            _ => {}
        }
    }
    let mut out: Vec<Value> = threads.into_values().collect();
    if !all {
        out.retain(|t| t["closed"].as_bool() != Some(true));
    }
    out.sort_by(|a, b| b["created_at"].as_str().unwrap_or("").cmp(a["created_at"].as_str().unwrap_or("")));
    Ok(json!({ "ok": true, "discussions": out }))
}

fn open(conn: &Connection, id: &str, session: &str, team_opt: Option<&str>) -> Result<Value, String> {
    let (_, _, team_id) = member_id_for_session(conn, session, team_opt)?;
    let evs = events::list(conn, &team_id, None).map_err(|e| e.to_string())?;
    let mut title = String::new();
    let mut closed = false;
    let mut messages = Vec::new();
    for e in &evs {
        let Some(payload) = &e.payload else { continue };
        if payload.get("discussion_id").and_then(|x| x.as_str()) != Some(id) {
            continue;
        }
        match e.r#type.as_str() {
            EV_CREATED => {
                title = payload.get("title").and_then(|x| x.as_str()).unwrap_or("").to_string();
            }
            EV_MESSAGE => {
                let mid = payload.get("message_id").and_then(|x| x.as_str()).unwrap_or(&format!("{id}#{}", e.seq)).to_string();
                messages.push(json!({
                    "seq": e.seq,
                    "message_id": mid,
                    "author": payload.get("author").and_then(|x| x.as_str()).unwrap_or(""),
                    "body": payload.get("body").and_then(|x| x.as_str()).unwrap_or(""),
                    "reply_to": payload.get("reply_to"),
                    "quotes": payload.get("quotes").cloned().unwrap_or_else(|| json!([])),
                    "created_at": e.created_at,
                }));
            }
            EV_CLOSED => closed = true,
            _ => {}
        }
    }
    if title.is_empty() && messages.is_empty() && !closed {
        return Err(format!("discussion `{id}` not found"));
    }
    messages.reverse(); // newest first
    Ok(json!({ "ok": true, "discussion_id": id, "title": title, "closed": closed, "messages": messages }))
}

fn close(conn: &mut Connection, id: &str, session: &str, team_opt: Option<&str>) -> Result<Value, String> {
    let (member_id, display_name, team_id) = member_id_for_session(conn, session, team_opt)?;
    // Only team owner/lead may close a discussion (they hold team authority).
    let is_lead: bool = conn
        .query_row(
            "SELECT (t.owner_member_id = ?1 OR m.is_lead = 1)
             FROM teams t JOIN members m ON m.team_id = t.id
             WHERE t.id = ?2 AND m.id = ?1",
            params![member_id, team_id],
            |r| r.get(0),
        )
        .unwrap_or(false);
    if !is_lead {
        return Err("discussion.close: only the team owner or a co-lead may close a discussion".to_string());
    }
    let evs = events::list(conn, &team_id, None).map_err(|e| e.to_string())?;
    let exists = evs.iter().any(|e| {
        e.r#type == EV_CREATED
            && e.payload.as_ref().and_then(|p| p.get("discussion_id")).and_then(|x| x.as_str()) == Some(id)
    });
    if !exists {
        return Err(format!("discussion `{id}` not found"));
    }
    let payload = json!({ "discussion_id": id, "closed_by": display_name });
    let seq = db::with_write(conn, |tx| events::emit(tx, &team_id, Some(&member_id), EV_CLOSED, Some(&payload)))
        .map_err(|e| e.to_string())?;
    Ok(json!({ "ok": true, "discussion_id": id, "closed_seq": seq }))
}

fn uuid_slug() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let pid = std::process::id();
    format!("{nanos:x}{pid:x}")
}

/// Dispatch for `teamx discussion`.
pub fn run(conn: &mut Connection, cmd: &DiscussionCmd) -> Result<Value, String> {
    match cmd {
        DiscussionCmd::Create { title, body, task, session, team } => {
            create(conn, title, body.as_deref(), task.as_deref(), session, team.as_deref())
        }
        DiscussionCmd::List { all, session, team } => list(conn, *all, session, team.as_deref()),
        DiscussionCmd::Open { id, session, team } => open(conn, id, session, team.as_deref()),
        DiscussionCmd::Say { id, body, quotes, session, team } => {
            say_or_reply(conn, id, body, quotes.as_deref(), None, session, team.as_deref())
        }
        DiscussionCmd::Reply { id, message_id, body, session, team } => {
            say_or_reply(conn, id, body, None, Some(message_id), session, team.as_deref())
        }
        DiscussionCmd::Close { id, session, team } => close(conn, id, session, team.as_deref()),
    }
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

    fn seed(conn: &Connection) -> (String, String, String) {
        let team_id = "t1".to_string();
        let owner = "m-owner".to_string();
        let member = "m-worker".to_string();
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
        (team_id, owner, member)
    }

    #[test]
    fn discussion_flow_with_reply_quote_close() {
        let mut conn = test_conn();
        seed(&conn);
        // owner creates
        let r = create(&mut conn, "Review round2", Some("open"), Some("search-r"), "s-owner", Some("t1")).unwrap();
        let did = r["discussion_id"].as_str().unwrap().to_string();

        // worker posts a message
        let m = say_or_reply(&mut conn, &did, "DOI ok", None, None, "s-worker", Some("t1")).unwrap();
        let msg1 = m["message_id"].as_str().unwrap().to_string();
        assert!(msg1.starts_with(&format!("{did}#")));

        // owner replies to that message (reference)
        let r2 = say_or_reply(&mut conn, &did, "thanks", None, Some(&msg1), "s-owner", Some("t1")).unwrap();
        assert!(r2["ok"] == json!(true));

        // quote a message in a third message
        let q = json!([{ "kind": "message", "ref": msg1, "text": "snapshot" }]);
        let r3 = say_or_reply(&mut conn, &did, "quoting", Some(&q.to_string()), None, "s-worker", Some("t1")).unwrap();
        assert!(r3["ok"] == json!(true));

        // list shows one thread, 3 messages, not closed
        let l = list(&conn, false, "s-owner", Some("t1")).unwrap();
        let arr = l["discussions"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["messages"], json!(3));
        assert_eq!(arr[0]["closed"], json!(false));

        // open returns messages newest-first with reply_to on the reply
        let o = open(&conn, &did, "s-owner", Some("t1")).unwrap();
        let msgs = o["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        let reply = msgs.iter().find(|x| x["body"] == "thanks").unwrap();
        assert!(reply["reply_to"].is_object());

        // non-lead worker cannot close
        assert!(close(&mut conn, &did, "s-worker", Some("t1")).is_err());
        // owner closes
        let c = close(&mut conn, &did, "s-owner", Some("t1")).unwrap();
        assert!(c["ok"] == json!(true));
        // after close, saying is rejected
        assert!(say_or_reply(&mut conn, &did, "late", None, None, "s-worker", Some("t1")).is_err());
        // list excludes closed by default
        let l2 = list(&conn, false, "s-owner", Some("t1")).unwrap();
        assert_eq!(l2["discussions"].as_array().unwrap().len(), 0);
        let l3 = list(&conn, true, "s-owner", Some("t1")).unwrap();
        assert_eq!(l3["discussions"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn discussion_reply_to_unknown_message_rejected() {
        let mut conn = test_conn();
        seed(&conn);
        let did = create(&mut conn, "T", None, None, "s-owner", Some("t1")).unwrap()["discussion_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(say_or_reply(&mut conn, &did, "hi", None, Some(&format!("{did}#9999")), "s-worker", Some("t1")).is_err());
    }

    #[test]
    fn discussion_requires_active_member_of_team() {
        let mut conn = test_conn();
        seed(&conn);
        // Unknown session errors
        assert!(create(&mut conn, "X", None, None, "nobody", Some("t1")).is_err());
        // Cross team: member is only in t1
        assert!(create(&mut conn, "X", None, None, "s-worker", Some("other")).is_err());
    }
}
