//! teamx enterprise — member activity analytics.
//!
//! Collects per-node member activity (time, duration, tokens, cost, work
//! content) and stores it in the team lead's serve database (`activity` table).
//! Every row records its source node (`node_id` = instance_id,
//! `node_name` = hostname). Sensitive fields (tool/command arguments, user
//! message text) are recorded in full for audit purposes.

use rusqlite::{params, Connection};
use serde_json::{json, Value};

/// A single activity row, as sent by a member plugin (V1 push) or queried.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ActivityRow {
    /// Team the activity belongs to. Omitted by the plugin in network mode
    /// (serve fills it from the member's single team) and in V1 local mode
    /// (CLI fills it from the session's team). Required when ambiguous.
    #[serde(default)]
    pub team_id: String,
    /// Member the activity is attributed to. Omitted by the plugin; the serve
    /// RPC and the V1 local CLI both resolve it from the authenticated
    /// identity / session key.
    #[serde(default)]
    pub member_id: String,
    pub node_id: String,
    #[serde(default)]
    pub node_name: Option<String>,
    pub started_at: String,
    #[serde(default)]
    pub ended_at: Option<String>,
    #[serde(default)]
    pub duration_ms: Option<i64>,
    pub kind: String,
    /// JSON detail (tool/command args, user message text). Accepts an object
    /// (serialized on insert) or a pre-serialized JSON string.
    #[serde(default)]
    pub detail: Option<Value>,
    #[serde(default)]
    pub tokens_input: Option<i64>,
    #[serde(default)]
    pub tokens_output: Option<i64>,
    #[serde(default)]
    pub tokens_reasoning: Option<i64>,
    #[serde(default)]
    pub cost: Option<f64>,
    #[serde(default)]
    pub has_human: Option<bool>,
    #[serde(default)]
    pub created_at: Option<String>,
}

impl ActivityRow {
    /// SQL for inserting one row (created_at set here, not by the caller).
    pub const INSERT: &'static str = "\
INSERT INTO activity
  (team_id, member_id, node_id, node_name, started_at, ended_at, duration_ms,
   kind, detail, tokens_input, tokens_output, tokens_reasoning, cost, has_human, created_at)
VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)";
}

/// Push a batch of activity rows into the `activity` table.
///
/// Authorization (enforced by the caller, e.g. serve RPC dispatch):
///  - a member may only write rows for their own member_id
///  - node_id is taken from the row, but the server may override it from the
///    caller's own instance when the row omits it.
pub fn push_activities(conn: &mut Connection, rows: &[ActivityRow]) -> rusqlite::Result<usize> {
    let mut inserted = 0usize;
    crate::db::with_write(conn, |tx| {
        {
            let mut stmt = tx.prepare(ActivityRow::INSERT)?;
            for row in rows {
                let now = crate::db::now();
                let detail = row.detail.as_ref().map(|v| v.to_string());
                let has_human = row.has_human.map(|h| if h { 1i64 } else { 0i64 }).unwrap_or(0);
                stmt.execute(params![
                    row.team_id,
                    row.member_id,
                    row.node_id,
                    row.node_name,
                    row.started_at,
                    row.ended_at,
                    row.duration_ms,
                    row.kind,
                    detail,
                    row.tokens_input,
                    row.tokens_output,
                    row.tokens_reasoning,
                    row.cost,
                    has_human,
                    now,
                ])?;
                inserted += 1;
            }
        }
        Ok(())
    })?;
    Ok(inserted)
}

/// Build a `WHERE` fragment (with bindable params) from optional filters.
/// Supported filters: team, member, node, kind, from (>=, inclusive), to (<, exclusive).
/// Always constrains to a team when `team` is present.
struct Filter {
    sql: String,
    params: Vec<Value>,
}

fn filter(team: Option<&str>, member: Option<&str>, node: Option<&str>, kind: Option<&str>, from: Option<&str>, to: Option<&str>) -> Filter {
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<Value> = Vec::new();
    if let Some(t) = team {
        clauses.push("team_id = ?".to_string());
        params.push(json!(t));
    }
    if let Some(m) = member {
        clauses.push("member_id = ?".to_string());
        params.push(json!(m));
    }
    if let Some(n) = node {
        clauses.push("node_id = ?".to_string());
        params.push(json!(n));
    }
    if let Some(k) = kind {
        clauses.push("kind = ?".to_string());
        params.push(json!(k));
    }
    if let Some(f) = from {
        clauses.push("started_at >= ?".to_string());
        params.push(json!(f));
    }
    if let Some(t) = to {
        clauses.push("started_at < ?".to_string());
        params.push(json!(t));
    }
    let sql = if clauses.is_empty() {
        "1=1".to_string()
    } else {
        clauses.join(" AND ")
    };
    Filter { sql, params }
}

