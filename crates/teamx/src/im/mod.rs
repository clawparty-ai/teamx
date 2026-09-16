//! IM access — the **control plane** for remote team operation & management.
//!
//! Layout (locked by `docs/design/im-access-requirements.md` §5.4):
//! `im/{config,types,providers/*}` with `gateway` / `notifier` to follow.
//!
//! Phase-1 (P1) lands the **provider seam** + the feishu primitives so the
//! binding / notifier / read-only / gateway tasks can build on a stable base.
//! IM is control-plane only: it never executes code or tasks (that stays in
//! opencode/dsh clients).
//!
//! `binding` (P1) is the identity half of that: one-time binding codes that a
//! member creates over an authenticated channel and redeems from their IM
//! account. Everything inbound resolves its actor through `im_bindings` — the
//! IM gateway itself stays privilege-free (design §5.3).

pub mod actions;
pub mod agent;
pub mod binding;
pub mod cards;
pub mod config;
pub mod gateway;
pub mod gateway_client;
pub mod notifier;
pub mod providers;
pub mod query;
pub mod types;
pub mod whitelist;
pub mod ws_gateway;

use rusqlite::Connection;
use serde_json::Value;

pub use config::ImConfig;
pub use notifier::Notifier;
pub use providers::feishu::FeishuProvider;
pub use providers::ImProvider;
pub use types::{ImChannel, ImDelegatedContext, ImEvent, ImMessage};

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str).filter(|x| !x.is_empty())
}

