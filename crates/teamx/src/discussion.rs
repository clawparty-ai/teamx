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

/// Message body length cap (chars).
pub const MAX_BODY: usize = 4096;
/// Max quoted objects per message.
pub const MAX_QUOTES: usize = 8;
/// Per-quote snapshot text cap (chars).
pub const MAX_QUOTE_TEXT: usize = 500;

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

/// Read the member's per-topic discussion quota (default 5).
fn member_quota(conn: &Connection, member_id: &str, team_id: &str) -> Result<i64, String> {
    conn.query_row(
        "SELECT discussion_quota FROM members WHERE id = ?1 AND team_id = ?2",
        params![member_id, team_id],
        |r| r.get::<_, i64>(0),
    )
    .map_err(|e| format!("read discussion_quota: {e}"))
}

/// Does `body` mention `@name`? Case-insensitive, word-boundary match so
/// `@Alice` does not match inside `@Alice2`. The char before/after the name
/// must not be alphanumeric/underscore/@.
fn body_mentions(body: &str, name: &str) -> bool {
    let b = body.to_lowercase();
    let n = name.trim().to_lowercase();
    if n.is_empty() {
        return false;
    }
    let needle = format!("@{n}");
    let mut start = 0;
    while let Some(pos) = b[start..].find(&needle) {
        let abs = start + pos;
        let before_ok = abs == 0
            || !b[..abs]
                .chars()
                .last()
                .map(|c| c.is_alphanumeric() || c == '_' || c == '@')
                .unwrap_or(false);
        let after = &b[abs + needle.len()..];
        let after_ok = after.is_empty()
            || !after
                .chars()
                .next()
                .map(|c| c.is_alphanumeric() || c == '_' || c == '@')
                .unwrap_or(false);
        if before_ok && after_ok {
            return true;
        }
        start = abs + needle.len();
    }
    false
}

/// Whether a reply (whose target message ref is `reply_target`) is a response
/// to a message that mentions `@<display_name>` — i.e. a "mention reply" that
/// must NOT consume the member's active-post quota.
fn is_mention_reply(
    conn: &Connection,
    team_id: &str,
    display_name: &str,
    reply_target: Option<&str>,
) -> Result<bool, String> {
    let Some(mref) = reply_target else { return Ok(false) };
    let snap = snapshot_message(conn, team_id, mref)?;
    let body = snap["body"].as_str().unwrap_or("");
    Ok(body_mentions(body, display_name))
}