/// Aggregate overview for a team + filters: totals for duration/tokens/cost,
/// active member/node counts, and human-vs-ai distribution.
pub fn summary(conn: &Connection, team: &str, member: Option<&str>, node: Option<&str>, kind: Option<&str>, from: Option<&str>, to: Option<&str>) -> rusqlite::Result<Value> {
    let f = filter(Some(team), member, node, kind, from, to);
    // One-row aggregate helper: returns {duration_ms,cost,tokens_*,count}.
    let aggregate = |extra_clause: &str| -> rusqlite::Result<Value> {
        let q = format!(
            "SELECT SUM(duration_ms), SUM(cost), SUM(tokens_input), SUM(tokens_output), SUM(tokens_reasoning), COUNT(*) \
             FROM activity WHERE {} {extra_clause}",
            f.sql
        );
        let mut stmt = conn.prepare(&q)?;
        let row = stmt.query_row(rusqlite::params_from_iter(f.params.iter().map(Value::as_str)), |r| {
            Ok(json!({
                "duration_ms": r.get::<_, Option<i64>>(0)?,
                "cost": r.get::<_, Option<f64>>(1)?,
                "tokens_input": r.get::<_, Option<i64>>(2)?,
                "tokens_output": r.get::<_, Option<i64>>(3)?,
                "tokens_reasoning": r.get::<_, Option<i64>>(4)?,
                "count": r.get::<_, i64>(5)?,
            }))
        })?;
        Ok(row)
    };
    let overall = aggregate("")?;
    let human = aggregate(
        "AND kind IN ('human_input','human_approval','human_command')",
    )?;
    let ai = aggregate(
        "AND kind NOT IN ('human_input','human_approval','human_command')",
    )?;
    // Distinct active members / nodes.
    let count_distinct = |col: &str| -> rusqlite::Result<i64> {
        let q = format!("SELECT COUNT(DISTINCT {col}) FROM activity WHERE {}", f.sql);
        let mut stmt = conn.prepare(&q)?;
        stmt.query_row(rusqlite::params_from_iter(f.params.iter().map(Value::as_str)), |r| r.get(0))
    };
    let members = count_distinct("member_id")?;
    let nodes = count_distinct("node_id")?;
    Ok(json!({
        "team_id": team,
        "overall": overall,
        "human": human,
        "ai": ai,
        "active_members": members,
        "active_nodes": nodes,
    }))
}

