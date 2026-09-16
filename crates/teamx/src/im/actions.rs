//! IM write-action execution body (P2, design §5.3).
//!
//! Executes the four frozen write actions: approve/deny (member approval) and
//! verify/reject (task completion). Every path resolves the actor from the
//! binding table (never trusts caller-provided member_id) and checks `is_lead`.
//!
//! Idempotency: same `im_msg_id` is only processed once (via `im_seen`).
//! Audit: every action emits an event with `source`, `im_user`, `im_msg_id`.

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::{db, events, server_tasks, state};
use super::{binding, whitelist};

/// Top-level dispatcher for `im.*` write-action RPCs.
///
/// Called from `im::rpc` (which is called from `serve.rs` dispatch).
/// `platform`/`im_user_id` come from the gateway delegation (or CLI path);
/// `team_id` is resolved by the certificate.
pub fn handle(
    conn: &mut Connection,
    method: &str,
    args: &Value,
    platform: &str,
    im_user_id: &str,
    team_id: &str,
) -> Result<Value, String> {
    // Resolve actor from binding (never trust caller-provided member_id).
    let actor_member_id =
        binding::active_binding_member(conn, platform, im_user_id, team_id)?.ok_or_else(|| {
            format!("{method}: IM identity not bound to any member in team {team_id}")
        })?;
    handle_as_member(conn, method, args, &actor_member_id, platform, im_user_id, team_id)
}

