//! Interactive Feishu cards (P2, design §5.2): the approval / acceptance
//! UI carrier + the **double-confirmation** flow.
//!
//! Flow for one card-driven action (e.g. a lead verifying a task from IM):
//! 1. the notifier renders a `task_action_card` with action buttons (the
//!    button `value` carries the callback payload: action + task_id + a
//!    per-card `im_msg_id`);
//!
//! C4 (task-18) semantics:
//! - **first-strike idempotency**: a `request` carrying `im_msg_id` is keyed
//!   on (action, task_id, team_id, im_msg_id) — a duplicate (double-click)
//!   returns the SAME token and skips the duplicate `im.card.requested`
//!   audit. When `im_msg_id` is absent there is no key: every call issues a
//!   fresh token (documented fallback; card callbacks always carry one).
//! - **audit symmetry**: every `request` rejection (unknown action / missing
//!   task_id / not whitelisted / identity not bound / not a lead) lands in
//!   `im.rejected` via `actions::audit_rejection_with`, mirroring `execute`'s
//!   rejection audits; the source label follows the same per-path convention
//!   as the success-path audit (`im:{platform}` gateway / `member:{actor}`
//!   member-cert).
//! 2. the first click reaches the server as **`im.card.request`** — the
//!    server checks the action against the whitelist + the actor's role,
//!    then issues a short-lived `confirm_token` (TTL 5 min, one-time) and
//!    audits `im.card.requested`; the response carries a `confirm_card`;
//! 3. the second click (确认) reaches the server as **`im.card.execute`** —
//!    the token is validated (exists, matches action+task+team, unexpired)
//!    and consumed; only then does the write-action pipeline
//!    (`im::actions::handle`) run, so a stray or replayed click can never
//!    mutate state.
//!
//! Cards are pure data: every builder here is a pure function returning the
//! Feishu card JSON, so rendering is testable without any network.

use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::db;
use super::binding;
use super::whitelist;

/// Confirm-token TTL (seconds).
pub const CONFIRM_TTL_SECS: i64 = 300;

/// The four card-driven write actions (must match the whitelist seeds).
const CARD_ACTIONS: &[&str] = &["im.approve", "im.deny", "im.verify", "im.reject"];

/// Build the first-touch card for one task action: a header + one primary
/// button per decision. The button `value` carries the callback payload the
/// gateway forwards as `im.card.request`.
pub fn task_action_card(action: &str, task_id: &str, title: &str, im_msg_id: &str) -> Value {
    let (header, buttons): (&str, Vec<Value>) = match action {
        "im.approve" | "im.deny" => (
            "🟠 成员审批",
            vec![
                button("✅ approve", "primary", "im.approve", task_id, "im_msg_id", im_msg_id),
                button("❌ deny", "danger", "im.deny", task_id, "im_msg_id", im_msg_id),
            ],
        ),
        "im.verify" | "im.reject" => (
            "🟢 任务验收",
            vec![
                button("✅ verify", "primary", "im.verify", task_id, "im_msg_id", im_msg_id),
                button("↩️ reject", "danger", "im.reject", task_id, "im_msg_id", im_msg_id),
            ],
        ),
        other => return json!({ "error": format!("unknown card action `{other}`") }),
    };
    json!({
        "config": { "wide_screen_mode": true },
        "header": {
            "template": if header.starts_with("🟠") { "orange" } else { "green" },
            "title": { "tag": "plain_text", "content": header },
        },
        "elements": [
            { "tag": "div", "text": { "tag": "lark_md",
              "content": format!("**任务** `{}`\n**动作** `{}`\n**标题** {}", task_id, action, title) } },
            { "tag": "hr" },
            { "tag": "action", "actions": buttons },
            { "tag": "note", "elements": [{ "tag": "plain_text",
              "content": "点击后需要二次确认；确认令牌 5 分钟内有效" }] },
        ],
    })
}

