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

/// 是否是该团队的在册成员（active/idle/waiting；不含 left/denied）。
fn is_member(conn: &Connection, team_id: &str, member_id: &str) -> Result<bool, String> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM members WHERE id = ?1 AND team_id = ?2
               AND state NOT IN ('left','denied','pending')",
            rusqlite::params![member_id, team_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    Ok(n > 0)
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
    // 任何**在册成员**都可以建任务（此前限 lead，导致普通成员发现问题只能绕道找
    // owner；真机踩到：contributor 无法给云端 member 派"重启隧道"的任务）。
    // 任务状态机自带 reject 流，且全程落台账，可审计可回滚。
    if !is_member(conn, team_id, member_id)? {
        return Err("task.create: only a team member can create tasks".to_string());
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
        ("", false, false) => return Err("task.create requires --assignee <member_id>, --role <role>, or --mode bid|broadcast".to_string()),
        ("", true, _) => "direct",
        ("", false, true) => "bid",
        ("direct", true, _) => "direct",
        ("direct", false, _) => return Err("task.create --mode direct requires --assignee".to_string()),
        ("bid", _, _) => "bid",              // role 可空 = 开放池（全队先抢先得）
        ("broadcast", _, _) => "broadcast",  // role 可空 = 广播给全体在册成员
        (other, _, _) => return Err(format!("unknown mode `{other}` (valid: direct, bid, broadcast)")),
    };

    // Instances: direct = [assignee]; bid = [one row, unassigned]; broadcast =
    // one row per active role member (or every active member when role is empty).
    let instances: Vec<Option<String>> = match mode {
        "direct" => vec![assignee.clone()],
        "bid" => vec![None],
        "broadcast" => {
            let mut ids: Vec<String> = Vec::new();
            if let Some(r) = role.as_deref() {
                let mut stmt = conn
                    .prepare("SELECT id FROM members WHERE team_id=?1 AND role=?2 AND state='active'")
                    .map_err(|e| format!("db: {e}"))?;
                let rows = stmt
                    .query_map(params![team_id, r], |row| row.get::<_, String>(0))
                    .map_err(|e| format!("db: {e}"))?;
                for row in rows {
                    ids.push(row.map_err(|e| format!("db: {e}"))?);
                }
            } else {
                let mut stmt = conn
                    .prepare("SELECT id FROM members WHERE team_id=?1 AND state='active'")
                    .map_err(|e| format!("db: {e}"))?;
                let rows = stmt
                    .query_map(params![team_id], |row| row.get::<_, String>(0))
                    .map_err(|e| format!("db: {e}"))?;
                for row in rows {
                    ids.push(row.map_err(|e| format!("db: {e}"))?);
                }
            }
            if ids.is_empty() {
                return Err(match role.as_deref() {
                    Some(r) => format!("broadcast role `{r}` has no active members"),
                    None => "broadcast: no active members".to_string(),
                });
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
                // Emit BOTH field spellings: server-side taskx uses
                // `assignee`/`role`/`task`, while the opencode plugin's
                // automation (auto-ack / auto-claim / auto-execute) matches on
                // the legacy doc-flow names `assignee_member_id` /
                // `assignee_role` / `id` / `executor`. Keep both so every
                // consumer (old plugin bundles included) sees its shape.
                Some(&json!({
                    "doc": "taskx", "task": id, "id": id, "title": title,
                    "mode": mode,
                    "assignee": target_ref, "assignee_member_id": target_ref,
                    "role": role.as_deref(), "assignee_role": role.as_deref(),
                    "priority": priority, "executor": executor,
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
        // 我的任务 = 指派给我的，或（未指派且）开放池/我的角色可认领的。
        // 开放池：assignee 为空且 role_key 为空 → 对全体成员可见（先抢先得）。
        sql.push_str(" AND (assignee_member_id = ?2 OR (assignee_member_id IS NULL AND (role_key = ?3 OR role_key IS NULL)))");
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
    // Open pool: no assignee and no role → any active member may act (先抢先得).
    let open_pool = !is_assignee && assignee.is_none() && role.is_none();
    let role_member = !is_assignee && assignee.is_none() && role.is_some() && role == my_role;
    let can_work = is_assignee || role_member || lead || open_pool;

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
            if !role_member && !lead && !open_pool {
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
    // Effective routing fields for the event payload (aliases for the plugin).
    // `task` is loaded BEFORE the state update; for `claim` the assignee only
    // takes effect in this transition, so the emitted event must already carry
    // the new assignee or the plugin's auto path (which matches on
    // `assignee_member_id`) would miss it.
    let effective_assignee: Option<String> = if action == "claim" {
        Some(member_id.to_string())
    } else {
        assignee.clone()
    };
    let effective_assignee_ref: Option<&str> = effective_assignee.as_deref();
    let role_ref: Option<&str> = role.as_deref();
    let executor_str: String = task
        .get("executor")
        .and_then(Value::as_str)
        .unwrap_or("either")
        .to_string();
    let mode_str: Option<String> = task
        .get("mode")
        .and_then(Value::as_str)
        .map(str::to_string);
    let priority_str: Option<String> = task
        .get("priority")
        .and_then(Value::as_str)
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
                // Emit BOTH field spellings (see create()): the plugin matches
                // on `assignee_member_id` / `assignee_role` / `id` /
                // `executor`, while older consumers read `assignee` / `role`
                // / `task`. `detail`/`note` are likewise dual-written.
                "doc": "taskx", "task": id, "id": id,
                "title": task["title"],
                "to": nstate,
                "detail": detail, "note": detail,
                "mode": mode_str,
                "assignee": effective_assignee_ref,
                "assignee_member_id": effective_assignee_ref,
                "role": role_ref, "assignee_role": role_ref,
                "priority": priority_str, "executor": executor_str,
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
             WHERE team_id = ?1 AND (payload_json LIKE ?2 OR payload_json LIKE ?3) ORDER BY seq",
        )
        .map_err(|e| format!("db: {e}"))?;
    // Payloads carry BOTH spellings (`task` + `id`); match either so log()
    // keeps working regardless of which key a future emitter writes.
    // NOTE: payload_json is stored via serde_json::to_string (compact, no
    // space after `:`), so the needles must NOT contain a space (issue #29:
    // the original `": "` patterns never matched anything, so this test
    // failed from the day it was introduced and CI never caught it).
    let needle_task = format!("%\"task\":\"{id}\"%");
    let needle_id = format!("%\"id\":\"{id}\"%");
    let rows: Vec<Value> = stmt
        .query_map(params![team_id, needle_task, needle_id], |r| {
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
        // 普通成员（非 lead）也能创建任务（真机踩到：contributor 无法给云端 member
        // 派"重启隧道"的任务）；left/denied/pending 的座位则不行。
        let extra = call(&mut c, "task.create", "m-w1", json!({"title":"x", "assignee":"m-w1"})).unwrap();
        assert_eq!(extra["tasks"].as_array().unwrap().len(), 1);
        assert!(call(&mut c, "task.create", "m-gone", json!({"title":"x", "assignee":"m-gone"})).is_err());
    }

    #[test]
    fn open_bid_without_role_any_member_can_claim() {
        // 期望①：不限角色的开放池 —— mode=bid 且无 role，全队任何在册成员先抢先得。
        let mut c = test_conn();
        let out = call(&mut c, "task.create", "m-owner", json!({"title":"open", "mode":"bid"})).unwrap();
        let id = out["tasks"][0].as_str().unwrap().to_string();
        // 非 lead、不同角色也能认领
        assert_eq!(call(&mut c, "task.claim", "m-w2", json!({"id": id})).unwrap()["task"]["state"], "claimed");
        // 已被认领 → 第二个人不能再认领
        assert!(call(&mut c, "task.claim", "m-w1", json!({"id": id})).is_err());
    }

    #[test]
    fn broadcast_without_role_targets_every_active_member() {
        // 期望③：mode=broadcast 且无 role = 广播给全体在册成员（排除 left）。
        let mut c = test_conn();
        c.execute(
            "INSERT INTO members (id, team_id, session_key, display_name, role, state, joined_at)
             VALUES ('m-left','t1','s9','Gone','worker','left','now')",
            [],
        )
        .unwrap();
        let out = call(&mut c, "task.create", "m-owner", json!({"title":"all", "mode":"broadcast"})).unwrap();
        // owner + w1 + w2 = 3（left 不算）
        assert_eq!(out["tasks"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn role_scoped_bid_still_gated_to_role() {
        // 回归：带 --role 的 bid 仍只允许该角色成员认领。
        let mut c = test_conn();
        let out = call(&mut c, "task.create", "m-owner", json!({"title":"rolebid", "role":"worker"})).unwrap();
        let id = out["tasks"][0].as_str().unwrap().to_string();
        c.execute(
            "INSERT INTO members (id, team_id, session_key, display_name, role, state, joined_at)
             VALUES ('m-x','t1','sx','X','other','active','now')",
            [],
        )
        .unwrap();
        assert!(call(&mut c, "task.claim", "m-x", json!({"id": id})).is_err());
        assert_eq!(call(&mut c, "task.claim", "m-w1", json!({"id": id})).unwrap()["task"]["state"], "claimed");
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

    fn last_payload(conn: &Connection, event_type: &str) -> Value {
        let raw: String = conn
            .query_row(
                "SELECT payload_json FROM events WHERE team_id = 't1' AND type = ?1 ORDER BY seq DESC LIMIT 1",
                rusqlite::params![event_type],
                |r| r.get(0),
            )
            .unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    #[test]
    fn task_event_payload_carries_both_field_spellings() {
        let mut c = test_conn();

        // Direct task: create emits both spellings for the plugin auto path.
        let out = call(
            &mut c,
            "task.create",
            "m-owner",
            json!({"title":"alias me", "assignee":"m-w1", "executor":"either"}),
        )
        .unwrap();
        let id = out["tasks"][0].as_str().unwrap().to_string();
        let created = last_payload(&c, "doc.created");
        assert_eq!(created["doc"], "taskx");
        assert_eq!(created["task"], created["id"]);
        assert_eq!(created["id"].as_str().unwrap(), id);
        assert_eq!(created["assignee"], created["assignee_member_id"]);
        assert_eq!(created["assignee_member_id"].as_str().unwrap(), "m-w1");
        assert_eq!(created["role"], created["assignee_role"]);
        assert!(created.get("executor").and_then(Value::as_str).is_some());
        assert_eq!(created["detail"], Value::Null);

        // Transition (ack/update) keeps the aliases so state-change pushes
        // are not silently dropped by the plugin; detail/note stay in sync.
        call(&mut c, "task.ack", "m-w1", json!({"id": id})).unwrap();
        let acked = last_payload(&c, "doc.acknowledged");
        assert_eq!(acked["task"].as_str().unwrap(), id);
        assert_eq!(acked["id"].as_str().unwrap(), id);
        assert_eq!(acked["assignee_member_id"].as_str().unwrap(), "m-w1");
        assert_eq!(acked["assignee"], acked["assignee_member_id"]);
        assert_eq!(acked["role"], acked["assignee_role"]);
        assert_eq!(acked["detail"], acked["note"]);

        call(&mut c, "task.update", "m-w1", json!({"id": id, "progress":"started"})).unwrap();
        let updated = last_payload(&c, "doc.updated");
        assert_eq!(updated["assignee_member_id"].as_str().unwrap(), "m-w1");
        assert_eq!(updated["note"].as_str().unwrap(), "started");

        // Bid task: claim transitions the assignee from NULL to the claimer;
        // the emitted claim event must already carry the new assignee alias.
        let out = call(
            &mut c,
            "task.create",
            "m-owner",
            json!({"title":"contest", "role":"worker", "mode":"bid"}),
        )
        .unwrap();
        let bid = out["tasks"][0].as_str().unwrap().to_string();
        let bid_created = last_payload(&c, "doc.created");
        assert_eq!(bid_created["assignee_role"].as_str().unwrap(), "worker");
        assert_eq!(bid_created["role"], bid_created["assignee_role"]);
        call(&mut c, "task.claim", "m-w1", json!({"id": bid})).unwrap();
        let claimed = last_payload(&c, "doc.claimed");
        assert_eq!(claimed["assignee_member_id"].as_str().unwrap(), "m-w1");
        assert_eq!(claimed["assignee"], claimed["assignee_member_id"]);

        // task.log finds events via either `task` or `id` key.
        let logged = call(&mut c, "task.log", "m-w1", json!({"id": id})).unwrap();
        assert!(logged["events"].as_array().unwrap().len() >= 3);
    }

    #[test]
    fn task_ids_are_scoped_per_team() {
        // issue #13: two teams on one server may each own `task-1`.
        let mut c = test_conn();
        c.execute_batch(
            "INSERT INTO teams (id,name,owner_member_id,state,invite_token,created_at,updated_at)
             VALUES ('t2','test2','m-owner2','active','tok2','now','now');
             INSERT INTO members (id,team_id,session_key,display_name,role,state,joined_at)
             VALUES ('m-owner2','t2','s9','Owner2','owner','active','now');",
        )
        .unwrap();
        let a = rpc("task.create", &json!({"title":"A","assignee":"m-w1"}), &mut c, "t1", "m-owner").unwrap();
        assert_eq!(a["tasks"][0], json!("task-1"));
        // Before the scoped PK this second insert hit the global PK → raw UNIQUE.
        let b = rpc("task.create", &json!({"title":"B","assignee":"m-owner2"}), &mut c, "t2", "m-owner2").unwrap();
        assert_eq!(b["tasks"][0], json!("task-1"));
    }
}
