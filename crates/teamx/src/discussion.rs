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
/// 头脑风暴方向交接事件（design/007 P1b）。
pub const EV_HANDED_OFF: &str = "brainstorm.handed_off";

/// brainstorm 发散窗口内允许的消息类型（design/007 P0b）。
pub const BRAINSTORM_KINDS: &[&str] = &["idea", "concern", "question", "reference"];
/// `directions[]` 中禁止出现的判定类字段（design/007 P1a 拒绝清单）。
pub const FORBIDDEN_DIRECTION_FIELDS: &[&str] = &["score", "rank", "priority"];
/// 默认发散窗口（秒）与每成员 idea 下限。
pub const DEFAULT_WINDOW_SECS: i64 = 86_400;
pub const DEFAULT_MIN_IDEAS: i64 = 3;

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

/// Read the member's per-topic discussion quota (schema default 20).
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

// --- Discussion subscription (G5: wake target for inbox) ---

/// Subscribe a member to a discussion's new messages.
pub fn subscribe(conn: &Connection, team_id: &str, member_id: &str, discussion_id: &str) -> Result<(), String> {
    // The discussion must exist in this team.
    let exists = events::list(conn, team_id, None)
        .map_err(|e| e.to_string())?
        .iter()
        .any(|e| e.r#type == EV_CREATED && e.payload.as_ref().and_then(|p| p.get("discussion_id")).and_then(|v| v.as_str()) == Some(discussion_id));
    if !exists {
        return Err(format!("discussion `{discussion_id}` not found in this team"));
    }
    conn.execute(
        "INSERT OR IGNORE INTO discussion_subs (team_id, member_id, discussion_id, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![team_id, member_id, discussion_id, events::db_now()],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Unsubscribe a member from a discussion.
pub fn unsubscribe(conn: &Connection, team_id: &str, member_id: &str, discussion_id: &str) -> Result<(), String> {
    conn.execute(
        "DELETE FROM discussion_subs WHERE team_id=?1 AND member_id=?2 AND discussion_id=?3",
        params![team_id, member_id, discussion_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Is the member subscribed to the discussion?
pub fn is_subscribed(conn: &Connection, team_id: &str, member_id: &str, discussion_id: &str) -> Result<bool, String> {
    conn.query_row(
        "SELECT COUNT(*) FROM discussion_subs WHERE team_id=?1 AND member_id=?2 AND discussion_id=?3",
        params![team_id, member_id, discussion_id],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
    .map_err(|e| e.to_string())
}

/// Resolve the `task_id` attached to a discussion (from its `discussion.created` event).
fn discussion_task_id(conn: &Connection, team_id: &str, discussion_id: &str) -> Option<String> {
    events::list(conn, team_id, None)
        .ok()?
        .into_iter()
        .find(|e| {
            e.r#type == EV_CREATED
                && e.payload.as_ref().and_then(|p| p.get("discussion_id")).and_then(|v| v.as_str()) == Some(discussion_id)
        })
        .and_then(|e| e.payload)
        .and_then(|p| p.get("task_id").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .filter(|s| !s.is_empty())
}

/// Collect discussions with NEW activity that should wake `member_id`:
/// triggered when a message (seq > cursor) @mentions the member, OR the member
/// subscribed to the discussion, OR the discussion is linked to a task the
/// member is assigned to. Returns newest-first message objects with enough
/// context for a wake prompt.
pub fn wake_discussions(conn: &Connection, team_id: &str, member_id: &str, display_name: &str, cursor: i64) -> Result<Vec<Value>, String> {
    let evs = events::list(conn, team_id, None).map_err(|e| e.to_string())?;
    let mut out: Vec<Value> = Vec::new();
    for e in &evs {
        if e.r#type != EV_MESSAGE || e.seq <= cursor {
            continue;
        }
        let Some(p) = &e.payload else { continue };
        let did = p.get("discussion_id").and_then(|v| v.as_str()).unwrap_or("");
        if did.is_empty() {
            continue;
        }
        let body = p.get("body").and_then(|v| v.as_str()).unwrap_or("");
        let author = p.get("author").and_then(|v| v.as_str()).unwrap_or("");
        // Skip the member's own messages (no need to wake yourself).
        if e.member_id.as_deref() == Some(member_id) {
            continue;
        }
        // Trigger 1: @mention of this member in the message body.
        let mentioned = body_mentions(body, display_name);
        // Trigger 2: member subscribed to this discussion.
        let subbed = is_subscribed(conn, team_id, member_id, did)?;
        // Trigger 3: discussion linked to a task assigned to this member.
        let linked_to_my_task = match discussion_task_id(conn, team_id, did) {
            Some(tid) => {
                let mine: bool = conn
                    .query_row(
                        "SELECT COUNT(*) FROM taskx WHERE team_id=?1 AND id=?2 AND assignee_member_id=?3",
                        params![team_id, tid, member_id],
                        |r| r.get::<_, i64>(0),
                    )
                    .unwrap_or(0)
                    > 0;
                mine
            }
            None => false,
        };
        // design/007 P1c 唤醒纪律：brainstorm 线程内**普通 idea 不唤醒**——
        // 仅显式 @ 提及可唤醒（订阅/关联任务在该模式下被抑制），
        // 防头脑风暴放大智能体回合数。
        let brainstorm_thread = is_brainstorm(created_payload(&evs, did));
        if brainstorm_thread {
            if !mentioned {
                continue;
            }
        } else if !mentioned && !subbed && !linked_to_my_task {
            continue;
        }
        let title = events::list(conn, team_id, None)
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|ce| {
                ce.r#type == EV_CREATED
                    && ce.payload.as_ref().and_then(|q| q.get("discussion_id")).and_then(|v| v.as_str()) == Some(did)
            })
            .and_then(|ce| ce.payload)
            .and_then(|q| q.get("title").and_then(|v| v.as_str()).map(|s| s.to_string()))
            .unwrap_or_else(|| did.to_string());
        out.push(json!({
            "discussion_id": did,
            "title": title,
            "message_id": p.get("message_id").cloned().unwrap_or_else(|| json!("")),
            "author": author,
            "body": body,
            "seq": e.seq,
            "trigger": {
                "mentioned": mentioned,
                "subscribed": subbed,
                "my_task": linked_to_my_task,
            },
            "created_at": e.created_at,
        }));
    }
    out.sort_by(|a, b| b["seq"].as_i64().unwrap_or(0).cmp(&a["seq"].as_i64().unwrap_or(0)));
    out.truncate(10);
    Ok(out)
}

/// 该事件是否属于某个 discussion。
fn same_disc(e: &events::Event, id: &str) -> bool {
    e.payload
        .as_ref()
        .and_then(|p| p.get("discussion_id"))
        .and_then(|x| x.as_str())
        == Some(id)
}

/// 取 `discussion.created` 的 payload。
fn created_payload<'a>(evs: &'a [events::Event], id: &str) -> Option<&'a Value> {
    evs.iter()
        .find(|e| e.r#type == EV_CREATED && same_disc(e, id))
        .and_then(|e| e.payload.as_ref())
}

/// 是否 brainstorm 模式（design/007 P0a：mode 是**配置**不是状态）。
pub fn is_brainstorm(created: Option<&Value>) -> bool {
    created.and_then(|p| p.get("mode")).and_then(|x| x.as_str()) == Some("brainstorm")
}

fn window_secs_of(created: Option<&Value>) -> i64 {
    created
        .and_then(|p| p.get("window_secs"))
        .and_then(Value::as_i64)
        .unwrap_or(DEFAULT_WINDOW_SECS)
}

fn min_ideas_of(created: Option<&Value>) -> i64 {
    created
        .and_then(|p| p.get("min_ideas_per_member"))
        .and_then(Value::as_i64)
        .unwrap_or(DEFAULT_MIN_IDEAS)
}

/// 发散窗口是否已过（design/007 P0a：phase **派生**，不落库）。
pub fn window_expired(created: Option<&Value>, created_at: &str, now: &str) -> bool {
    let secs = window_secs_of(created);
    match (
        chrono::DateTime::parse_from_rfc3339(created_at),
        chrono::DateTime::parse_from_rfc3339(now),
    ) {
        (Ok(c), Ok(n)) => (n - c).num_seconds() >= secs,
        _ => false,
    }
}

/// 派生 phase：`harvested`（已收窗）/ `clusterable`（窗口已过，待收窗）/ `diverging`。
pub fn brainstorm_phase(evs: &[events::Event], id: &str, now: &str) -> String {
    if evs.iter().any(|e| e.r#type == EV_CLOSED && same_disc(e, id)) {
        return "harvested".to_string();
    }
    let created_ev = evs.iter().find(|e| e.r#type == EV_CREATED && same_disc(e, id));
    match created_ev {
        Some(c) => {
            if window_expired(c.payload.as_ref(), &c.created_at, now) {
                "clusterable".to_string()
            } else {
                "diverging".to_string()
            }
        }
        None => "diverging".to_string(),
    }
}

/// 校验 `directions[]`（design/007 P1a）：JSON 数组，每项必填 `source_message_ids[]` 与
/// `next_step`，且**禁止** score/rank/priority。
pub fn validate_directions(raw: &str) -> Result<Vec<Value>, String> {
    let parsed: Value = serde_json::from_str(raw).map_err(|e| format!("directions must be a JSON array: {e}"))?;
    let arr = parsed.as_array().ok_or_else(|| "directions must be a JSON array".to_string())?;
    let mut out = Vec::new();
    for d in arr {
        let obj = d.as_object().ok_or_else(|| "each direction must be an object".to_string())?;
        for f in FORBIDDEN_DIRECTION_FIELDS {
            if obj.contains_key(*f) {
                return Err(format!("directions must not carry `{f}` (no premature judgement)"));
            }
        }
        let src = obj.get("source_message_ids").and_then(Value::as_array).cloned().unwrap_or_default();
        if src.is_empty() {
            return Err("each direction requires a non-empty `source_message_ids[]`".to_string());
        }
        let next = obj.get("next_step").and_then(Value::as_str).unwrap_or("").trim();
        if next.is_empty() {
            return Err("each direction requires `next_step`".to_string());
        }
        out.push(d.clone());
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn create(
    conn: &mut Connection,
    title: &str,
    body: Option<&str>,
    task: Option<&str>,
    session: &str,
    team_opt: Option<&str>,
    mode: Option<&str>,
    window_secs: Option<i64>,
    min_ideas: Option<i64>,
    kind: Option<&str>,
) -> Result<Value, String> {
    let (member_id, display_name, team_id) = member_id_for_session(conn, session, team_opt)?;
    if title.trim().is_empty() {
        return Err("discussion title is required".to_string());
    }
    let discussion_id = format!("dsc_{}", uuid_slug());
    let mut payload = json!({
        "discussion_id": discussion_id,
        "title": title.trim(),
        "body": body.unwrap_or("").to_string(),
        "author": display_name,
        "task_id": task.unwrap_or(""),
    });
    // design/007 P0a：mode 是**配置**不是状态（纯加法，零迁移）。
    if let Some(m) = mode.map(str::trim).filter(|m| !m.is_empty()) {
        if m != "brainstorm" {
            return Err(format!("unknown discussion mode `{m}` (only `brainstorm`)"));
        }
        payload["mode"] = json!("brainstorm");
        payload["window_secs"] = json!(window_secs.unwrap_or(DEFAULT_WINDOW_SECS));
        payload["min_ideas_per_member"] = json!(min_ideas.unwrap_or(DEFAULT_MIN_IDEAS));
    }
    // 讨论**类型**（kind）：normal（默认，不落字段）| brainstorm | grill。
    // `--kind brainstorm` 等价于 `--mode brainstorm`（便捷）；grill 是
    // dsh-plugin 的 teamx-grill-with-docs 协议（Consensus Mode）标记。
    let mut kind_v = kind.map(str::trim).filter(|k| !k.is_empty()).unwrap_or("");
    if kind_v == "brainstorm" && payload.get("mode").is_none() {
        payload["mode"] = json!("brainstorm");
        payload["window_secs"] = json!(window_secs.unwrap_or(DEFAULT_WINDOW_SECS));
        payload["min_ideas_per_member"] = json!(min_ideas.unwrap_or(DEFAULT_MIN_IDEAS));
    }
    if payload.get("mode").is_some() && kind_v.is_empty() {
        kind_v = "brainstorm"; // 老调用方只传 mode，类型自动补齐
    }
    match kind_v {
        "" | "normal" => {}
        "brainstorm" | "grill" => payload["kind"] = json!(kind_v),
        other => return Err(format!("unknown discussion kind `{other}` (normal|brainstorm|grill)")),
    }
    let seq = db::with_write(conn, |tx| events::emit(tx, &team_id, Some(&member_id), EV_CREATED, Some(&payload)))
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "ok": true,
        "discussion_id": discussion_id,
        "created_seq": seq,
        "team_id": team_id,
        "title": title.trim(),
        "mode": payload.get("mode"),
        "kind": discussion_kind(payload.get("mode"), payload.get("kind")),
    }))
}

/// 讨论类型：显式 `kind` 优先，其次由 brainstorm mode 推导，否则 `normal`。
pub fn discussion_kind(mode: Option<&Value>, kind: Option<&Value>) -> &'static str {
    match kind.and_then(Value::as_str) {
        Some("grill") => "grill",
        Some("brainstorm") => "brainstorm",
        Some("normal") => "normal",
        _ => {
            if mode.and_then(Value::as_str) == Some("brainstorm") {
                "brainstorm"
            } else {
                "normal"
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn say_or_reply(
    conn: &mut Connection,
    discussion_id: &str,
    body: &str,
    quotes_raw: Option<&str>,
    reply_to: Option<&str>,
    session: &str,
    team_opt: Option<&str>,
    kind: Option<&str>,
) -> Result<Value, String> {
    let (member_id, display_name, team_id) = member_id_for_session(conn, session, team_opt)?;
    if body.trim().is_empty() {
        return Err("message body is required".to_string());
    }
    if body.chars().count() > MAX_BODY {
        return Err(format!("message body too long (max {MAX_BODY} chars)"));
    }
    // Verify the discussion exists and is open.
    let evs = events::list(conn, &team_id, None).map_err(|e| e.to_string())?;
    let created_ev = evs
        .iter()
        .find(|e| e.r#type == EV_CREATED && same_disc(e, discussion_id));
    if created_ev.is_none() {
        return Err(format!("discussion `{discussion_id}` not found in this team"));
    }
    if evs.iter().any(|e| e.r#type == EV_CLOSED && same_disc(e, discussion_id)) {
        return Err(format!("discussion `{discussion_id}` is closed"));
    }

    // design/007 P0b：发散窗口内消息必填 kind；concern 必须挂 idea。
    let created_p = created_ev.and_then(|e| e.payload.as_ref());
    let mut kind_norm: Option<String> = None;
    if is_brainstorm(created_p) {
        let now = db::now();
        let expired = created_ev
            .map(|c| window_expired(c.payload.as_ref(), &c.created_at, &now))
            .unwrap_or(false);
        if !expired {
            let k = kind.map(str::trim).unwrap_or("");
            if !BRAINSTORM_KINDS.contains(&k) {
                return Err(format!(
                    "brainstorm diverging window requires `kind` in {{idea,concern,question,reference}} (got `{k}`)"
                ));
            }
            if k == "concern" && reply_to.is_none() {
                return Err("brainstorm `concern` must reply_to an idea".to_string());
            }
            kind_norm = Some(k.to_string());
        }
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
        let mut payload = json!({
            "discussion_id": discussion_id,
            "message_id": format!("{discussion_id}#{next_seq}"),
            "author": display_name,
            "body": body.trim(),
            "reply_to": reply_obj,
            "quotes": quotes,
        });
        if let Some(k) = &kind_norm {
            payload["kind"] = json!(k);
        }
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
    list_for_team(conn, &team_id, all)
}

/// Team-scoped discussion listing — the shared engine behind the CLI
/// `discussion list` path and the IM read-only query path (`im.query`),
/// which has a resolved member identity instead of a session key.
pub fn list_for_team(conn: &Connection, team_id: &str, all: bool) -> Result<Value, String> {
    let evs = events::list(conn, team_id, None).map_err(|e| e.to_string())?;
    let numbers = discussion_numbers(&evs);
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
                // design/007：列表暴露 mode / phase（brainstorm 可见「待收窗」）。
                if let Some(m) = payload.get("mode").and_then(|x| x.as_str()) {
                    entry["mode"] = json!(m);
                }
                // 讨论类型（normal/brainstorm/grill）——console/CLI 用它显示徽标。
                entry["kind"] = json!(discussion_kind(payload.get("mode"), payload.get("kind")));
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
    let now = db::now();
    let mut out: Vec<Value> = threads.into_iter().map(|(did, mut t)| {
        t["number"] = json!(numbers.get(&did).copied().unwrap_or(0));
        // design/007 P0a：brainstorm 的 phase 由事件派生（diverging / clusterable / harvested）。
        let created_p = created_payload(&evs, &did);
        if is_brainstorm(created_p) {
            t["phase"] = json!(brainstorm_phase(&evs, &did, &now));
        }
        t
    }).collect();
    if !all {
        out.retain(|t| t["closed"].as_bool() != Some(true));
    }
    out.sort_by(|a, b| b["created_at"].as_str().unwrap_or("").cmp(a["created_at"].as_str().unwrap_or("")));
    Ok(json!({ "ok": true, "discussions": out }))
}

/// 待收窗的 brainstorm 线程（design/007 P1c：窗口到期 / budget.alert 时提醒 owner/lead
/// **提前收窗**，而非自动收窗——自动收窗 = agent 自行收敛，违反定稿）。
pub fn brainstorm_needs_harvest(conn: &Connection, team_id: &str) -> Result<Vec<Value>, String> {
    let evs = events::list(conn, team_id, None).map_err(|e| e.to_string())?;
    let now = db::now();
    let mut out = Vec::new();
    for (did, _) in discussion_numbers(&evs) {
        let created_p = created_payload(&evs, &did);
        if !is_brainstorm(created_p) {
            continue;
        }
        let phase = brainstorm_phase(&evs, &did, &now);
        if phase == "clusterable" {
            out.push(json!({ "discussion_id": did, "phase": phase }));
        }
    }
    Ok(out)
}

/// Per-team sequential discussion numbers (`#1`, `#2`, …) derived from the
/// creation order of `discussion.created` events — no schema change, and it
/// works for every historical thread. Used by `list` / `open` to expose a
/// short human-friendly `number` alongside the opaque `discussion_id`.
fn discussion_numbers(evs: &[events::Event]) -> std::collections::HashMap<String, i64> {
    let mut map = std::collections::HashMap::new();
    let mut n: i64 = 0;
    for e in evs {
        if e.r#type != EV_CREATED {
            continue;
        }
        if let Some(did) = e.payload.as_ref().and_then(|p| p.get("discussion_id")).and_then(|x| x.as_str()) {
            n += 1;
            map.insert(did.to_string(), n);
        }
    }
    map
}

fn open(conn: &Connection, id: &str, session: &str, team_opt: Option<&str>) -> Result<Value, String> {
    let (_, _, team_id) = member_id_for_session(conn, session, team_opt)?;
    let evs = events::list(conn, &team_id, None).map_err(|e| e.to_string())?;
    let number = discussion_numbers(&evs).get(id).copied().unwrap_or(0);
    let mut title = String::new();
    let mut closed = false;
    let mut closed_by = String::new();
    let mut closed_at = String::new();
    let mut summary: Option<String> = None;
    let mut summary_auto = false;
    let mut directions: Vec<Value> = Vec::new();
    let mut handed_off: Vec<Value> = Vec::new();
    for e in &evs {
        let Some(payload) = &e.payload else { continue };
        if payload.get("discussion_id").and_then(|x| x.as_str()) != Some(id) {
            continue;
        }
        match e.r#type.as_str() {
            EV_CREATED => {
                title = payload.get("title").and_then(|x| x.as_str()).unwrap_or("").to_string();
            }
            EV_CLOSED => {
                closed = true;
                closed_by = payload.get("closed_by").and_then(|x| x.as_str()).unwrap_or("").to_string();
                closed_at = e.created_at.clone();
                if let Some(s) = payload.get("summary").and_then(|x| x.as_str()) {
                    if !s.trim().is_empty() {
                        summary = Some(s.to_string());
                    }
                }
                if let Some(b) = payload.get("summary_auto").and_then(|x| x.as_bool()) {
                    summary_auto = b;
                }
                if let Some(ds) = payload.get("directions").and_then(Value::as_array) {
                    directions = ds.clone();
                }
            }
            EV_HANDED_OFF => {
                handed_off.push(json!({
                    "direction_id": payload.get("direction_id"),
                    "target_kind": payload.get("target_kind"),
                    "target_id": payload.get("target_id"),
                    "by": payload.get("by"),
                    "at": e.created_at,
                }));
            }
            _ => {}
        }
    }
    let mut messages = collect_messages(&evs, id);
    if title.is_empty() && messages.is_empty() && !closed {
        return Err(format!("discussion `{id}` not found"));
    }
    // Threads closed before summaries existed (or with an empty one) get a
    // deterministic digest so members always see a quick recap under the topic.
    // design/007 P1a：brainstorm **恒不生成** auto digest（按 mode 分叉）。
    let created_p = created_payload(&evs, id);
    let brainstorm = is_brainstorm(created_p);
    if closed
        && !brainstorm
        && summary.as_deref().map(|s| s.trim().is_empty()).unwrap_or(true)
    {
        summary = Some(digest_summary(&messages));
        summary_auto = true;
    }
    messages.reverse(); // newest first
    let now = db::now();
    let phase = if brainstorm {
        Some(brainstorm_phase(&evs, id, &now))
    } else {
        None
    };
    let window_exp = created_p
        .and_then(|_| evs.iter().find(|e| e.r#type == EV_CREATED && same_disc(e, id)))
        .map(|c| window_expired(c.payload.as_ref(), &c.created_at, &now));
    Ok(json!({
        "ok": true,
        "discussion_id": id,
        "number": number,
        "title": title,
        "mode": created_p.and_then(|p| p.get("mode")).cloned().unwrap_or(Value::Null),
        "kind": discussion_kind(
            created_p.and_then(|p| p.get("mode")),
            created_p.and_then(|p| p.get("kind")),
        ),
        "phase": phase,
        "window_expired": window_exp,
        "window_secs": created_p.map(|p| window_secs_of(Some(p))),
        "min_ideas_per_member": created_p.map(|p| min_ideas_of(Some(p))),
        "closed": closed,
        "closed_by": closed_by,
        "closed_at": closed_at,
        "summary": summary,
        "summary_auto": summary_auto,
        "directions": directions,
        "handed_off": handed_off,
        "messages": messages,
    }))
}

/// Collect a thread's messages in chronological order (as stored).
fn collect_messages(evs: &[events::Event], id: &str) -> Vec<Value> {
    let mut messages = Vec::new();
    for e in evs {
        let Some(payload) = &e.payload else { continue };
        if payload.get("discussion_id").and_then(|x| x.as_str()) != Some(id) {
            continue;
        }
        if e.r#type != EV_MESSAGE {
            continue;
        }
        let mid = payload
            .get("message_id")
            .and_then(|x| x.as_str())
            .unwrap_or(&format!("{id}#{}", e.seq))
            .to_string();
        messages.push(json!({
            "seq": e.seq,
            "message_id": mid,
            "author": payload.get("author").and_then(|x| x.as_str()).unwrap_or(""),
            "body": payload.get("body").and_then(|x| x.as_str()).unwrap_or(""),
            "reply_to": payload.get("reply_to"),
            "quotes": payload.get("quotes").cloned().unwrap_or_else(|| json!([])),
            "kind": payload.get("kind").cloned().unwrap_or(Value::Null),
            "created_at": e.created_at,
        }));
    }
    messages
}

/// Truncate a string to at most `n` chars (UTF-8 safe), appending `…` if cut.
fn truncate_chars(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n).collect();
    out.push('…');
    out
}

/// Short timestamp for the digest header (`YYYY-MM-DD HH:MM` from RFC3339).
fn short_ts(s: &str) -> String {
    let t = s.get(0..16).unwrap_or(s);
    t.replace('T', " ")
}

/// Deterministic fallback summary built from a thread's messages (chronological
/// order). Used when the closer supplies no summary, and to backfill threads
/// that were closed before summaries existed. Not a semantic summary — a
/// compact digest: participants, size, time range and per-message key lines.
fn digest_summary(msgs: &[Value]) -> String {
    if msgs.is_empty() {
        return "（自动摘要）该讨论没有消息。".to_string();
    }
    let mut who: Vec<String> = Vec::new();
    for m in msgs {
        let a = m.get("author").and_then(|x| x.as_str()).unwrap_or("").trim();
        if !a.is_empty() && !who.iter().any(|w| w == a) {
            who.push(a.to_string());
        }
    }
    let first = msgs.first().and_then(|m| m.get("created_at")).and_then(|x| x.as_str()).unwrap_or("");
    let last = msgs.last().and_then(|m| m.get("created_at")).and_then(|x| x.as_str()).unwrap_or("");
    let mut out = format!(
        "**参与者**：{}（{} 人）· **消息**：{} 条 · **时间**：{} → {}",
        if who.is_empty() { "—".to_string() } else { who.join("、") },
        who.len(),
        msgs.len(),
        short_ts(first),
        short_ts(last),
    );
    const CAP: usize = 15;
    out.push_str("\n\n**要点（按时间）**\n");
    for m in msgs.iter().take(CAP) {
        let a = m.get("author").and_then(|x| x.as_str()).unwrap_or("");
        let body = m.get("body").and_then(|x| x.as_str()).unwrap_or("").replace(['\r', '\n'], " ");
        out.push_str(&format!("- {}：{}\n", a, truncate_chars(body.trim(), 160)));
    }
    if msgs.len() > CAP {
        out.push_str(&format!("- …（共 {} 条，仅摘录前 {} 条）\n", msgs.len(), CAP));
    }
    out.trim_end().to_string()
}

fn close(conn: &mut Connection, id: &str, summary: Option<&str>, directions: Option<&str>, session: &str, team_opt: Option<&str>) -> Result<Value, String> {
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
    // design/007 P1a/P0b：brainstorm 收窗需要 directions[]，并校验每成员 idea 下限
    // （owner 可豁免）；且**恒不生成** auto digest。
    let created_p = created_payload(&evs, id);
    let brainstorm = is_brainstorm(created_p);
    let mut directions_val: Option<Vec<Value>> = None;
    if brainstorm {
        let is_owner: bool = conn
            .query_row(
                "SELECT owner_member_id = ?1 FROM teams WHERE id = ?2",
                params![member_id, team_id],
                |r| r.get(0),
            )
            .unwrap_or(false);
        let msgs0 = collect_messages(&evs, id);
        if !is_owner {
            let min = min_ideas_of(created_p);
            let mut ideas: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
            let mut authors: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            for m in &msgs0 {
                let a = m.get("author").and_then(|x| x.as_str()).unwrap_or("").to_string();
                if a.is_empty() {
                    continue;
                }
                authors.insert(a.clone());
                if m.get("kind").and_then(|x| x.as_str()) == Some("idea") {
                    *ideas.entry(a).or_insert(0) += 1;
                }
            }
            let short: Vec<String> = authors
                .iter()
                .filter(|a| ideas.get(*a).copied().unwrap_or(0) < min)
                .cloned()
                .collect();
            if !short.is_empty() {
                return Err(format!(
                    "brainstorm close requires >= {min} `kind=idea` per participant; below: {}",
                    short.join(", ")
                ));
            }
        }
        let raw = directions
            .ok_or_else(|| "brainstorm close requires `directions` (JSON array)".to_string())?;
        directions_val = Some(validate_directions(raw)?);
    }
    // A closing summary is what members read under the topic. Prefer the one
    // supplied by the closer (an agent can summarize content + conclusions);
    // otherwise store a deterministic digest so the topic is never bare
    // (brainstorm threads deliberately get NO auto digest).
    let msgs = collect_messages(&evs, id);
    let (summary_text, summary_auto) = if brainstorm {
        (
            summary.map(str::trim).filter(|s| !s.is_empty()).unwrap_or("").to_string(),
            false,
        )
    } else {
        match summary.map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => (s.to_string(), false),
            None => (digest_summary(&msgs), true),
        }
    };
    let mut payload = json!({
        "discussion_id": id,
        "closed_by": display_name,
        "summary": summary_text,
        "summary_auto": summary_auto,
    });
    if let Some(d) = &directions_val {
        payload["directions"] = json!(d);
    }
    let seq = db::with_write(conn, |tx| events::emit(tx, &team_id, Some(&member_id), EV_CLOSED, Some(&payload)))
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "ok": true,
        "discussion_id": id,
        "closed_seq": seq,
        "summary": summary_text,
        "summary_auto": summary_auto,
        "directions": directions_val,
    }))
}

/// 方向交接（design/007 P1b）：仅 owner/lead；方向级 1:N 单向只读；原帖 harvested 不可变。
#[allow(clippy::too_many_arguments)]
fn handoff(
    conn: &mut Connection,
    id: &str,
    direction_id: &str,
    target_kind: &str,
    target_id: &str,
    session: &str,
    team_opt: Option<&str>,
) -> Result<Value, String> {
    let (member_id, display_name, team_id) = member_id_for_session(conn, session, team_opt)?;
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
        return Err("discussion.handoff: only the team owner or a co-lead may hand off a direction".to_string());
    }
    let evs = events::list(conn, &team_id, None).map_err(|e| e.to_string())?;
    let created_p = created_payload(&evs, id).ok_or_else(|| format!("discussion `{id}` not found"))?;
    if !is_brainstorm(Some(created_p)) {
        return Err("discussion.handoff requires a brainstorm discussion".to_string());
    }
    let closed_ev = evs
        .iter()
        .find(|e| e.r#type == EV_CLOSED && same_disc(e, id))
        .ok_or_else(|| "discussion.handoff requires the thread to be harvested (closed)".to_string())?;
    let dirs = closed_ev
        .payload
        .as_ref()
        .and_then(|p| p.get("directions"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let dir_exists = dirs.iter().any(|d| {
        d.get("direction_id").and_then(|x| x.as_str()) == Some(direction_id)
            || d.get("id").and_then(|x| x.as_str()) == Some(direction_id)
    });
    if !dir_exists {
        return Err(format!("direction `{direction_id}` not found in this thread's directions[]"));
    }
    if evs.iter().any(|e| {
        e.r#type == EV_HANDED_OFF
            && same_disc(e, id)
            && e.payload.as_ref().and_then(|p| p.get("direction_id")).and_then(|x| x.as_str())
                == Some(direction_id)
    }) {
        return Err(format!("direction `{direction_id}` was already handed off"));
    }
    // 目标存在性校验（评审建议）。
    match target_kind {
        "discussion" => {
            if !evs.iter().any(|e| e.r#type == EV_CREATED && same_disc(e, target_id)) {
                return Err(format!("target discussion `{target_id}` not found"));
            }
        }
        "task" => {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM taskx WHERE team_id = ?1 AND id = ?2",
                    params![team_id, target_id],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            if n == 0 {
                return Err(format!("target task `{target_id}` not found"));
            }
        }
        other => return Err(format!("unknown target_kind `{other}` (discussion|task)")),
    }
    let payload = json!({
        "discussion_id": id,
        "direction_id": direction_id,
        "target_kind": target_kind,
        "target_id": target_id,
        "by": display_name,
    });
    let seq = db::with_write(conn, |tx| {
        events::emit(tx, &team_id, Some(&member_id), EV_HANDED_OFF, Some(&payload))
    })
    .map_err(|e| e.to_string())?;
    Ok(json!({
        "ok": true,
        "discussion_id": id,
        "direction_id": direction_id,
        "target_kind": target_kind,
        "target_id": target_id,
        "seq": seq,
    }))
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
        DiscussionCmd::Create { title, body, task, mode, window_secs, min_ideas, kind, session, team } => {
            create(
                conn, title, body.as_deref(), task.as_deref(), session, team.as_deref(),
                mode.as_deref(), *window_secs, *min_ideas, kind.as_deref(),
            )
        }
        DiscussionCmd::List { all, session, team } => list(conn, *all, session, team.as_deref()),
        DiscussionCmd::Open { id, session, team } => open(conn, id, session, team.as_deref()),
        DiscussionCmd::Say { id, body, quotes, kind, session, team } => {
            say_or_reply(conn, id, body, quotes.as_deref(), None, session, team.as_deref(), kind.as_deref())
        }
        DiscussionCmd::Reply { id, message_id, body, kind, session, team } => {
            say_or_reply(conn, id, body, None, Some(message_id), session, team.as_deref(), kind.as_deref())
        }
        DiscussionCmd::Close { id, summary, directions, session, team } => {
            close(conn, id, summary.as_deref(), directions.as_deref(), session, team.as_deref())
        }
        DiscussionCmd::Handoff { id, direction_id, target_kind, target_id, session, team } => {
            handoff(conn, id, direction_id, target_kind, target_id, session, team.as_deref())
        }
        DiscussionCmd::Subscribe { id, session, team } => {
            let (member_id, _name, team_id) = member_id_for_session(conn, session, team.as_deref())?;
            subscribe(conn, &team_id, &member_id, id)?;
            Ok(json!({ "ok": true, "discussion_id": id, "subscribed": true }))
        }
        DiscussionCmd::Unsubscribe { id, session, team } => {
            let (member_id, _name, team_id) = member_id_for_session(conn, session, team.as_deref())?;
            unsubscribe(conn, &team_id, &member_id, id)?;
            Ok(json!({ "ok": true, "discussion_id": id, "subscribed": false }))
        }
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
        let r = create(&mut conn, "Review round2", Some("open"), Some("search-r"), "s-owner", Some("t1"), None, None, None, None).unwrap();
        let did = r["discussion_id"].as_str().unwrap().to_string();

        // worker posts a message
        let m = say_or_reply(&mut conn, &did, "DOI ok", None, None, "s-worker", Some("t1"), None).unwrap();
        let msg1 = m["message_id"].as_str().unwrap().to_string();
        assert!(msg1.starts_with(&format!("{did}#")));

        // owner replies to that message (reference)
        let r2 = say_or_reply(&mut conn, &did, "thanks", None, Some(&msg1), "s-owner", Some("t1"), None).unwrap();
        assert!(r2["ok"] == json!(true));

        // quote a message in a third message
        let q = json!([{ "kind": "message", "ref": msg1, "text": "snapshot" }]);
        let r3 = say_or_reply(&mut conn, &did, "quoting", Some(&q.to_string()), None, "s-worker", Some("t1"), None).unwrap();
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
        assert!(close(&mut conn, &did, None, None, "s-worker", Some("t1")).is_err());
        // owner closes without a summary -> deterministic digest is stored
        let c = close(&mut conn, &did, None, None, "s-owner", Some("t1")).unwrap();
        assert!(c["ok"] == json!(true));
        assert_eq!(c["summary_auto"], json!(true));
        assert!(c["summary"].as_str().unwrap().contains("参与者"));
        // open exposes the stored summary + closed metadata
        let oc = open(&conn, &did, "s-owner", Some("t1")).unwrap();
        assert_eq!(oc["closed"], json!(true));
        assert_eq!(oc["summary_auto"], json!(true));
        assert!(oc["summary"].as_str().unwrap().contains("参与者"));
        assert!(!oc["closed_by"].as_str().unwrap().is_empty());
        // after close, saying is rejected
        assert!(say_or_reply(&mut conn, &did, "late", None, None, "s-worker", Some("t1"), None).is_err());
        // list excludes closed by default
        let l2 = list(&conn, false, "s-owner", Some("t1")).unwrap();
        assert_eq!(l2["discussions"].as_array().unwrap().len(), 0);
        let l3 = list(&conn, true, "s-owner", Some("t1")).unwrap();
        assert_eq!(l3["discussions"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn close_with_explicit_summary_is_stored_and_returned() {
        let mut conn = test_conn();
        seed(&conn);
        let did = create(&mut conn, "T", None, None, "s-owner", Some("t1"), None, None, None, None).unwrap()["discussion_id"]
            .as_str()
            .unwrap()
            .to_string();
        say_or_reply(&mut conn, &did, "hi", None, None, "s-worker", Some("t1"), None).unwrap();
        let c = close(&mut conn, &did, Some("## 结论\n采用方案 A"), None, "s-owner", Some("t1")).unwrap();
        assert_eq!(c["summary_auto"], json!(false));
        assert_eq!(c["summary"], json!("## 结论\n采用方案 A"));
        // open returns the closer-supplied summary verbatim (not the digest)
        let oc = open(&conn, &did, "s-owner", Some("t1")).unwrap();
        assert_eq!(oc["summary"], json!("## 结论\n采用方案 A"));
        assert_eq!(oc["summary_auto"], json!(false));
    }

    #[test]
    fn closed_thread_without_stored_summary_gets_digest_on_open() {
        // Simulate a thread closed before summaries existed: emit EV_CLOSED
        // with the legacy payload (no `summary`).
        let mut conn = test_conn();
        seed(&conn);
        let did = create(&mut conn, "T", None, None, "s-owner", Some("t1"), None, None, None, None).unwrap()["discussion_id"]
            .as_str()
            .unwrap()
            .to_string();
        say_or_reply(&mut conn, &did, "hello", None, None, "s-worker", Some("t1"), None).unwrap();
        let legacy = json!({ "discussion_id": did, "closed_by": "Owner" });
        db::with_write(&mut conn, |tx| events::emit(tx, "t1", None, EV_CLOSED, Some(&legacy))).unwrap();
        let oc = open(&conn, &did, "s-owner", Some("t1")).unwrap();
        assert_eq!(oc["closed"], json!(true));
        assert_eq!(oc["summary_auto"], json!(true));
        let s = oc["summary"].as_str().unwrap();
        assert!(s.contains("参与者"));
        assert!(s.contains("hello"));
    }

    #[test]
    fn discussion_reply_to_unknown_message_rejected() {
        let mut conn = test_conn();
        seed(&conn);
        let did = create(&mut conn, "T", None, None, "s-owner", Some("t1"), None, None, None, None).unwrap()["discussion_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(say_or_reply(&mut conn, &did, "hi", None, Some(&format!("{did}#9999")), "s-worker", Some("t1"), None).is_err());
    }

    #[test]
    fn discussion_requires_active_member_of_team() {
        let mut conn = test_conn();
        seed(&conn);
        // Unknown session errors
        assert!(create(&mut conn, "X", None, None, "nobody", Some("t1"), None, None, None, None).is_err());
        // Cross team: member is only in t1
        assert!(create(&mut conn, "X", None, None, "s-worker", Some("other"), None, None, None, None).is_err());
    }

    /// D-06: quotes with a message ref get their snapshot re-generated by the
    /// server from the ledger (a forged client text is replaced).
    #[test]
    fn discussion_quote_snapshot_regenerated_by_server() {
        let mut conn = test_conn();
        seed(&conn);
        let did = create(&mut conn, "Q", None, None, "s-owner", Some("t1"), None, None, None, None).unwrap()["discussion_id"]
            .as_str()
            .unwrap()
            .to_string();
        let m = say_or_reply(&mut conn, &did, "真实原文", None, None, "s-owner", Some("t1"), None).unwrap();
        let msg1 = m["message_id"].as_str().unwrap().to_string();

        // Client claims a forged text; server must replace it with the real one.
        let q = json!([{ "kind": "message", "ref": msg1, "text": "forged!!!" }]);
        say_or_reply(&mut conn, &did, "引用", Some(&q.to_string()), None, "s-owner", Some("t1"), None).unwrap();
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
        let did2 = create(&mut conn, "InT2", None, None, "s-owner2", Some("t2"), None, None, None, None).unwrap()["discussion_id"]
            .as_str()
            .unwrap()
            .to_string();
        let m2 = say_or_reply(&mut conn, &did2, "t2 消息", None, None, "s-owner2", Some("t2"), None).unwrap();
        let m2id = m2["message_id"].as_str().unwrap().to_string();

        // From t1, replying to a t2 message must fail (snapshot_message scopes by team).
        let did1 = create(&mut conn, "InT1", None, None, "s-owner", Some("t1"), None, None, None, None).unwrap()["discussion_id"]
            .as_str()
            .unwrap()
            .to_string();
        let r = say_or_reply(&mut conn, &did1, "reply cross team", None, Some(&m2id), "s-owner", Some("t1"), None);
        assert!(r.is_err());
    }

    /// D-08: body length and quote-count caps are enforced.
    #[test]
    fn discussion_length_caps_enforced() {
        let mut conn = test_conn();
        seed(&conn);
        let did = create(&mut conn, "Len", None, None, "s-owner", Some("t1"), None, None, None, None).unwrap()["discussion_id"]
            .as_str()
            .unwrap()
            .to_string();
        // oversize body
        let big = "x".repeat(MAX_BODY + 10);
        assert!(say_or_reply(&mut conn, &did, &big, None, None, "s-owner", Some("t1"), None).is_err());
        // too many quotes
        let many: Vec<Value> = (0..MAX_QUOTES + 1).map(|_| json!({"kind":"other","ref":"","text":"t"})).collect();
        let qs = json!(many).to_string();
        assert!(say_or_reply(&mut conn, &did, "hi", Some(&qs), None, "s-owner", Some("t1"), None).is_err());
    }

    // --- Quota (per-topic participation) tests ---

    #[test]
    fn fresh_member_default_quota_covers_multi_question_grill() {
        // Fresh members inherit the column default: it must fit a 6-question
        // grill (one active post per question) with headroom. Mention replies
        // never consume quota, so 20 active posts is ample without
        // meaningfully weakening spam protection.
        let conn = test_conn();
        seed(&conn);
        let q: i64 = conn
            .query_row(
                "SELECT discussion_quota FROM members WHERE id = 'm-worker' AND team_id = 't1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(q, 20);
    }

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
        let did = create(&mut conn, "Q", None, None, "s-owner", Some("t1"), None, None, None, None).unwrap()["discussion_id"]
            .as_str()
            .unwrap()
            .to_string();
        // worker makes 2 active posts (ok), third is blocked
        assert!(say_or_reply(&mut conn, &did, "a", None, None, "s-worker", Some("t1"), None).is_ok());
        assert!(say_or_reply(&mut conn, &did, "b", None, None, "s-worker", Some("t1"), None).is_ok());
        let r = say_or_reply(&mut conn, &did, "c", None, None, "s-worker", Some("t1"), None);
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("quota reached"));
    }

    #[test]
    fn quota_is_per_topic_independent() {
        let mut conn = test_conn();
        seed(&conn);
        set_quota(&conn, "m-worker", 1);
        let d1 = create(&mut conn, "T1", None, None, "s-owner", Some("t1"), None, None, None, None).unwrap()["discussion_id"]
            .as_str()
            .unwrap()
            .to_string();
        let d2 = create(&mut conn, "T2", None, None, "s-owner", Some("t1"), None, None, None, None).unwrap()["discussion_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(say_or_reply(&mut conn, &d1, "a", None, None, "s-worker", Some("t1"), None).is_ok());
        // T1 now at limit: blocked
        assert!(say_or_reply(&mut conn, &d1, "b", None, None, "s-worker", Some("t1"), None).is_err());
        // T2 still fresh: allowed
        assert!(say_or_reply(&mut conn, &d2, "x", None, None, "s-worker", Some("t1"), None).is_ok());
    }

    #[test]
    fn mention_reply_does_not_consume_quota_and_bypasses_limit() {
        let mut conn = test_conn();
        seed(&conn);
        set_quota(&conn, "m-worker", 1);
        let did = create(&mut conn, "M", None, None, "s-owner", Some("t1"), None, None, None, None).unwrap()["discussion_id"]
            .as_str()
            .unwrap()
            .to_string();
        // owner posts a message mentioning @Worker
        let ow = say_or_reply(&mut conn, &did, "@Worker 请看一下", None, None, "s-owner", Some("t1"), None).unwrap();
        let ow_id = ow["message_id"].as_str().unwrap().to_string();
        // worker consumes its 1 active post
        assert!(say_or_reply(&mut conn, &did, "active", None, None, "s-worker", Some("t1"), None).is_ok());
        // worker's active is now 1/1 -> plain say blocked
        assert!(say_or_reply(&mut conn, &did, "extra", None, None, "s-worker", Some("t1"), None).is_err());
        // but replying to the @ mention is allowed (no quota)
        assert!(say_or_reply(&mut conn, &did, "ok on it", None, Some(&ow_id), "s-worker", Some("t1"), None).is_ok());
        // and it did NOT count: a second plain say is still blocked (still 1/1)
        assert!(say_or_reply(&mut conn, &did, "extra2", None, None, "s-worker", Some("t1"), None).is_err());
    }

    #[test]
    fn raising_quota_resumes_posting() {
        let mut conn = test_conn();
        seed(&conn);
        set_quota(&conn, "m-worker", 1);
        let did = create(&mut conn, "R", None, None, "s-owner", Some("t1"), None, None, None, None).unwrap()["discussion_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(say_or_reply(&mut conn, &did, "a", None, None, "s-worker", Some("t1"), None).is_ok());
        assert!(say_or_reply(&mut conn, &did, "b", None, None, "s-worker", Some("t1"), None).is_err());
        set_quota(&conn, "m-worker", 3);
        assert!(say_or_reply(&mut conn, &did, "b", None, None, "s-worker", Some("t1"), None).is_ok());
    }

    // --- G5 wake_discussions (inbox trigger) tests ---

    #[test]
    fn wake_on_mention_of_self() {
        let mut conn = test_conn();
        seed(&conn);
        let did = create(&mut conn, "W", None, None, "s-owner", Some("t1"), None, None, None, None).unwrap()["discussion_id"]
            .as_str()
            .unwrap()
            .to_string();
        // owner posts a message that mentions @Worker -> worker should wake
        say_or_reply(&mut conn, &did, "@Worker 请复核", None, None, "s-owner", Some("t1"), None).unwrap();
        let woke = wake_discussions(&conn, "t1", "m-worker", "Worker", 0).unwrap();
        assert_eq!(woke.len(), 1, "worker mentioned by exact name should wake");
        assert_eq!(woke[0]["discussion_id"], did);
        assert_eq!(woke[0]["trigger"]["mentioned"], true);
    }

    #[test]
    fn no_wake_without_mention_sub_or_task() {
        let mut conn = test_conn();
        seed(&conn);
        let did = create(&mut conn, "N", None, None, "s-owner", Some("t1"), None, None, None, None).unwrap()["discussion_id"]
            .as_str()
            .unwrap()
            .to_string();
        say_or_reply(&mut conn, &did, "普通消息，无关", None, None, "s-owner", Some("t1"), None).unwrap();
        let woke = wake_discussions(&conn, "t1", "m-worker", "Worker", 0).unwrap();
        assert!(woke.is_empty(), "no @ / sub / task link -> no wake");
    }

    #[test]
    fn wake_when_subscribed() {
        let mut conn = test_conn();
        seed(&conn);
        let did = create(&mut conn, "S", None, None, "s-owner", Some("t1"), None, None, None, None).unwrap()["discussion_id"]
            .as_str()
            .unwrap()
            .to_string();
        subscribe(&conn, "t1", "m-worker", &did).unwrap();
        assert!(is_subscribed(&conn, "t1", "m-worker", &did).unwrap());
        // owner posts an unrelated message; worker is subscribed so it wakes
        say_or_reply(&mut conn, &did, "更新", None, None, "s-owner", Some("t1"), None).unwrap();
        let woke = wake_discussions(&conn, "t1", "m-worker", "Worker", 0).unwrap();
        assert_eq!(woke.len(), 1);
        assert_eq!(woke[0]["trigger"]["subscribed"], true);
        // after unsubscribe, further messages no longer wake
        unsubscribe(&conn, "t1", "m-worker", &did).unwrap();
        assert!(!is_subscribed(&conn, "t1", "m-worker", &did).unwrap());
    }

    #[test]
    fn wake_skips_own_messages_and_respects_cursor() {
        let mut conn = test_conn();
        seed(&conn);
        let did = create(&mut conn, "C", None, None, "s-owner", Some("t1"), None, None, None, None).unwrap()["discussion_id"]
            .as_str()
            .unwrap()
            .to_string();
        // worker messages itself (should not wake); then owner mentions worker
        say_or_reply(&mut conn, &did, "@Worker 自查", None, None, "s-worker", Some("t1"), None).unwrap();
        let m = say_or_reply(&mut conn, &did, "@Worker 请处理", None, None, "s-owner", Some("t1"), None).unwrap();
        let seq = m["seq"].as_i64().unwrap();
        let woke = wake_discussions(&conn, "t1", "m-worker", "Worker", 0).unwrap();
        // Only the owner's @mention wakes; worker's own message is skipped.
        assert_eq!(woke.len(), 1, "self messages must be skipped");
        assert_eq!(woke[0]["author"], "Owner");
        // cursor at the owner's seq means nothing newer remains
        let woke2 = wake_discussions(&conn, "t1", "m-worker", "Worker", seq).unwrap();
        assert!(woke2.is_empty(), "seq <= cursor should not be re-delivered");
    }

    #[test]
    fn discussions_have_sequential_numbers() {
        let mut conn = test_conn();
        seed(&conn);
        let d1 = create(&mut conn, "A", None, None, "s-owner", Some("t1"), None, None, None, None).unwrap()["discussion_id"]
            .as_str().unwrap().to_string();
        let d2 = create(&mut conn, "B", None, None, "s-owner", Some("t1"), None, None, None, None).unwrap()["discussion_id"]
            .as_str().unwrap().to_string();
        let l = list(&conn, true, "s-owner", Some("t1")).unwrap();
        let arr = l["discussions"].as_array().unwrap();
        let n = |id: &str| arr.iter().find(|x| x["discussion_id"] == json!(id)).unwrap()["number"].as_i64().unwrap();
        assert_eq!((n(&d1), n(&d2)), (1, 2));
        assert_eq!(open(&conn, &d2, "s-owner", Some("t1")).unwrap()["number"], json!(2));
    }

    // --- brainstorm mode (design/007) ---

    fn dirs_json() -> String {
        json!([{
            "direction_id": "d1", "title": "方向A",
            "source_message_ids": ["x#1"], "next_step": "做原型验证"
        }]).to_string()
    }

    #[test]
    fn discussion_kind_derivation_and_validation() {
        // 派生：mode=brainstorm → kind=brainstorm；显式 grill 优先；默认 normal
        assert_eq!(discussion_kind(Some(&json!("brainstorm")), None), "brainstorm");
        assert_eq!(discussion_kind(None, Some(&json!("grill"))), "grill");
        assert_eq!(discussion_kind(Some(&json!("brainstorm")), Some(&json!("grill"))), "grill");
        assert_eq!(discussion_kind(None, None), "normal");
        assert_eq!(discussion_kind(None, Some(&json!("bogus"))), "normal");

        let mut conn = test_conn();
        seed(&conn);
        // 显式 grill
        let g = create(&mut conn, "G", None, None, "s-owner", Some("t1"), None, None, None, Some("grill")).unwrap();
        assert_eq!(g["kind"], json!("grill"));
        // --kind brainstorm 等价于 --mode brainstorm（自动补 mode）
        let b = create(&mut conn, "B", None, None, "s-owner", Some("t1"), None, None, None, Some("brainstorm")).unwrap();
        assert_eq!(b["kind"], json!("brainstorm"));
        assert_eq!(b["mode"], json!("brainstorm"));
        // 普通讨论（不传 kind）
        let n = create(&mut conn, "N", None, None, "s-owner", Some("t1"), None, None, None, None).unwrap();
        assert_eq!(n["kind"], json!("normal"));
        // 列表暴露 kind
        let list = list(&conn, true, "s-owner", Some("t1")).unwrap();
        let ks: Vec<String> = list["discussions"].as_array().unwrap().iter()
            .filter_map(|d| d.get("kind").and_then(|k| k.as_str()).map(str::to_string)).collect();
        assert!(ks.contains(&"grill".to_string()), "{ks:?}");
        assert!(ks.contains(&"brainstorm".to_string()), "{ks:?}");
        assert!(ks.contains(&"normal".to_string()), "{ks:?}");
        // 未知 kind 报错
        assert!(create(&mut conn, "X", None, None, "s-owner", Some("t1"), None, None, None, Some("bogus")).is_err());
    }

    fn bs_create(conn: &mut Connection, window_secs: Option<i64>, min_ideas: Option<i64>) -> String {
        create(conn, "BS", None, None, "s-owner", Some("t1"), Some("brainstorm"), window_secs, min_ideas, None)
            .unwrap()["discussion_id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn brainstorm_phase_derivation_three_branches() {
        let mut conn = test_conn();
        seed(&conn);
        let did = bs_create(&mut conn, Some(86_400), None);
        let now = db::now();
        let evs = events::list(&conn, "t1", None).unwrap();
        assert_eq!(brainstorm_phase(&evs, &did, &now), "diverging");
        // 窗口过期 → 可聚类
        assert_eq!(brainstorm_phase(&evs, &did, "2999-01-01T00:00:00+00:00"), "clusterable");
        // 收窗 → harvested
        say_or_reply(&mut conn, &did, "i1", None, None, "s-worker", Some("t1"), Some("idea")).unwrap();
        close(&mut conn, &did, None, Some(&dirs_json()), "s-owner", Some("t1")).unwrap();
        let evs2 = events::list(&conn, "t1", None).unwrap();
        assert_eq!(brainstorm_phase(&evs2, &did, &now), "harvested");
        // open() 暴露 mode/phase/directions
        let o = open(&conn, &did, "s-owner", Some("t1")).unwrap();
        assert_eq!(o["mode"], json!("brainstorm"));
        assert_eq!(o["phase"], json!("harvested"));
        assert_eq!(o["directions"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn brainstorm_requires_kind_in_diverging_window() {
        let mut conn = test_conn();
        seed(&conn);
        let did = bs_create(&mut conn, None, None);
        // 无 kind → 拒
        assert!(say_or_reply(&mut conn, &did, "no kind", None, None, "s-worker", Some("t1"), None).is_err());
        // 三类可直接发的 kind 正例
        for k in ["idea", "question", "reference"] {
            assert!(
                say_or_reply(&mut conn, &did, "x", None, None, "s-worker", Some("t1"), Some(k)).is_ok(),
                "kind {k} should be accepted"
            );
        }
        // concern 必须 reply_to 某 idea：无 reply_to 拒、挂 idea 后过
        assert!(say_or_reply(&mut conn, &did, "c", None, None, "s-worker", Some("t1"), Some("concern")).is_err());
        let idea = say_or_reply(&mut conn, &did, "an idea", None, None, "s-worker", Some("t1"), Some("idea"))
            .unwrap()["message_id"].as_str().unwrap().to_string();
        assert!(say_or_reply(&mut conn, &did, "c", None, Some(&idea), "s-worker", Some("t1"), Some("concern")).is_ok());
        assert_eq!(BRAINSTORM_KINDS.len(), 4);
    }

    #[test]
    fn brainstorm_min_ideas_enforced_for_lead_but_owner_exempt() {
        let mut conn = test_conn();
        seed(&conn);
        conn.execute("UPDATE members SET is_lead = 1 WHERE id = 'm-worker'", []).unwrap();
        let did = bs_create(&mut conn, None, Some(2));
        say_or_reply(&mut conn, &did, "i1", None, None, "s-worker", Some("t1"), Some("idea")).unwrap();
        // lead 收窗：参与者 idea 数 1 < 2 → 拒
        assert!(close(&mut conn, &did, None, Some(&dirs_json()), "s-worker", Some("t1")).is_err());
        // owner 豁免 → 过
        assert!(close(&mut conn, &did, None, Some(&dirs_json()), "s-owner", Some("t1")).is_ok());
    }

    #[test]
    fn brainstorm_no_auto_digest_normal_thread_keeps_digest() {
        let mut conn = test_conn();
        seed(&conn);
        // 普通讨论：无 summary 关闭 → auto digest（存量行为回归锚）
        let d = create(&mut conn, "N", None, None, "s-owner", Some("t1"), None, None, None, None)
            .unwrap()["discussion_id"]
            .as_str()
            .unwrap()
            .to_string();
        say_or_reply(&mut conn, &d, "hi", None, None, "s-worker", Some("t1"), None).unwrap();
        let c = close(&mut conn, &d, None, None, "s-owner", Some("t1")).unwrap();
        assert_eq!(c["summary_auto"], json!(true));
        // brainstorm：恒不生成
        let b = bs_create(&mut conn, None, None);
        say_or_reply(&mut conn, &b, "i", None, None, "s-worker", Some("t1"), Some("idea")).unwrap();
        let cb = close(&mut conn, &b, None, Some(&dirs_json()), "s-owner", Some("t1")).unwrap();
        assert_eq!(cb["summary_auto"], json!(false));
        assert_eq!(cb["summary"], json!(""));
    }

    #[test]
    fn directions_structure_and_rejection_list() {
        // 合法
        assert!(validate_directions(&dirs_json()).is_ok());
        // 缺 source_message_ids
        assert!(validate_directions(&json!([{ "next_step": "x" }]).to_string()).is_err());
        // 缺 next_step
        assert!(validate_directions(&json!([{ "source_message_ids": ["a#1"] }]).to_string()).is_err());
        // 判定类字段出现即拒
        for f in FORBIDDEN_DIRECTION_FIELDS {
            let mut d = json!([{ "source_message_ids": ["a#1"], "next_step": "x" }]);
            d[0][*f] = json!(1);
            assert!(validate_directions(&d.to_string()).is_err(), "{f} must be rejected");
        }
    }

    #[test]
    fn handoff_requires_harvested_target_exists_and_no_duplicate() {
        let mut conn = test_conn();
        seed(&conn);
        let did = bs_create(&mut conn, None, None);
        // 未收窗 → 拒
        assert!(handoff(&mut conn, &did, "d1", "discussion", "dsc_nope", "s-owner", Some("t1")).is_err());
        say_or_reply(&mut conn, &did, "i", None, None, "s-worker", Some("t1"), Some("idea")).unwrap();
        close(&mut conn, &did, None, Some(&dirs_json()), "s-owner", Some("t1")).unwrap();
        // 目标不存在 → 拒
        assert!(handoff(&mut conn, &did, "d1", "discussion", "dsc_nope", "s-owner", Some("t1")).is_err());
        // 方向不存在 → 拒
        assert!(handoff(&mut conn, &did, "d404", "discussion", "dsc_nope", "s-owner", Some("t1")).is_err());
        // 非 lead → 拒
        let target = create(&mut conn, "target", None, None, "s-owner", Some("t1"), None, None, None, None)
            .unwrap()["discussion_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(handoff(&mut conn, &did, "d1", "discussion", &target, "s-worker", Some("t1")).is_err());
        // 正常交接
        assert!(handoff(&mut conn, &did, "d1", "discussion", &target, "s-owner", Some("t1")).is_ok());
        // 同方向二次 → 拒
        assert!(handoff(&mut conn, &did, "d1", "discussion", &target, "s-owner", Some("t1")).is_err());
        // open() 暴露 handed_off
        let o = open(&conn, &did, "s-owner", Some("t1")).unwrap();
        assert_eq!(o["handed_off"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn list_exposes_brainstorm_phase_and_needs_harvest() {
        let mut conn = test_conn();
        seed(&conn);
        // 窗口 0 秒 → 立即进入「可聚类（待收窗）」
        let did = bs_create(&mut conn, Some(0), None);
        let l = list(&conn, false, "s-owner", Some("t1")).unwrap();
        let t = l["discussions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["discussion_id"] == json!(did))
            .unwrap()
            .clone();
        assert_eq!(t["mode"], json!("brainstorm"));
        assert_eq!(t["phase"], json!("clusterable"));
        let need = brainstorm_needs_harvest(&conn, "t1").unwrap();
        assert_eq!(need.len(), 1);
        assert_eq!(need[0]["discussion_id"], json!(did));
        // 普通讨论不出现在待收窗列表
        create(&mut conn, "plain", None, None, "s-owner", Some("t1"), None, None, None, None).unwrap();
        assert_eq!(brainstorm_needs_harvest(&conn, "t1").unwrap().len(), 1);
    }

    #[test]
    fn brainstorm_close_requires_directions() {
        let mut conn = test_conn();
        seed(&conn);
        let did = bs_create(&mut conn, None, Some(1));
        say_or_reply(&mut conn, &did, "i", None, None, "s-worker", Some("t1"), Some("idea")).unwrap();
        // 缺 directions → 拒
        assert!(close(&mut conn, &did, None, None, "s-owner", Some("t1")).is_err());
        // 带 directions → 过
        assert!(close(&mut conn, &did, None, Some(&dirs_json()), "s-owner", Some("t1")).is_ok());
    }
}