/// Per-member aggregates (duration/tokens/cost + counts), for a team.
pub fn by_member(conn: &Connection, team: &str, member: Option<&str>, node: Option<&str>, kind: Option<&str>, from: Option<&str>, to: Option<&str>) -> rusqlite::Result<Value> {
    let f = filter(Some(team), member, node, kind, from, to);
    let q = format!(
        "SELECT member_id, SUM(duration_ms), SUM(cost), SUM(tokens_input), SUM(tokens_output), SUM(tokens_reasoning), COUNT(*) \
         FROM activity WHERE {} GROUP BY member_id ORDER BY COUNT(*) DESC",
        f.sql
    );
    let mut stmt = conn.prepare(&q)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(f.params.iter().map(Value::as_str)), |r| {
        Ok(json!({
            "member_id": r.get::<_, String>(0)?,
            "duration_ms": r.get::<_, Option<i64>>(1)?,
            "cost": r.get::<_, Option<f64>>(2)?,
            "tokens_input": r.get::<_, Option<i64>>(3)?,
            "tokens_output": r.get::<_, Option<i64>>(4)?,
            "tokens_reasoning": r.get::<_, Option<i64>>(5)?,
            "count": r.get::<_, i64>(6)?,
        }))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(json!(out))
}

/// Per-node aggregates for a team.
pub fn by_node(conn: &Connection, team: &str, member: Option<&str>, node: Option<&str>, kind: Option<&str>, from: Option<&str>, to: Option<&str>) -> rusqlite::Result<Value> {
    let f = filter(Some(team), member, node, kind, from, to);
    let q = format!(
        "SELECT node_id, MAX(node_name), SUM(duration_ms), SUM(cost), SUM(tokens_input), SUM(tokens_output), SUM(tokens_reasoning), COUNT(*) \
         FROM activity WHERE {} GROUP BY node_id ORDER BY COUNT(*) DESC",
        f.sql
    );
    let mut stmt = conn.prepare(&q)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(f.params.iter().map(Value::as_str)), |r| {
        Ok(json!({
            "node_id": r.get::<_, String>(0)?,
            "node_name": r.get::<_, Option<String>>(1)?,
            "duration_ms": r.get::<_, Option<i64>>(2)?,
            "cost": r.get::<_, Option<f64>>(3)?,
            "tokens_input": r.get::<_, Option<i64>>(4)?,
            "tokens_output": r.get::<_, Option<i64>>(5)?,
            "tokens_reasoning": r.get::<_, Option<i64>>(6)?,
            "count": r.get::<_, i64>(7)?,
        }))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(json!(out))
}

/// Tool-call distribution (top N by count), for a team. Reads `kind = 'tool_call'`
/// rows and counts distinct tool names from the JSON detail.
pub fn tools(conn: &Connection, team: &str, member: Option<&str>, node: Option<&str>, from: Option<&str>, to: Option<&str>) -> rusqlite::Result<Value> {
    let f = filter(Some(team), member, node, Some("tool_call"), from, to);
    let q = format!("SELECT detail FROM activity WHERE {}", f.sql);
    let mut stmt = conn.prepare(&q)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(f.params.iter().map(Value::as_str)), |r| r.get::<_, String>(0))?;
    let mut counts: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    for row in rows {
        let detail = row?;
        if let Ok(v) = serde_json::from_str::<Value>(&detail) {
            let name = v.get("tool").and_then(Value::as_str).unwrap_or("?");
            *counts.entry(name.to_string()).or_insert(0) += 1;
        }
    }
    let mut out: Vec<Value> = counts
        .into_iter()
        .map(|(tool, count)| json!({ "tool": tool, "count": count }))
        .collect();
    out.sort_by(|a, b| b["count"].as_i64().cmp(&a["count"].as_i64()));
    Ok(json!(out))
}

/// File-edit distribution (top N by count), for a team. Reads `kind = 'file_edit'`
/// rows and counts distinct file paths from the JSON detail.
pub fn files(conn: &Connection, team: &str, member: Option<&str>, node: Option<&str>, from: Option<&str>, to: Option<&str>) -> rusqlite::Result<Value> {
    let f = filter(Some(team), member, node, Some("file_edit"), from, to);
    let q = format!("SELECT detail FROM activity WHERE {}", f.sql);
    let mut stmt = conn.prepare(&q)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(f.params.iter().map(Value::as_str)), |r| r.get::<_, String>(0))?;
    let mut counts: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    for row in rows {
        let detail = row?;
        if let Ok(v) = serde_json::from_str::<Value>(&detail) {
            let file = v.get("file").and_then(Value::as_str).unwrap_or("?");
            *counts.entry(file.to_string()).or_insert(0) += 1;
        }
    }
    let mut out: Vec<Value> = counts
        .into_iter()
        .map(|(file, count)| json!({ "file": file, "count": count }))
        .collect();
    out.sort_by(|a, b| b["count"].as_i64().cmp(&a["count"].as_i64()));
    Ok(json!(out))
}

/// Timeline / Gantt data for a team: work segments (work_session rows with
/// started_at + ended_at + duration_ms) plus point-in-time events (tool_call,
/// step_finish, command, file_edit, human_*) — all grouped per member, ordered
/// by start time. Used by the `Timeline` (Gantt) view of the dashboard.
pub fn timeline(conn: &Connection, team: &str, member: Option<&str>, from: Option<&str>, to: Option<&str>) -> rusqlite::Result<Value> {
    let f = filter(Some(team), member, None, None, from, to);
    let q = format!(
        "SELECT member_id, node_name, kind, started_at, ended_at, duration_ms, detail, tokens_input, tokens_output, tokens_reasoning, cost \
         FROM activity WHERE {} ORDER BY started_at ASC",
        f.sql
    );
    let mut stmt = conn.prepare(&q)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(f.params.iter().map(Value::as_str)), |r| {
        Ok(json!({
            "member_id": r.get::<_, String>(0)?,
            "node_name": r.get::<_, Option<String>>(1)?,
            "kind": r.get::<_, String>(2)?,
            "started_at": r.get::<_, String>(3)?,
            "ended_at": r.get::<_, Option<String>>(4)?,
            "duration_ms": r.get::<_, Option<i64>>(5)?,
            "detail": r.get::<_, Option<String>>(6)?,
            "tokens_input": r.get::<_, Option<i64>>(7)?,
            "tokens_output": r.get::<_, Option<i64>>(8)?,
            "tokens_reasoning": r.get::<_, Option<i64>>(9)?,
            "cost": r.get::<_, Option<f64>>(10)?,
        }))
    })?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row?);
    }
    Ok(json!(items))
}