/// Entry point for every `im.*` RPC method (see `serve.rs` dispatch).
///
/// `team_id` is resolved by the caller from the certificate identity, so a
/// member can only ever act inside a team they belong to. In P1 the gateway
/// runs in-process and calls [`binding::redeem`] directly; the RPC surface here
/// is what members use (create a code, inspect/unbind their own binding).
pub fn rpc(
    method: &str,
    args: &Value,
    conn: &mut Connection,
    member_id: &str,
    team_id: &str,
) -> Result<Value, String> {
    match method {
        "im.bind_code.create" => {
            let ttl = args.get("ttl_secs").and_then(Value::as_i64);
            binding::create_code(conn, member_id, team_id, ttl)
        }
        "im.bind" => {
            let platform = s(args, "platform").unwrap_or("feishu");
            let im_user_id = s(args, "im_user_id").ok_or("im.bind: `im_user_id` is required")?;
            let code = s(args, "code").ok_or("im.bind: `code` is required")?;
            let ctx = ImDelegatedContext {
                platform: platform.to_string(),
                im_user_id: im_user_id.to_string(),
                im_team: s(args, "im_team").map(str::to_string),
                signature: s(args, "signature").map(str::to_string),
                // CLI/RPC path (P1): no gateway envelope — minted now, no nonce.
                timestamp: chrono::Utc::now().timestamp(),
                nonce: String::new(),
            };
            binding::redeem_in_team(conn, &ctx, code, team_id)
        }
        "im.unbind" => {
            let platform = s(args, "platform").unwrap_or("feishu");
            let im_user_id = s(args, "im_user_id")
                .ok_or("im.unbind: `im_user_id` is required")?;
            binding::unbind(conn, member_id, team_id, platform, im_user_id)
        }
        "im.bindings.list" => {
            let all = args.get("all").and_then(Value::as_bool).unwrap_or(false);
            binding::list_bindings(conn, member_id, team_id, all)
        }
        "im.resolve" => {
            let platform = s(args, "platform").unwrap_or("feishu");
            let im_user_id = s(args, "im_user_id").ok_or("im.resolve: `im_user_id` is required")?;
            if !binding::is_lead(conn, team_id, member_id)? {
                return Err("im.resolve: only the team owner or a co-lead may resolve an IM identity".to_string());
            }
            let found = binding::resolve_member(conn, platform, im_user_id, team_id)?;
            Ok(serde_json::json!({ "ok": true, "bound": found.is_some(), "member": found }))
        }
        "im.query" => {
            // IM read-only queries (P1): the identity comes from the binding
            // (resolve_member), not from the RPC caller's certificate — a
            // bound IM user reads their own team slice through the gateway.
            let platform = s(args, "platform").unwrap_or("feishu");
            let im_user_id =
                s(args, "im_user_id").ok_or("im.query: `im_user_id` is required")?;
            let kind = s(args, "kind").ok_or("im.query: `kind` is required")?;
            let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(10);
            query::query(conn, platform, im_user_id, team_id, kind, limit)
        }
        "im.whitelist" => whitelist::list(conn),
        // IM chat audit (方案 1 sidecar): the gateway logs each turn of a
        // free-text conversation with the member's agent. Audit only — it
        // mutates no state; the reply itself travels gateway → Feishu.
        "im.chat.log" => {
            let platform = s(args, "platform").unwrap_or("feishu");
            let im_user_id =
                s(args, "im_user_id").ok_or("im.chat.log: `im_user_id` is required")?;
            let direction = s(args, "direction").ok_or("im.chat.log: `direction` is required")?;
            let event_type = match direction {
                "in" => "im.chat.in",
                "out" => "im.chat.out",
                other => {
                    return Err(format!("im.chat.log: unknown direction `{other}` (in|out)"))
                }
            };
            let text: String = s(args, "text").unwrap_or("").chars().take(2000).collect();
            let payload = serde_json::json!({
                "source": format!("im:{platform}"),
                "im_user": im_user_id,
                "direction": direction,
                "text": text,
                "im_msg_id": s(args, "im_msg_id"),
            });
            let tid = team_id.to_string();
            let mid = member_id.to_string();
            crate::db::with_write(conn, |tx| {
                crate::events::emit(tx, &tid, Some(&mid), event_type, Some(&payload))?;
                Ok(())
            })
            .map_err(|e: rusqlite::Error| e.to_string())?;
            Ok(serde_json::json!({ "ok": true }))
        }
        // P2 write actions (member approval / task acceptance) — the
        // executor pipeline (whitelist → role → idempotency → state machine
        // → audit) lives in im::actions.
        "im.approve" | "im.deny" | "im.verify" | "im.reject" => {
            let platform = s(args, "platform").unwrap_or("feishu");
            let im_user_id = s(args, "im_user_id")
                .ok_or("im write action: `im_user_id` is required")?;
            // Identity consistency (fault drill L2): the delegated identity
            // must be bound to the certificate member — otherwise any member
            // could pass a lead's im_user_id and act with lead privileges.
            // The gateway path passes trivially (it dispatches as the
            // binding-resolved member already).
            if let Err(e) =
                binding::ensure_identity_bound_to_member(conn, platform, im_user_id, member_id, team_id)
            {
                // Audit the refused escalation attempt (L2 interception 留痕).
                let _ = actions::audit_rejection(conn, team_id, platform, im_user_id, args, &e);
                return Err(e);
            }
            actions::handle(conn, method, args, platform, im_user_id, team_id)
        }
        // Interactive cards (design §5.2): a card button first REQUESTS a
        // short-lived confirm token (audited), then EXECUTES the action with
        // that token (double confirmation). Both reuse the write-action
        // pipeline; neither trusts a caller-supplied member_id.
        "im.card.request" => {
            cards::request(conn, args, s(args, "platform"), s(args, "im_user_id"), member_id, team_id)
        }
        "im.card.execute" => {
            cards::execute(conn, args, s(args, "platform"), s(args, "im_user_id"), member_id, team_id)
        }
        other => Err(format!("unknown im method `{other}`")),
    }
}