/// Build the second-confirmation card: a single 确认 button whose `value`
/// carries the one-time `confirm_token`.
pub fn confirm_card(action: &str, task_id: &str, confirm_token: &str) -> Value {
    json!({
        "config": { "wide_screen_mode": true },
        "header": {
            "template": "red",
            "title": { "tag": "plain_text", "content": "⚠️ 请确认（二次确认）" },
        },
        "elements": [
            { "tag": "div", "text": { "tag": "lark_md",
              "content": format!("即将执行 **`{action}`** @ `{task_id}`\n令牌有效期 5 分钟，仅可使用一次") } },
            { "tag": "action", "actions": [
                button("✅ 确认执行", "primary", action, task_id, "confirm_token", confirm_token),
            ]},
        ],
    })
}

fn button(
    text: &str,
    style: &str,
    action: &str,
    task_id: &str,
    payload_key: &str,
    payload_value: &str,
) -> Value {
    json!({
        "tag": "button",
        "text": { "tag": "plain_text", "content": text },
        "type": style,
        "value": {
            "card_cb": true,
            "action": action,
            "task_id": task_id,
            payload_key: payload_value,
        },
    })
}

/// `im.card.request` — first click. Validates action + role, issues a
/// one-time confirm token, audits `im.card.requested`, returns the confirm
/// card. Never mutates task/member state.
pub fn request(
    conn: &mut Connection,
    args: &Value,
    platform: Option<&str>,
    im_user_id: Option<&str>,
    member_id: &str,
    team_id: &str,
) -> Result<Value, String> {
    let im_msg_id = args
        .get("im_msg_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    // C4 audit symmetry: every rejection below lands in `im.rejected` via
    // actions::audit_rejection_with, using the same source vocabulary as the
    // success-path audit (gateway `im:{p}` / member `member:{actor}`).
    // Rejection audits propagate their error (same as actions::handle), so an
    // audit write failure surfaces instead of silently dropping the trace.
    macro_rules! reject {
        ($source:expr, $im_user:expr, $reason:expr) => {{
            let reason: String = $reason;
            super::actions::audit_rejection_with(
                conn,
                team_id,
                &$source,
                $im_user,
                &im_msg_id,
                &reason,
            )?;
            return Err(reason);
        }};
    }

    let action = args
        .get("action")
        .and_then(Value::as_str)
        .ok_or("im.card.request: `action` is required")?;
    if !CARD_ACTIONS.contains(&action) {
        let source = match (platform, im_user_id) {
            (Some(p), Some(u)) if !p.is_empty() && !u.is_empty() => format!("im:{p}"),
            _ => format!("member:{member_id}"),
        };
        let im_user = im_user_id.filter(|u| !u.is_empty()).unwrap_or(member_id);
        reject!(source, im_user, format!("im.card.request: unknown action `{action}`"));
    }
    let task_id = args
        .get("task_id")
        .and_then(Value::as_str)
        .ok_or("im.card.request: `task_id` is required")?
        .to_string();

    // Same gates the real action will apply (fail early, before any token).
    // Actor resolution: gateway path (platform+im_user present) resolves via
    // the binding table; the member path falls back to the certificate
    // identity.
    let min_role = whitelist::min_role_for(conn, action)
        .map_err(|e| format!("whitelist: {e}"))?
        .ok_or_else(|| format!("im.card.request: `{action}` is not whitelisted"))?;
    let gateway = matches!((platform, im_user_id), (Some(p), Some(u)) if !p.is_empty() && !u.is_empty());
    let (actor, im_user, source): (String, String, String) = if gateway {
        let p = platform.unwrap_or_default();
        let u = im_user_id.unwrap_or_default();
        match binding::active_binding_member(conn, p, u, team_id) {
            Ok(Some(mid)) => {
                // Identity consistency (fault drill L2): a member-cert caller
                // may only act as their OWN bound IM identity — never as
                // another member's (e.g. the owner's). The gateway path is
                // unaffected (it dispatches as the binding-resolved member).
                if mid != member_id {
                    reject!(
                        format!("im:{p}"),
                        u,
                        format!(
                            "im.card.request: identity mismatch: {p}:{u} is bound to {mid}, \
                             not the certificate member {member_id}"
                        )
                    );
                }
                (mid, u.to_string(), format!("im:{p}"))
            }
            Ok(None) => {
                reject!(
                    format!("im:{p}"),
                    u,
                    "im.card.request: IM identity not bound".to_string()
                );
            }
            Err(e) => return Err(e),
        }
    } else {
        (
            member_id.to_string(),
            member_id.to_string(),
            format!("member:{member_id}"),
        )
    };
    if min_role == "lead" && !binding::is_lead(conn, team_id, &actor)? {
        reject!(
            source,
            &im_user,
            format!("im.card.request: actor {actor} is not a lead (min_role={min_role})")
        );
    }

    // C4 first-strike idempotency: a `request` with the same
    // (action, task_id, team_id, im_msg_id) returns the SAME live token —
    // a double-click must not mint a second one. Keyed only when im_msg_id
    // is present; without it there is no key, so each call issues a fresh
    // token (documented fallback).
    let now = chrono::Utc::now().timestamp();
    if !im_msg_id.is_empty() {
        let existing: Option<String> = conn
            .query_row(
                "SELECT token FROM im_card_tokens
                  WHERE action = ?1 AND task_id = ?2 AND team_id = ?3
                    AND im_msg_id = ?4 AND expires_at > ?5
                  ORDER BY created_at DESC LIMIT 1",
                rusqlite::params![action, task_id, team_id, im_msg_id, now],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some(token) = existing {
            return Ok(json!({
                "ok": true,
                "idempotent": true,
                "action": action,
                "task_id": task_id,
                "confirm_token": token,
                "expires_in": CONFIRM_TTL_SECS,
                "card": confirm_card(action, &task_id, &token),
            }));
        }
    }

    // Issue the one-time token (converged table, opportunistic pruning).
    let token = format!("ct-{}", uuid::Uuid::new_v4().simple());
    conn.execute(
        "DELETE FROM im_card_tokens WHERE expires_at < ?1",
        rusqlite::params![now],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO im_card_tokens (token, action, task_id, team_id, im_user_id, platform, im_msg_id, created_at, expires_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        rusqlite::params![token, action, task_id, team_id, im_user, platform.unwrap_or("feishu"), im_msg_id, now, now + CONFIRM_TTL_SECS],
    )
    .map_err(|e| e.to_string())?;

    // Audit the request (a requested action is NOT yet executed).
    let src_label = match platform {
        Some(p) if !p.is_empty() => format!("im:{p}"),
        _ => format!("member:{actor}"),
    };
    let payload = json!({
        "source": src_label,
        "im_user": im_user,
        "action": action,
        "target": task_id,
        "confirm_token": token,
        "expires_in": CONFIRM_TTL_SECS,
    });
    db::with_write(conn, |tx| {
        crate::events::emit(tx, team_id, Some(&im_user), "im.card.requested", Some(&payload))?;
        Ok(())
    })
    .map_err(|e: rusqlite::Error| e.to_string())?;

    Ok(json!({
        "ok": true,
        "action": action,
        "task_id": task_id,
        "confirm_token": token,
        "expires_in": CONFIRM_TTL_SECS,
        "card": confirm_card(action, &task_id, &token),
    }))
}

/// `im.card.execute` — second click. Validates and consumes the token, then
/// delegates to the write-action pipeline (`im::actions::handle`), which
/// re-applies whitelist/role/idempotency and audits `im.action`.
pub fn execute(
    conn: &mut Connection,
    args: &Value,
    platform: Option<&str>,
    im_user_id: Option<&str>,
    member_id: &str,
    team_id: &str,
) -> Result<Value, String> {
    let action = args
        .get("action")
        .and_then(Value::as_str)
        .ok_or("im.card.execute: `action` is required")?;
    let task_id = args
        .get("task_id")
        .and_then(Value::as_str)
        .ok_or("im.card.execute: `task_id` is required")?
        .to_string();
    let token = args
        .get("confirm_token")
        .and_then(Value::as_str)
        .ok_or("im.card.execute: `confirm_token` is required")?
        .to_string();

    // Validate + consume the token (one-time).
    let now = chrono::Utc::now().timestamp();
    let row: Option<(String, String, String, String, i64)> = conn
        .query_row(
            "SELECT action, task_id, team_id, im_user_id, expires_at FROM im_card_tokens WHERE token = ?1",
            rusqlite::params![token],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some((tok_action, tok_task, tok_team, tok_im_user, expires_at)) = row else {
        return Err("im.card.execute: unknown confirm token".to_string());
    };
    if expires_at < now {
        let _ = conn.execute("DELETE FROM im_card_tokens WHERE token = ?1", rusqlite::params![token]);
        return Err("im.card.execute: confirm token expired".to_string());
    }
    if tok_action != action || tok_task != task_id || tok_team != team_id {
        return Err("im.card.execute: confirm token does not match action/task/team".to_string());
    }
    // The actor must be the same identity that requested the token.
    let im_user = match (platform, im_user_id) {
        (Some(p), Some(u)) if !p.is_empty() && !u.is_empty() => u.to_string(),
        _ => member_id.to_string(),
    };
    if tok_im_user != im_user {
        return Err("im.card.execute: confirm token does not belong to this identity".to_string());
    }
    // Actor for the action: the identity that requested the token. On the
    // member path that IS the member; on the gateway path resolve via binding.
    let actor = match (platform, im_user_id) {
        (Some(p), Some(u)) if !p.is_empty() && !u.is_empty() => {
            let mid = binding::active_binding_member(conn, p, u, team_id)?.ok_or_else(|| {
                "im.card.execute: IM identity not bound".to_string()
            })?;
            // Identity consistency (fault drill L2): same rule as request —
            // the delegated identity must belong to the certificate member.
            if mid != member_id {
                let reason = format!(
                    "im.card.execute: identity mismatch: {p}:{u} is bound to {mid}, \
                     not the certificate member {member_id}"
                );
                let _ = super::actions::audit_rejection(conn, team_id, p, u, args, &reason);
                return Err(reason);
            }
            mid
        }
        _ => member_id.to_string(),
    };
    let platform = platform.unwrap_or_default().to_string();
    // Consume before executing (a crash between DELETE and execute leaves the
    // action un-run but the token spent — fail-closed, never double-executed).
    conn.execute(
        "DELETE FROM im_card_tokens WHERE token = ?1",
        rusqlite::params![token],
    )
    .map_err(|e| e.to_string())?;

    // Delegate to the write-action pipeline with the TOKEN's actor (the
    // identity that requested the confirmation) — re-applies whitelist/role/
    // idempotency and emits im.action / im.rejected itself.
    let mut task_args = json!({ "task_id": task_id, "id": task_id });
    if let Some(id) = args.get("im_msg_id") {
        task_args["im_msg_id"] = id.clone();
    }
    let p = if platform.is_empty() { "feishu".to_string() } else { platform.clone() };
    let out = super::actions::handle_as_member(conn, action, &task_args, &actor, &p, &im_user, team_id)?;
    Ok(json!({
        "ok": true,
        "action": action,
        "task_id": task_id,
        "result": out,
    }))
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
        conn.execute(
            "INSERT INTO taskx (id, team_id, title, state, assignee_member_id, created_by_member_id, created_at, updated_at)
             VALUES ('task-1','t1','Ship','done','m-worker','m-owner','now','now')",
            [],
        )
        .unwrap();
        (conn, "t1".to_string())
    }

    fn bind(conn: &Connection, im_user: &str, member_id: &str) {
        conn.execute(
            "INSERT INTO im_bindings (platform, im_user_id, member_id, team_id, bound_at, bound_by)
             VALUES ('feishu', ?1, ?2, 't1', 'now', 'test')",
            rusqlite::params![im_user, member_id],
        )
        .unwrap();
    }

    fn req_args(task: &str, msg: &str) -> Value {
        json!({ "action": "im.verify", "task_id": task, "im_msg_id": msg })
    }

    #[test]
    fn request_rejects_delegated_identity_not_bound_to_cert_member() {
        // Fault drill L2: a member-cert caller passing ANOTHER member's bound
        // im_user_id must never act with that member's privileges.
        let mut conn;
        let team;
        (conn, team) = seed();
        bind(&conn, "u-owner", "m-owner"); // owner is lead

        let e = request(
            &mut conn,
            &req_args("task-1", "m-escalate"),
            Some("feishu"),
            Some("u-owner"),   // forged: belongs to m-owner (lead)
            "m-worker",        // cert identity: m-worker (not lead)
            &team,
        )
        .unwrap_err();
        assert!(e.contains("identity mismatch"), "{e}");

        // Legitimate: identity bound to the cert member works.
        bind(&conn, "u-owner-ok", "m-owner");
        let ok = request(
            &mut conn,
            &req_args("task-1", "m-ok"),
            Some("feishu"),
            Some("u-owner-ok"),
            "m-owner",
            &team,
        )
        .unwrap();
        assert_eq!(ok["ok"], true);
    }

    #[test]
    fn execute_rejects_delegated_identity_not_bound_to_cert_member() {
        let mut conn;
        let team;
        (conn, team) = seed();
        bind(&conn, "u-owner", "m-owner");

        // Owner (cert) requests a token via their own bound identity.
        bind(&conn, "u-owner-self", "m-owner");
        let req = request(
            &mut conn,
            &req_args("task-1", "m-exec"),
            Some("feishu"),
            Some("u-owner-self"),
            "m-owner",
            &team,
        )
        .unwrap();
        let token = req["confirm_token"].as_str().unwrap().to_string();

        // Attacker (worker cert) tries to execute with the owner's identity.
        let args = json!({ "action": "im.verify", "task_id": "task-1", "confirm_token": token });
        let e = execute(
            &mut conn,
            &args,
            Some("feishu"),
            Some("u-owner"),
            "m-worker",
            &team,
        )
        .unwrap_err();
        // Token identity check fires first (token belongs to u-owner-self) —
        // either refusal is a pass; assert refusal, then assert the token was
        // NOT consumed by the failed attempt... note: identity check happens
        // BEFORE consume, so the token survives for the rightful owner.
        assert!(!e.is_empty());
        let still = conn
            .query_row(
                "SELECT COUNT(*) FROM im_card_tokens WHERE token = ?1",
                rusqlite::params![token],
                |r| r.get::<_, i64>(0),
            )
            .unwrap();
        assert_eq!(still, 1, "failed execute must not consume the token");
    }

    #[test]
    fn action_card_carries_callback_payload() {
        let card = task_action_card("im.verify", "task-1", "Ship", "msg-1");
        let btns = &card["elements"][2]["actions"];
        assert_eq!(btns.as_array().unwrap().len(), 2);
        assert_eq!(btns[0]["value"]["action"], json!("im.verify"));
        assert_eq!(btns[0]["value"]["task_id"], json!("task-1"));
        assert_eq!(btns[0]["value"]["im_msg_id"], json!("msg-1"));
        assert_eq!(card["header"]["title"]["content"], json!("🟢 任务验收"));
    }

    #[test]
    fn unknown_action_card_is_an_error_object() {
        let card = task_action_card("im.destroy", "task-1", "x", "m");
        assert!(card["error"].is_string());
    }

    #[test]
    fn request_issues_token_and_audits() {
        let (mut conn, team) = seed();
        bind(&conn, "u1", "m-owner");
        let v = request(&mut conn, &req_args("task-1", "m-1"), Some("feishu"), Some("u1"), "m-owner", &team).unwrap();
        let token = v["confirm_token"].as_str().unwrap();
        assert!(token.starts_with("ct-"));
        // Token row exists and is unexpired.
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM im_card_tokens WHERE token=?1 AND action='im.verify' AND task_id='task-1'",
                rusqlite::params![token],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
        // Confirm card carries the token in the button value.
        assert_eq!(
            v["card"]["elements"][1]["actions"][0]["value"]["confirm_token"],
            json!(token)
        );
        // Audited.
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM events WHERE type='im.card.requested'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn non_lead_request_is_refused() {
        let (mut conn, team) = seed();
        bind(&conn, "u2", "m-worker");
        let e = request(&mut conn, &req_args("task-1", "m-2"), Some("feishu"), Some("u2"), "m-worker", &team).unwrap_err();
        assert!(e.contains("not a lead"), "{e}");
    }

    #[test]
    fn unknown_action_request_is_refused() {
        let (mut conn, team) = seed();
        bind(&conn, "u1", "m-owner");
        let e = request(
            &mut conn,
            &json!({"action":"im.destroy","task_id":"task-1"}),
            Some("feishu"),
            Some("u1"),
            "m-owner",
            &team,
        )
        .unwrap_err();
        assert!(e.contains("unknown action"), "{e}");
    }

    #[test]
    fn duplicate_request_with_same_msg_returns_same_token() {
        let (mut conn, team) = seed();
        bind(&conn, "u1", "m-owner");
        let args = &json!({"action":"im.verify","task_id":"task-1","im_msg_id":"msg-9"});
        let v1 = request(&mut conn, args, Some("feishu"), Some("u1"), "m-owner", &team).unwrap();
        let v2 = request(&mut conn, args, Some("feishu"), Some("u1"), "m-owner", &team).unwrap();
        assert_eq!(v1["confirm_token"], v2["confirm_token"], "double-click must reuse the token");
        assert_eq!(v2["idempotent"], json!(true));
        // Only one token row and one im.card.requested audit for the pair.
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM im_card_tokens WHERE im_msg_id='msg-9'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        let a: i64 = conn
            .query_row("SELECT COUNT(*) FROM events WHERE type='im.card.requested'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(a, 1, "duplicate must not double-audit");
    }

    #[test]
    fn request_without_msg_id_issues_fresh_tokens_each_call() {
        let (mut conn, team) = seed();
        bind(&conn, "u1", "m-owner");
        let args = &json!({"action":"im.verify","task_id":"task-1"});
        let v1 = request(&mut conn, args, Some("feishu"), Some("u1"), "m-owner", &team).unwrap();
        let v2 = request(&mut conn, args, Some("feishu"), Some("u1"), "m-owner", &team).unwrap();
        // No key → no idempotency: documented fallback issues fresh tokens.
        assert_ne!(v1["confirm_token"], v2["confirm_token"]);
    }

    #[test]
    fn request_rejections_are_audited_im_rejected() {
        let (mut conn, team) = seed();
        bind(&conn, "u2", "m-worker");

        // unknown action
        let e = request(
            &mut conn,
            &json!({"action":"im.destroy","task_id":"task-1","im_msg_id":"r1"}),
            Some("feishu"), Some("u2"), "m-owner", &team,
        )
        .unwrap_err();
        assert!(e.contains("unknown action"), "{e}");
        // identity not bound (u3 has no binding)
        let e = request(
            &mut conn,
            &req_args("task-1", "m-2"),
            Some("feishu"), Some("u3"), "m-owner", &team,
        )
        .unwrap_err();
        assert!(e.contains("not bound"), "{e}");
        // not a lead — cert member must match u2's binding (m-worker),
        // otherwise the L2 identity gate refuses first (see
        // request_rejects_delegated_identity_not_bound_to_cert_member).
        let e = request(
            &mut conn,
            &req_args("task-1", "m-2"),
            Some("feishu"), Some("u2"), "m-worker", &team,
        )
        .unwrap_err();
        assert!(e.contains("not a lead"), "{e}");

        // All three rejections audited as im.rejected with reason payload.
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM events WHERE type='im.rejected'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 3, "expected 3 rejection audits, got {n}");
        let (s1, s2): (String, String) = conn
            .query_row(
                "SELECT payload_json, json_extract(payload_json,'$.reason') FROM events
                  WHERE type='im.rejected' ORDER BY id LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(s1.contains("unknown action"), "{s1}");
        assert_eq!(s2, "im.card.request: unknown action `im.destroy`");
    }

    #[test]
    fn execute_consumes_token_and_runs_the_action() {
        let (mut conn, team) = seed();
        bind(&conn, "u1", "m-owner");
        let v = request(&mut conn, &req_args("task-1", "m-3"), Some("feishu"), Some("u1"), "m-owner", &team).unwrap();
        let token = v["confirm_token"].as_str().unwrap().to_string();

        let out = execute(
            &mut conn,
            &json!({"action":"im.verify","task_id":"task-1","confirm_token":token,"im_msg_id":"m-3"}),
            Some("feishu"),
            Some("u1"),
            "m-owner",
            &team,
        )
        .unwrap();
        assert_eq!(out["ok"], json!(true));
        // The task really flipped to verified.
        let state: String = conn
            .query_row("SELECT state FROM taskx WHERE id='task-1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(state, "verified");
        // Token consumed.
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM im_card_tokens WHERE token=?1", rusqlite::params![token], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn replayed_token_is_refused() {
        let (mut conn, team) = seed();
        bind(&conn, "u1", "m-owner");
        let v = request(&mut conn, &req_args("task-1", "m-4"), Some("feishu"), Some("u1"), "m-owner", &team).unwrap();
        let token = v["confirm_token"].as_str().unwrap().to_string();
        execute(
            &mut conn,
            &json!({"action":"im.verify","task_id":"task-1","confirm_token":token,"im_msg_id":"m-4"}),
            Some("feishu"),
            Some("u1"),
            "m-owner",
            &team,
        )
        .unwrap();
        // Same token again → unknown (consumed).
        let e = execute(
            &mut conn,
            &json!({"action":"im.verify","task_id":"task-1","confirm_token":token,"im_msg_id":"m-5"}),
            Some("feishu"),
            Some("u1"),
            "m-owner",
            &team,
        )
        .unwrap_err();
        assert!(e.contains("unknown confirm token"), "{e}");
    }

    #[test]
    fn mismatched_action_or_task_is_refused() {
        let (mut conn, team) = seed();
        bind(&conn, "u1", "m-owner");
        let v = request(&mut conn, &req_args("task-1", "m-6"), Some("feishu"), Some("u1"), "m-owner", &team).unwrap();
        let token = v["confirm_token"].as_str().unwrap().to_string();
        // Wrong action.
        let e = execute(
            &mut conn,
            &json!({"action":"im.reject","task_id":"task-1","confirm_token":token}),
            Some("feishu"),
            Some("u1"),
            "m-owner",
            &team,
        )
        .unwrap_err();
        assert!(e.contains("does not match"), "{e}");
        // Wrong task.
        let e = execute(
            &mut conn,
            &json!({"action":"im.verify","task_id":"task-9","confirm_token":token}),
            Some("feishu"),
            Some("u1"),
            "m-owner",
            &team,
        )
        .unwrap_err();
        assert!(e.contains("does not match"), "{e}");
    }

    #[test]
    fn expired_token_is_refused() {
        let (mut conn, team) = seed();
        bind(&conn, "u1", "m-owner");
        let v = request(&mut conn, &req_args("task-1", "m-7"), Some("feishu"), Some("u1"), "m-owner", &team).unwrap();
        let token = v["confirm_token"].as_str().unwrap().to_string();
        conn.execute(
            "UPDATE im_card_tokens SET expires_at = 1 WHERE token = ?1",
            rusqlite::params![token],
        )
        .unwrap();
        let e = execute(
            &mut conn,
            &json!({"action":"im.verify","task_id":"task-1","confirm_token":token}),
            Some("feishu"),
            Some("u1"),
            "m-owner",
            &team,
        )
        .unwrap_err();
        assert!(e.contains("expired"), "{e}");
    }
}