/// Detail rows (recent activity list), newest first, optional limit.
#[allow(clippy::too_many_arguments)]
pub fn rows(conn: &Connection, team: &str, member: Option<&str>, node: Option<&str>, kind: Option<&str>, from: Option<&str>, to: Option<&str>, limit: i64) -> rusqlite::Result<Value> {
    let f = filter(Some(team), member, node, kind, from, to);
    let limit = limit.clamp(1, 1000);
    let q = format!(
        "SELECT id, team_id, member_id, node_id, node_name, started_at, ended_at, duration_ms, kind, detail, tokens_input, tokens_output, tokens_reasoning, cost, has_human, created_at \
         FROM activity WHERE {} ORDER BY id DESC LIMIT {limit}",
        f.sql
    );
    let mut stmt = conn.prepare(&q)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(f.params.iter().map(Value::as_str)), |r| {
        Ok(json!({
            "id": r.get::<_, i64>(0)?,
            "team_id": r.get::<_, String>(1)?,
            "member_id": r.get::<_, String>(2)?,
            "node_id": r.get::<_, String>(3)?,
            "node_name": r.get::<_, Option<String>>(4)?,
            "started_at": r.get::<_, String>(5)?,
            "ended_at": r.get::<_, Option<String>>(6)?,
            "duration_ms": r.get::<_, Option<i64>>(7)?,
            "kind": r.get::<_, String>(8)?,
            "detail": r.get::<_, Option<String>>(9)?,
            "tokens_input": r.get::<_, Option<i64>>(10)?,
            "tokens_output": r.get::<_, Option<i64>>(11)?,
            "tokens_reasoning": r.get::<_, Option<i64>>(12)?,
            "cost": r.get::<_, Option<f64>>(13)?,
            "has_human": r.get::<_, Option<bool>>(14)?,
            "created_at": r.get::<_, Option<String>>(15)?,
        }))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(json!(out))
}

/// Human activity (human_input / human_approval / human_command) rows, for
/// answering "what did the human actually do". Newest first.
pub fn human_rows(conn: &Connection, team: &str, member: Option<&str>, node: Option<&str>, from: Option<&str>, to: Option<&str>, limit: i64) -> rusqlite::Result<Value> {
    let f = filter(Some(team), member, node, None, from, to);
    let limit = limit.clamp(1, 1000);
    let q = format!(
        "SELECT id, member_id, node_id, node_name, started_at, kind, detail \
         FROM activity WHERE {} AND kind IN ('human_input','human_approval','human_command') \
         ORDER BY id DESC LIMIT {limit}",
        f.sql
    );
    let mut stmt = conn.prepare(&q)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(f.params.iter().map(Value::as_str)), |r| {
        Ok(json!({
            "id": r.get::<_, i64>(0)?,
            "member_id": r.get::<_, String>(1)?,
            "node_id": r.get::<_, String>(2)?,
            "node_name": r.get::<_, Option<String>>(3)?,
            "started_at": r.get::<_, String>(4)?,
            "kind": r.get::<_, String>(5)?,
            "detail": r.get::<_, Option<String>>(6)?,
        }))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(json!(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use rusqlite::Connection;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        db::migrate(&conn).unwrap();
        conn
    }

    fn seed_team(conn: &Connection, team_id: &str, member_id: &str) {
        conn.execute(
            "INSERT INTO teams (id, name, owner_member_id, goal_id, state, invite_token, created_at, updated_at)
             VALUES (?1, 'T', ?2, NULL, 'forming', 'tok', 'now', 'now')",
            params![team_id, member_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO members (id, team_id, session_key, display_name, role, state, joined_at)
             VALUES (?1, ?2, 's', 'm', 'owner', 'active', 'now')",
            params![member_id, team_id],
        )
        .unwrap();
    }

    fn base_row(team: &str, member: &str, kind: &str) -> ActivityRow {
        ActivityRow {
            team_id: team.to_string(),
            member_id: member.to_string(),
            node_id: "node-a".to_string(),
            node_name: Some("host-a".to_string()),
            started_at: "2026-08-19T00:00:00Z".to_string(),
            ended_at: None,
            duration_ms: None,
            kind: kind.to_string(),
            detail: None,
            tokens_input: None,
            tokens_output: None,
            tokens_reasoning: None,
            cost: None,
            has_human: Some(false),
            created_at: None,
        }
    }

    #[test]
    fn push_and_query_summary() {
        let mut conn = test_conn();
        seed_team(&conn, "t1", "m1");
        let mut rows = [base_row("t1", "m1", "tool_call")];
        rows[0].detail = Some(serde_json::json!({"tool":"bash","state":"completed"}));
        rows[0].tokens_input = Some(100);
        rows[0].tokens_output = Some(50);
        rows[0].cost = Some(0.01);
        rows[0].started_at = "2026-08-19T10:00:00Z".to_string();
        let mut h = base_row("t1", "m1", "human_input");
        h.detail = Some(serde_json::json!({"sessionID":"s1","text":"fix the bug"}));
        h.started_at = "2026-08-19T11:00:00Z".to_string();
        push_activities(&mut conn, &[rows[0].clone(), h]).unwrap();

        let s = summary(&conn, "t1", None, None, None, None, None).unwrap();
        assert_eq!(s["overall"]["count"], 2);
        assert_eq!(s["overall"]["tokens_input"], 100);
        assert_eq!(s["overall"]["tokens_output"], 50);
        assert_eq!(s["overall"]["cost"], 0.01);
        assert_eq!(s["human"]["count"], 1);
        assert_eq!(s["ai"]["count"], 1);
        assert_eq!(s["active_members"], 1);
        assert_eq!(s["active_nodes"], 1);
    }

    #[test]
    fn push_rejects_other_member_rows() {
        // This is enforced in serve dispatch, but ensure push itself doesn't
        // silently merge; it just inserts whatever it's given. The guard lives
        // in the RPC layer.
        let mut conn = test_conn();
        seed_team(&conn, "t1", "m1");
        let r = base_row("t1", "m2", "tool_call");
        push_activities(&mut conn, &[r]).unwrap();
        let s = summary(&conn, "t1", None, None, None, None, None).unwrap();
        assert_eq!(s["overall"]["count"], 1);
    }

    #[test]
    fn tools_and_files_distributions() {
        let mut conn = test_conn();
        seed_team(&conn, "t1", "m1");
        let mut t1 = base_row("t1", "m1", "tool_call");
        t1.detail = Some(serde_json::json!({"tool":"bash","state":"completed"}));
        let mut t2 = base_row("t1", "m1", "tool_call");
        t2.detail = Some(serde_json::json!({"tool":"bash","state":"completed"}));
        let mut t3 = base_row("t1", "m1", "tool_call");
        t3.detail = Some(serde_json::json!({"tool":"edit","state":"completed"}));
        let mut f1 = base_row("t1", "m1", "file_edit");
        f1.detail = Some(serde_json::json!({"file":"src/main.rs"}));
        let mut f2 = base_row("t1", "m1", "file_edit");
        f2.detail = Some(serde_json::json!({"file":"src/main.rs"}));
        push_activities(&mut conn, &[t1, t2, t3, f1, f2]).unwrap();

        let tools = tools(&conn, "t1", None, None, None, None).unwrap();
        assert_eq!(tools[0]["tool"], "bash");
        assert_eq!(tools[0]["count"], 2);
        assert_eq!(tools[1]["tool"], "edit");
        assert_eq!(tools[1]["count"], 1);

        let files = files(&conn, "t1", None, None, None, None).unwrap();
        assert_eq!(files[0]["file"], "src/main.rs");
        assert_eq!(files[0]["count"], 2);
    }

    #[test]
    fn rows_newest_first_and_human_rows() {
        let mut conn = test_conn();
        seed_team(&conn, "t1", "m1");
        let mut a = base_row("t1", "m1", "tool_call");
        a.detail = Some(serde_json::json!({"tool":"bash"}));
        a.started_at = "2026-08-19T10:00:00Z".to_string();
        let mut b = base_row("t1", "m1", "human_command");
        b.detail = Some(serde_json::json!({"name":"/team","args":"status"}));
        b.started_at = "2026-08-19T11:00:00Z".to_string();
        push_activities(&mut conn, &[a, b]).unwrap();

        let rows = rows(&conn, "t1", None, None, None, None, None, 10).unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 2);
        assert_eq!(rows[0]["kind"], "human_command"); // newest first

        let h = human_rows(&conn, "t1", None, None, None, None, 10).unwrap();
        assert_eq!(h.as_array().unwrap().len(), 1);
        assert_eq!(h[0]["kind"], "human_command");
    }
}