/// Resolve `(member_id, team_id)` for an actor session key (CLI path).
fn actor_for_session(
    conn: &Connection,
    session: &str,
    team: Option<&str>,
) -> Result<(String, String), String> {
    let mut stmt = conn
        .prepare(
            "SELECT m.id, m.team_id FROM members m JOIN teams t ON t.id = m.team_id
             WHERE m.session_key = ?1 AND m.state NOT IN ('left','denied') AND t.state != 'destroyed'
             ORDER BY m.joined_at ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([session], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?;
    let all: Vec<(String, String)> = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())?;
    match team {
        Some(tid) => all
            .into_iter()
            .find(|(_, t)| t == tid)
            .ok_or_else(|| format!("session `{session}` is not a member of team {tid}")),
        None => match all.len() {
            0 => Err(format!("session `{session}` is not a member of any team")),
            1 => Ok(all.into_iter().next().unwrap()),
            _ => {
                let ids: Vec<String> = all.into_iter().map(|(_, t)| t).collect();
                Err(format!(
                    "session `{session}` belongs to several teams; pass --team (one of {ids:?})"
                ))
            }
        },
    }
}

/// `teamx im ...` — the local/CLI path for IM binding.
///
/// Network teams do the same over RPC (`im.*`); the local path exists so a
/// member can create and redeem a code without a server, which is also the
/// acceptance path used by `tests/im-binding.sh` and the P1 demo.
pub fn run(conn: &mut Connection, cmd: &crate::cli::ImCmd) -> Result<Value, String> {
    use crate::cli::ImCmd;
    match cmd {
        ImCmd::BindCode { session, team, ttl_secs } => {
            let (member_id, team_id) = actor_for_session(conn, session, team.as_deref())?;
            binding::create_code(conn, &member_id, &team_id, *ttl_secs)
        }
        ImCmd::Bind { code, im_user, platform, im_team, signature, session, team } => {
            let (_member_id, team_id) = actor_for_session(conn, session, team.as_deref())?;
            let ctx = ImDelegatedContext {
                platform: platform.clone(),
                im_user_id: im_user.clone(),
                im_team: im_team.clone(),
                signature: signature.clone(),
                // CLI path (P1): no gateway envelope — minted now, no nonce.
                timestamp: chrono::Utc::now().timestamp(),
                nonce: String::new(),
            };
            // A code belongs to one team; the actor's team must match it.
            binding::redeem_in_team(conn, &ctx, code, &team_id)
        }
        ImCmd::Unbind { im_user, platform, session, team } => {
            let (member_id, team_id) = actor_for_session(conn, session, team.as_deref())?;
            binding::unbind(conn, &member_id, &team_id, platform, im_user)
        }
        ImCmd::Bindings { all, session, team } => {
            let (member_id, team_id) = actor_for_session(conn, session, team.as_deref())?;
            binding::list_bindings(conn, &member_id, &team_id, *all)
        }
        ImCmd::Resolve { im_user, platform, session, team } => {
            let (member_id, team_id) = actor_for_session(conn, session, team.as_deref())?;
            if !binding::is_lead(conn, &team_id, &member_id)? {
                return Err(
                    "im resolve: only the team owner or a co-lead may resolve an IM identity".to_string(),
                );
            }
            let found = binding::resolve_member(conn, platform, im_user, &team_id)?;
            Ok(serde_json::json!({ "ok": true, "team_id": team_id, "bound": found.is_some(), "member": found }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use serde_json::json;

    fn seed() -> (Connection, String) {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        db::migrate(&conn).unwrap();
        conn.execute(
            "INSERT INTO teams (id, name, owner_member_id, goal_id, state, invite_token, created_at, updated_at)
             VALUES ('t1','T','m-owner',NULL,'active','tok','now','now')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO members (id, team_id, session_key, display_name, role, state, is_lead, joined_at)
             VALUES ('m-owner','t1','s-owner','Owner','owner','active',1,'now'),
                    ('m-worker','t1','s-worker','Worker','contributor','active',0,'now'),
                    ('m-pending','t1','s-p','Pending','contributor','pending',0,'now')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO im_bindings (platform, im_user_id, member_id, team_id, bound_at, bound_by)
             VALUES ('feishu','u-owner','m-owner','t1','now','test')",
            [],
        )
        .unwrap();
        (conn, "t1".to_string())
    }

    #[test]
    fn write_action_rejects_delegated_identity_not_bound_to_cert_member() {
        // Fault drill L2 (privilege escalation): worker's certificate +
        // the owner's im_user_id must be refused before any state change.
        let mut conn;
        let team;
        (conn, team) = seed();
        let args = json!({ "member_id": "m-pending", "im_user_id": "u-owner" });
        let e = rpc("im.approve", &args, &mut conn, "m-worker", &team).unwrap_err();
        assert!(e.contains("identity mismatch"), "{e}");
        let state: String = conn
            .query_row("SELECT state FROM members WHERE id='m-pending'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(state, "pending", "no state change on the refused attempt");
    }

    #[test]
    fn write_action_with_own_bound_identity_works() {
        let mut conn;
        let team;
        (conn, team) = seed();
        // u-owner is bound to m-owner (lead); cert member = m-owner → pass.
        let args = json!({ "member_id": "m-pending", "im_user_id": "u-owner" });
        let out = rpc("im.approve", &args, &mut conn, "m-owner", &team).unwrap();
        assert_eq!(out["ok"], true);
        let state: String = conn
            .query_row("SELECT state FROM members WHERE id='m-pending'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(state, "active");
    }
}