/// Count this member's ACTIVE posts in one discussion (i.e. messages that were
/// NOT mention replies). Per-topic: scoped to `discussion_id`.
fn active_posts_in(conn: &Connection, team_id: &str, member_id: &str, display_name: &str, discussion_id: &str) -> Result<i64, String> {
    let evs = events::list(conn, team_id, None).map_err(|e| e.to_string())?;
    let mut n = 0i64;
    for e in evs {
        if e.r#type != EV_MESSAGE || e.member_id.as_deref() != Some(member_id) {
            continue;
        }
        let Some(p) = &e.payload else { continue };
        if p.get("discussion_id").and_then(|v| v.as_str()) != Some(discussion_id) {
            continue;
        }
        let reply_target = p.get("reply_to").and_then(|r| r.get("message_id")).and_then(|v| v.as_str());
        if is_mention_reply(conn, team_id, display_name, reply_target)? {
            continue;
        }
        n += 1;
    }
    Ok(n)
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
    if body.chars().count() > MAX_BODY {
        return Err(format!("message body too long (max {MAX_BODY} chars)"));
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

    // Per-topic participation quota: only ACTIVE posts consume quota. A reply
    // responding to a message that mentions @<self> is a "mention reply" and is
    // always allowed (does not count toward the cap, never blocked).
    let mention_reply = is_mention_reply(conn, &team_id, &display_name, reply_to)?;
    if !mention_reply {
        let quota = member_quota(conn, &member_id, &team_id)?;
        let used = active_posts_in(conn, &team_id, &member_id, &display_name, discussion_id)?;
        if used >= quota {
            return Err(format!(
                "discussion quota reached for this topic ({used}/{quota}); you may only reply when mentioned (@{display_name})"
            ));
        }
    }

    // Normalize quotes: accept a JSON array string. When the coordinate is a
    // resolvable message (`kind=message`, ref `discussion#seq`), the server
    // regenerates the snapshot `text` from the ledger so clients cannot forge
    // quoted text. Other kinds carry the client-provided text snapshot.
    let mut quotes: Vec<Value> = Vec::new();
    if let Some(qs) = quotes_raw {
        let parsed: Value = serde_json::from_str(qs).map_err(|e| format!("quotes must be a JSON array: {e}"))?;
        let arr = parsed.as_array().ok_or_else(|| "quotes must be a JSON array".to_string())?;
        if arr.len() > MAX_QUOTES {
            return Err(format!("too many quotes (max {MAX_QUOTES})"));
        }
        for q in arr {
            let kind = q.get("kind").and_then(|x| x.as_str()).unwrap_or("message");
            let mut text = q.get("text").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let refv = q.get("ref").and_then(|x| x.as_str()).unwrap_or("").to_string();
            if kind == "message" && parse_message_ref(&refv).is_some() {
                let snap = snapshot_message(conn, &team_id, &refv)?;
                text = snap["body"].as_str().unwrap_or("").to_string();
            }
            if text.chars().count() > MAX_QUOTE_TEXT {
                text = text.chars().take(MAX_QUOTE_TEXT).collect();
            }
            quotes.push(json!({
                "kind": kind,
                "ref": refv,
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

    /// D-06: quotes with a message ref get their snapshot re-generated by the
    /// server from the ledger (a forged client text is replaced).
    #[test]
    fn discussion_quote_snapshot_regenerated_by_server() {
        let mut conn = test_conn();
        seed(&conn);
        let did = create(&mut conn, "Q", None, None, "s-owner", Some("t1")).unwrap()["discussion_id"]
            .as_str()
            .unwrap()
            .to_string();
        let m = say_or_reply(&mut conn, &did, "真实原文", None, None, "s-owner", Some("t1")).unwrap();
        let msg1 = m["message_id"].as_str().unwrap().to_string();

        // Client claims a forged text; server must replace it with the real one.
        let q = json!([{ "kind": "message", "ref": msg1, "text": "forged!!!" }]);
        say_or_reply(&mut conn, &did, "引用", Some(&q.to_string()), None, "s-owner", Some("t1")).unwrap();
        let o = open(&conn, &did, "s-owner", Some("t1")).unwrap();
        // open() returns messages newest-first; the quote message is the newest.
        let first = o["messages"].as_array().unwrap().first().unwrap();
        let quotes = first["quotes"].as_array().unwrap();
        assert_eq!(quotes[0]["text"], "真实原文");
    }

    /// D-07: quoting/replying to a message from another team is rejected.
    #[test]
    fn discussion_cross_team_reference_rejected() {
        let mut conn = test_conn();
        seed(&conn);
        // second team + its own discussion/message (distinct member id/session)
        conn.execute(
            "INSERT INTO teams (id, name, owner_member_id, goal_id, state, invite_token, created_at, updated_at)
             VALUES ('t2','T2','m-owner2',NULL,'active','tok','now','now')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO members (id, team_id, session_key, display_name, role, state, is_lead, joined_at)
             VALUES ('m-owner2','t2','s-owner2','Owner2','owner','active',1,'now')",
            [],
        )
        .unwrap();
        let did2 = create(&mut conn, "InT2", None, None, "s-owner2", Some("t2")).unwrap()["discussion_id"]
            .as_str()
            .unwrap()
            .to_string();
        let m2 = say_or_reply(&mut conn, &did2, "t2 消息", None, None, "s-owner2", Some("t2")).unwrap();
        let m2id = m2["message_id"].as_str().unwrap().to_string();

        // From t1, replying to a t2 message must fail (snapshot_message scopes by team).
        let did1 = create(&mut conn, "InT1", None, None, "s-owner", Some("t1")).unwrap()["discussion_id"]
            .as_str()
            .unwrap()
            .to_string();
        let r = say_or_reply(&mut conn, &did1, "reply cross team", None, Some(&m2id), "s-owner", Some("t1"));
        assert!(r.is_err());
    }

    /// D-08: body length and quote-count caps are enforced.
    #[test]
    fn discussion_length_caps_enforced() {
        let mut conn = test_conn();
        seed(&conn);
        let did = create(&mut conn, "Len", None, None, "s-owner", Some("t1")).unwrap()["discussion_id"]
            .as_str()
            .unwrap()
            .to_string();
        // oversize body
        let big = "x".repeat(MAX_BODY + 10);
        assert!(say_or_reply(&mut conn, &did, &big, None, None, "s-owner", Some("t1")).is_err());
        // too many quotes
        let many: Vec<Value> = (0..MAX_QUOTES + 1).map(|_| json!({"kind":"other","ref":"","text":"t"})).collect();
        let qs = json!(many).to_string();
        assert!(say_or_reply(&mut conn, &did, "hi", Some(&qs), None, "s-owner", Some("t1")).is_err());
    }

    // --- Quota (per-topic participation) tests ---

    fn set_quota(conn: &Connection, member_id: &str, n: i64) {
        conn.execute(
            "UPDATE members SET discussion_quota = ?1 WHERE id = ?2 AND team_id='t1'",
            params![n, member_id],
        )
        .unwrap();
    }

    #[test]
    fn mention_match_respects_word_boundary() {
        assert!(body_mentions("请 @Worker 复核", "Worker"));
        assert!(body_mentions("hi @worker", "Worker"));
        assert!(!body_mentions("见 @Worker2 处理", "Worker"));
        assert!(!body_mentions("nothing here", "Worker"));
    }

    #[test]
    fn quota_blocks_active_posts_after_limit() {
        let mut conn = test_conn();
        seed(&conn);
        set_quota(&conn, "m-worker", 2);
        let did = create(&mut conn, "Q", None, None, "s-owner", Some("t1")).unwrap()["discussion_id"]
            .as_str()
            .unwrap()
            .to_string();
        // worker makes 2 active posts (ok), third is blocked
        assert!(say_or_reply(&mut conn, &did, "a", None, None, "s-worker", Some("t1")).is_ok());
        assert!(say_or_reply(&mut conn, &did, "b", None, None, "s-worker", Some("t1")).is_ok());
        let r = say_or_reply(&mut conn, &did, "c", None, None, "s-worker", Some("t1"));
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("quota reached"));
    }

    #[test]
    fn quota_is_per_topic_independent() {
        let mut conn = test_conn();
        seed(&conn);
        set_quota(&conn, "m-worker", 1);
        let d1 = create(&mut conn, "T1", None, None, "s-owner", Some("t1")).unwrap()["discussion_id"]
            .as_str()
            .unwrap()
            .to_string();
        let d2 = create(&mut conn, "T2", None, None, "s-owner", Some("t1")).unwrap()["discussion_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(say_or_reply(&mut conn, &d1, "a", None, None, "s-worker", Some("t1")).is_ok());
        // T1 now at limit: blocked
        assert!(say_or_reply(&mut conn, &d1, "b", None, None, "s-worker", Some("t1")).is_err());
        // T2 still fresh: allowed
        assert!(say_or_reply(&mut conn, &d2, "x", None, None, "s-worker", Some("t1")).is_ok());
    }

    #[test]
    fn mention_reply_does_not_consume_quota_and_bypasses_limit() {
        let mut conn = test_conn();
        seed(&conn);
        set_quota(&conn, "m-worker", 1);
        let did = create(&mut conn, "M", None, None, "s-owner", Some("t1")).unwrap()["discussion_id"]
            .as_str()
            .unwrap()
            .to_string();
        // owner posts a message mentioning @Worker
        let ow = say_or_reply(&mut conn, &did, "@Worker 请看一下", None, None, "s-owner", Some("t1")).unwrap();
        let ow_id = ow["message_id"].as_str().unwrap().to_string();
        // worker consumes its 1 active post
        assert!(say_or_reply(&mut conn, &did, "active", None, None, "s-worker", Some("t1")).is_ok());
        // worker's active is now 1/1 -> plain say blocked
        assert!(say_or_reply(&mut conn, &did, "extra", None, None, "s-worker", Some("t1")).is_err());
        // but replying to the @ mention is allowed (no quota)
        assert!(say_or_reply(&mut conn, &did, "ok on it", None, Some(&ow_id), "s-worker", Some("t1")).is_ok());
        // and it did NOT count: a second plain say is still blocked (still 1/1)
        assert!(say_or_reply(&mut conn, &did, "extra2", None, None, "s-worker", Some("t1")).is_err());
    }

    #[test]
    fn raising_quota_resumes_posting() {
        let mut conn = test_conn();
        seed(&conn);
        set_quota(&conn, "m-worker", 1);
        let did = create(&mut conn, "R", None, None, "s-owner", Some("t1")).unwrap()["discussion_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(say_or_reply(&mut conn, &did, "a", None, None, "s-worker", Some("t1")).is_ok());
        assert!(say_or_reply(&mut conn, &did, "b", None, None, "s-worker", Some("t1")).is_err());
        set_quota(&conn, "m-worker", 3);
        assert!(say_or_reply(&mut conn, &did, "b", None, None, "s-worker", Some("t1")).is_ok());
    }
}