/// Execute one write action where the actor is ALREADY resolved (member-cert
/// path, or the interactive-card flow whose token carries the actor). The
/// whitelist → role → idempotency → execution → audit pipeline is identical.
pub fn handle_as_member(
    conn: &mut Connection,
    method: &str,
    args: &Value,
    actor_member_id: &str,
    platform: &str,
    im_user_id: &str,
    team_id: &str,
) -> Result<Value, String> {
    // 1. Whitelist gate (fail-closed: unknown actions are refused).
    let min_role = whitelist::min_role_for(conn, method)
        .map_err(|e| format!("whitelist: {e}"))?
        .ok_or_else(|| format!("{method}: not whitelisted"))?;

    // 3. Role gate: "lead" requires owner or co-lead.
    if min_role == "lead"
        && !binding::is_lead(conn, team_id, actor_member_id)?
    {
        let reason = format!(
            "{method}: actor {actor_member_id} is not a lead (min_role={min_role})"
        );
        audit_rejection(conn, team_id, platform, im_user_id, args, &reason)?;
        return Err(reason);
    }

    // 4. Idempotency: same im_msg_id → skip (gateway already deduped HMAC
    //    nonce; this catches replay at the action level).
    if let Some(im_msg_id) = args.get("im_msg_id").and_then(Value::as_str) {
        if !im_msg_id.is_empty() {
            let already = conn
                .query_row(
                    "SELECT 1 FROM im_seen WHERE platform = ?1 AND nonce = ?2 AND team_id = ?3",
                    params![platform, im_msg_id, team_id],
                    |_| Ok(true),
                )
                .unwrap_or(false);
            if already {
                return Ok(json!({ "ok": true, "idempotent": true, "action": method }));
            }
        }
    }

    // 5. Dispatch the action.
    let result = match method {
        "im.approve" => action_approve(conn, team_id, actor_member_id, args),
        "im.deny" => action_deny(conn, team_id, actor_member_id, args),
        "im.verify" | "im.reject" => {
            // Delegate to server_tasks (task.verify / task.reject).
            let task_method = format!("task.{}", method.strip_prefix("im.").unwrap());
            server_tasks::rpc(&task_method, args, conn, team_id, actor_member_id)
        }
        other => return Err(format!("{other}: unknown write action")),
    };

    // 6. Audit + record im_msg_id for idempotency.
    match &result {
        Ok(val) => {
            audit_event(conn, team_id, platform, im_user_id, args, "im.accepted", val)?;
            record_im_msg_id(conn, platform, im_user_id, team_id, args)?;
        }
        Err(e) => {
            audit_rejection(conn, team_id, platform, im_user_id, args, e)?;
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Action implementations
// ---------------------------------------------------------------------------

fn action_approve(
    conn: &mut Connection,
    team_id: &str,
    actor_member_id: &str,
    args: &Value,
) -> Result<Value, String> {
    let target_id = args
        .get("member_id")
        .and_then(Value::as_str)
        .ok_or("im.approve: `member_id` is required")?;

    let (current_state, display_name): (String, String) = conn
        .query_row(
            "SELECT state, display_name FROM members WHERE id = ?1 AND team_id = ?2",
            params![target_id, team_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| format!("im.approve: member not found: {e}"))?;

    if current_state != "pending" {
        return Err(format!(
            "im.approve: member {target_id} is not pending (state: {current_state})"
        ));
    }

    let from = state::MemberState::Pending;
    let to = state::member_transition(from, &state::Action::Approve)
        .map_err(|e| format!("im.approve: transition failed: {e}"))?;
    let to_s = to.as_str();

    db::with_write(conn, |tx| {
        tx.execute(
            "UPDATE members SET state = ?1 WHERE id = ?2",
            params![to_s, target_id],
        )?;
        events::emit(
            tx,
            team_id,
            Some(actor_member_id),
            "membership.approved",
            Some(&json!({ "member_id": target_id, "display_name": display_name })),
        )?;
        Ok(())
    })
    .map_err(|e| format!("im.approve: {e}"))?;

    // Auto-grant write access to all team git repos (same as CLI approve).
    if let Ok(repos) = crate::git_service::list_repos(conn, team_id) {
        for r in repos {
            let _ = crate::git_service::grant_permission(
                conn,
                &r.id,
                target_id,
                crate::git_service::PERM_WRITE,
                actor_member_id,
            );
        }
    }

    Ok(json!({ "ok": true, "action": "approved", "member_id": target_id }))
}

fn action_deny(
    conn: &mut Connection,
    team_id: &str,
    actor_member_id: &str,
    args: &Value,
) -> Result<Value, String> {
    let target_id = args
        .get("member_id")
        .and_then(Value::as_str)
        .ok_or("im.deny: `member_id` is required")?;

    let (current_state, display_name): (String, String) = conn
        .query_row(
            "SELECT state, display_name FROM members WHERE id = ?1 AND team_id = ?2",
            params![target_id, team_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| format!("im.deny: member not found: {e}"))?;

    if current_state != "pending" {
        return Err(format!(
            "im.deny: member {target_id} is not pending (state: {current_state})"
        ));
    }

    let from = state::MemberState::Pending;
    let to = state::member_transition(from, &state::Action::Deny)
        .map_err(|e| format!("im.deny: transition failed: {e}"))?;
    let to_s = to.as_str();

    db::with_write(conn, |tx| {
        tx.execute(
            "UPDATE members SET state = ?1 WHERE id = ?2",
            params![to_s, target_id],
        )?;
        events::emit(
            tx,
            team_id,
            Some(actor_member_id),
            "membership.denied",
            Some(&json!({ "member_id": target_id, "display_name": display_name })),
        )?;
        Ok(())
    })
    .map_err(|e| format!("im.deny: {e}"))?;

    Ok(json!({ "ok": true, "action": "denied", "member_id": target_id }))
}

// ---------------------------------------------------------------------------
// Audit helpers
// ---------------------------------------------------------------------------

fn audit_event(
    conn: &mut Connection,
    team_id: &str,
    platform: &str,
    im_user_id: &str,
    args: &Value,
    event_type: &str,
    result: &Value,
) -> Result<(), String> {
    let im_msg_id = args.get("im_msg_id").and_then(Value::as_str).unwrap_or("");
    let payload = json!({
        "source": format!("im:{platform}"),
        "im_user": im_user_id,
        "im_msg_id": im_msg_id,
        "result": result,
    });
    db::with_write(conn, |tx| {
        events::emit(tx, team_id, None, event_type, Some(&payload))?;
        Ok(())
    })
    .map_err(|e| format!("audit: {e}"))
}

fn audit_rejection(
    conn: &mut Connection,
    team_id: &str,
    platform: &str,
    im_user_id: &str,
    args: &Value,
    reason: &str,
) -> Result<(), String> {
    let im_msg_id = args.get("im_msg_id").and_then(Value::as_str).unwrap_or("");
    let payload = json!({
        "source": format!("im:{platform}"),
        "im_user": im_user_id,
        "im_msg_id": im_msg_id,
        "reason": reason,
    });
    db::with_write(conn, |tx| {
        events::emit(tx, team_id, None, "im.rejected", Some(&payload))?;
        Ok(())
    })
    .map_err(|e| format!("audit: {e}"))
}

fn record_im_msg_id(
    conn: &mut Connection,
    platform: &str,
    im_user_id: &str,
    team_id: &str,
    args: &Value,
) -> Result<(), String> {
    if let Some(im_msg_id) = args.get("im_msg_id").and_then(Value::as_str) {
        if !im_msg_id.is_empty() {
            let now = chrono::Utc::now().timestamp();
            conn.execute(
                "INSERT OR IGNORE INTO im_seen (platform, nonce, team_id, im_user_id, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![platform, im_msg_id, team_id, im_user_id, now],
            )
            .map_err(|e| format!("record im_msg_id: {e}"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn setup_team(conn: &Connection) -> String {
        let team_id = "t-act";
        conn.execute(
            "INSERT INTO teams (id, name, owner_member_id, state, invite_token, created_at, updated_at)
             VALUES (?1, 'T', 'm-owner', 'active', 'tok', 'now', 'now')",
            params![team_id],
        )
        .unwrap();
        team_id.to_string()
    }

    fn setup_members(conn: &Connection, team_id: &str) {
        conn.execute(
            "INSERT INTO members (id, team_id, session_key, display_name, role, state, is_lead, joined_at)
             VALUES ('m-owner', ?1, 's-owner', 'Owner', 'owner', 'active', 1, 'now')",
            params![team_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO members (id, team_id, session_key, display_name, role, state, joined_at)
             VALUES ('m-pending', ?1, 's-pending', 'Pending', 'contributor', 'pending', 'now')",
            params![team_id],
        )
        .unwrap();
    }

    fn setup_binding(conn: &Connection, team_id: &str, member_id: &str, platform: &str, im_user_id: &str) {
        conn.execute(
            "INSERT INTO im_bindings (platform, im_user_id, member_id, team_id, bound_at, bound_by)
             VALUES (?1, ?2, ?3, ?4, 'now', ?3)",
            params![platform, im_user_id, member_id, team_id],
        )
        .unwrap();
    }

    #[test]
    fn approve_pending_member_as_lead() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        db::migrate(&conn).unwrap();
        let team_id = setup_team(&conn);
        setup_members(&conn, &team_id);
        setup_binding(&conn, &team_id, "m-owner", "feishu", "u-owner");

        let args = json!({ "member_id": "m-pending", "im_msg_id": "msg-001" });
        let result = handle(&mut conn, "im.approve", &args, "feishu", "u-owner", &team_id).unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["action"], "approved");
        assert_eq!(result["member_id"], "m-pending");

        let state: String = conn
            .query_row("SELECT state FROM members WHERE id = 'm-pending'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(state, "active");
    }

    #[test]
    fn deny_pending_member_as_lead() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        db::migrate(&conn).unwrap();
        let team_id = setup_team(&conn);
        setup_members(&conn, &team_id);
        setup_binding(&conn, &team_id, "m-owner", "feishu", "u-owner");

        let args = json!({ "member_id": "m-pending", "im_msg_id": "msg-002" });
        let result = handle(&mut conn, "im.deny", &args, "feishu", "u-owner", &team_id).unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["action"], "denied");

        let state: String = conn
            .query_row("SELECT state FROM members WHERE id = 'm-pending'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(state, "denied");
    }

    #[test]
    fn non_lead_rejected() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        db::migrate(&conn).unwrap();
        let team_id = setup_team(&conn);
        setup_members(&conn, &team_id);
        conn.execute(
            "INSERT INTO members (id, team_id, session_key, display_name, role, state, joined_at)
             VALUES ('m-member', ?1, 's-member', 'M', 'contributor', 'active', 'now')",
            params![team_id],
        )
        .unwrap();
        setup_binding(&conn, &team_id, "m-member", "feishu", "u-member");

        let args = json!({ "member_id": "m-pending" });
        let e = handle(&mut conn, "im.approve", &args, "feishu", "u-member", &team_id)
            .unwrap_err();
        assert!(e.contains("not a lead"), "{e}");

        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM events WHERE type = 'im.rejected'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn unknown_action_fail_closed() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        db::migrate(&conn).unwrap();
        let team_id = setup_team(&conn);
        setup_members(&conn, &team_id);
        setup_binding(&conn, &team_id, "m-owner", "feishu", "u-owner");

        let args = json!({});
        let e = handle(&mut conn, "im.destroy_team", &args, "feishu", "u-owner", &team_id)
            .unwrap_err();
        assert!(e.contains("not whitelisted"), "{e}");
    }

    #[test]
    fn idempotent_same_im_msg_id() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        db::migrate(&conn).unwrap();
        let team_id = setup_team(&conn);
        setup_members(&conn, &team_id);
        setup_binding(&conn, &team_id, "m-owner", "feishu", "u-owner");

        let args = json!({ "member_id": "m-pending", "im_msg_id": "msg-dup" });
        let r1 = handle(&mut conn, "im.approve", &args, "feishu", "u-owner", &team_id).unwrap();
        assert_eq!(r1["ok"], true);
        assert_eq!(r1["idempotent"], Value::Null);

        let r2 = handle(&mut conn, "im.approve", &args, "feishu", "u-owner", &team_id).unwrap();
        assert_eq!(r2["ok"], true);
        assert_eq!(r2["idempotent"], true);
    }

    #[test]
    fn audit_events_have_source_and_im_user() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        db::migrate(&conn).unwrap();
        let team_id = setup_team(&conn);
        setup_members(&conn, &team_id);
        setup_binding(&conn, &team_id, "m-owner", "feishu", "u-owner");

        let args = json!({ "member_id": "m-pending", "im_msg_id": "msg-audit" });
        handle(&mut conn, "im.approve", &args, "feishu", "u-owner", &team_id).unwrap();

        let payload: String = conn
            .query_row(
                "SELECT payload_json FROM events WHERE type = 'im.accepted' ORDER BY seq DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let v: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["source"], "im:feishu");
        assert_eq!(v["im_user"], "u-owner");
        assert_eq!(v["im_msg_id"], "msg-audit");
    }

    #[test]
    fn non_pending_member_rejected() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        db::migrate(&conn).unwrap();
        let team_id = setup_team(&conn);
        setup_members(&conn, &team_id);
        setup_binding(&conn, &team_id, "m-owner", "feishu", "u-owner");

        let args = json!({ "member_id": "m-owner" });
        let e = handle(&mut conn, "im.approve", &args, "feishu", "u-owner", &team_id)
            .unwrap_err();
        assert!(e.contains("not pending"), "{e}");
    }

    #[test]
    fn unbound_im_identity_rejected() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        db::migrate(&conn).unwrap();
        let team_id = setup_team(&conn);
        setup_members(&conn, &team_id);

        let args = json!({ "member_id": "m-pending" });
        let e = handle(&mut conn, "im.approve", &args, "feishu", "u-ghost", &team_id)
            .unwrap_err();
        assert!(e.contains("not bound"), "{e}");
    }
}
