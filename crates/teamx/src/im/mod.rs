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

pub mod binding;
pub mod config;
pub mod gateway;
pub mod gateway_client;
pub mod notifier;
pub mod providers;
pub mod query;
pub mod types;
pub mod whitelist;

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
