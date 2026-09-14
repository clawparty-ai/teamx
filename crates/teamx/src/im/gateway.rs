//! Inbound IM gateway (P1): the security pipeline every delegated IM action
//! passes before the server acts on it (design §5.3/§5.4, task
//! `im-p1-inbound-gateway-security`).
//!
//! Pipeline (in order, each failure audited as `im.rejected`):
//! 1. required fields present (malformed)
//! 2. HMAC-SHA256 signature over the delegated fields (invalid/missing)
//! 3. timestamp freshness ±5min (stale)
//! 4. one-time nonce replay check via `im_seen` (duplicate, TTL 7d)
//! 5. binding resolution (unbound / forbidden)
//! 6. per-(im_user, team) rate limit (rate_limited)
//!
//! The delegated context **never carries a member_id** — the actor is always
//! resolved from the binding table, so a compromised gateway cannot
//! impersonate a member. Rejections and successes are audited equally.

use rusqlite::{Connection, params};
use serde_json::{Value, json};

use crate::db;
use super::binding;
use super::config::ImConfig;
use super::types::ImDelegatedContext;

/// Delegation freshness window (±5min, design §5.4).
pub const TIMESTAMP_WINDOW_SECS: i64 = 300;
/// `im_seen` replay-protection TTL (7 days).
pub const SEEN_TTL_SECS: i64 = 7 * 24 * 3600;
/// Max accepted delegations per (im_user, team) inside the rate window.
pub const RATE_LIMIT_MAX: i64 = 30;
/// Rate-limit sliding window (seconds).
pub const RATE_WINDOW_SECS: i64 = 60;

/// A rejection reason — also the ledger `reason_code`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    Malformed,
    MissingSignature,
    InvalidSignature,
    StaleTimestamp,
    Duplicate,
    Unbound,
    Forbidden,
    RateLimited,
}

impl Rejection {
    fn code(self) -> &'static str {
        match self {
            Rejection::Malformed => "malformed",
            Rejection::MissingSignature => "missing_signature",
            Rejection::InvalidSignature => "invalid_signature",
            Rejection::StaleTimestamp => "stale_timestamp",
            Rejection::Duplicate => "duplicate",
            Rejection::Unbound => "unbound",
            Rejection::Forbidden => "forbidden",
            Rejection::RateLimited => "rate_limited",
        }
    }
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

/// HMAC-SHA256 over the canonical delegation string, hex-encoded.
/// Canonical form: `platform|im_user_id|im_team|timestamp|nonce`.
pub fn sign_delegation(secret: &str, ctx: &ImDelegatedContext) -> String {
    use ring::hmac;
    let msg = canonical(ctx);
    let key = hmac::Key::new(ring::hmac::HMAC_SHA256, secret.as_bytes());
    hex(hmac::sign(&key, msg.as_bytes()).as_ref())
}

fn canonical(ctx: &ImDelegatedContext) -> String {
    format!(
        "{}|{}|{}|{}|{}",
        ctx.platform,
        ctx.im_user_id,
        ctx.im_team.as_deref().unwrap_or(""),
        ctx.timestamp,
        ctx.nonce
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Audit one gateway outcome to the ledger (rejections and successes alike).
fn audit(
    conn: &mut Connection,
    team_id: &str,
    platform: &str,
    im_user_id: &str,
    reason: Option<Rejection>,
) -> Result<(), String> {
    let payload = json!({
        "source": format!("im:{platform}"),
        "im_user": im_user_id,
        "team_id": team_id,
        "reason_code": reason.map(|r| r.code()).unwrap_or("accepted"),
    });
    db::with_write(conn, |tx| {
        crate::events::emit(
            tx,
            team_id,
            Some(im_user_id),
            if reason.is_some() { "im.rejected" } else { "im.accepted" },
            Some(&payload),
        )?;
        Ok(())
    })
    .map_err(|e: rusqlite::Error| e.to_string())
}

/// The full inbound pipeline. Returns the resolved member identity on
/// success; every failure path audited as `im.rejected`.
pub fn accept(
    conn: &mut Connection,
    cfg: &ImConfig,
    team_id: &str,
    ctx: &ImDelegatedContext,
) -> Result<Value, String> {
    let platform = ctx.platform.clone();
    let im_user_id = ctx.im_user_id.clone();
    let reject = |conn: &mut Connection, reason: Rejection| -> String {
        let _ = audit(conn, team_id, &platform, &im_user_id, Some(reason));
        format!("im gateway rejected: {reason}")
    };

    // 1. Required fields.
    if im_user_id.trim().is_empty() || ctx.nonce.trim().is_empty() {
        return Err(reject(conn, Rejection::Malformed));
    }

    // 2. HMAC signature (gateway holds the provider secret; serve verifies).
    let secret = cfg
        .secret_for(&platform)
        .ok_or_else(|| reject(conn, Rejection::MissingSignature))?;
    let expected = sign_delegation(secret, ctx);
    if ctx.signature.as_deref().unwrap_or("") != expected {
        return Err(reject(
            conn,
            if ctx.signature.as_deref().unwrap_or("").is_empty() {
                Rejection::MissingSignature
            } else {
                Rejection::InvalidSignature
            },
        ));
    }

    // 3. Timestamp freshness (±5min).
    let now = chrono::Utc::now().timestamp();
    if (now - ctx.timestamp).abs() > TIMESTAMP_WINDOW_SECS {
        return Err(reject(conn, Rejection::StaleTimestamp));
    }

    // 4. Per-(im_user, team) rate limit — checked BEFORE consuming a slot
    // (count = prior attempts in the window; the current one is not yet seen).
    if rate_limited(conn, team_id, &im_user_id)? {
        return Err(reject(conn, Rejection::RateLimited));
    }

    // 5. One-time nonce replay protection (im_seen, TTL 7d).
    mark_seen(conn, team_id, &platform, &ctx.nonce, &im_user_id).map_err(|e| {
        let _ = audit(conn, team_id, &platform, &im_user_id, Some(Rejection::Duplicate));
        format!("im gateway rejected: duplicate ({e})")
    })?;

    // 6. Binding resolution — unbound or revoked → no actor.
    let ident = match binding::resolve_member(conn, &platform, &im_user_id, team_id)? {
        Some(v) => v,
        None => return Err(reject(conn, Rejection::Unbound)),
    };
    let member_id = ident["member_id"].as_str().unwrap_or_default().to_string();

    let _ = audit(conn, team_id, &platform, &im_user_id, None);
    Ok(json!({
        "ok": true,
        "member_id": member_id,
        "display_name": ident["display_name"],
        "role": ident["role"],
        "team_id": team_id,
        "platform": platform,
        "im_user_id": im_user_id,
    }))
}

/// `im_seen` replay protection: INSERT-or-fail on the (platform, nonce, team)
/// key; rows older than the TTL are pruned opportunistically.
fn mark_seen(
    conn: &Connection,
    team_id: &str,
    platform: &str,
    nonce: &str,
    im_user_id: &str,
) -> Result<(), String> {
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "DELETE FROM im_seen WHERE created_at < ?1",
        params![now - SEEN_TTL_SECS],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO im_seen (platform, nonce, team_id, im_user_id, created_at) VALUES (?1,?2,?3,?4,?5)",
        params![platform, nonce, team_id, im_user_id, now],
    )
    .map_err(|e| match e {
        rusqlite::Error::SqliteFailure(fe, _) if fe.code == rusqlite::ErrorCode::ConstraintViolation => {
            "nonce already seen (replay)".to_string()
        }
        other => other.to_string(),
    })?;
    Ok(())
}

/// True when this (im_user, team) pair sent too many delegations recently.
fn rate_limited(conn: &Connection, team_id: &str, im_user_id: &str) -> Result<bool, String> {
    let cutoff = chrono::Utc::now().timestamp() - RATE_WINDOW_SECS;
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM im_seen WHERE team_id = ?1 AND im_user_id = ?2 AND created_at >= ?3",
            params![team_id, im_user_id, cutoff],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    Ok(n >= RATE_LIMIT_MAX)
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

    fn cfg() -> ImConfig {
        ImConfig {
            feishu_app_id: Some("cli_x".into()),
            feishu_app_secret: Some("gw-secret".into()),
            dry_run: true,
            ..Default::default()
        }
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
        (conn, "t1".to_string())
    }

    fn bind(conn: &Connection, im_user: &str, member_id: &str) {
        conn.execute(
            "INSERT INTO im_bindings (platform, im_user_id, member_id, team_id, bound_at, bound_by)
             VALUES ('feishu', ?1, ?2, 't1', 'now', 'test')",
            params![im_user, member_id],
        )
        .unwrap();
    }

    /// A correctly signed context for `im_user` (nonce must be unique).
    fn signed(im_user: &str, nonce: &str, secret: &str) -> ImDelegatedContext {
        let mut ctx = ImDelegatedContext {
            platform: "feishu".into(),
            im_user_id: im_user.into(),
            im_team: None,
            signature: None,
            timestamp: chrono::Utc::now().timestamp(),
            nonce: nonce.into(),
        };
        ctx.signature = Some(sign_delegation(secret, &ctx));
        ctx
    }

    #[test]
    fn happy_path_resolves_and_audits_accepted() {
        let (mut conn, team) = seed();
        bind(&conn, "u1", "m-worker");
        let v = accept(&mut conn, &cfg(), &team, &signed("u1", "n1", "gw-secret")).unwrap();
        assert_eq!(v["member_id"], json!("m-worker"));
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM events WHERE type='im.accepted'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "success is audited as im.accepted");
    }

    #[test]
    fn missing_signature_rejected() {
        let (mut conn, team) = seed();
        bind(&conn, "u1", "m-worker");
        let mut ctx = signed("u1", "n1", "gw-secret");
        ctx.signature = None;
        assert!(accept(&mut conn, &cfg(), &team, &ctx).unwrap_err().contains("missing_signature"));
    }

    #[test]
    fn invalid_signature_rejected() {
        let (mut conn, team) = seed();
        bind(&conn, "u1", "m-worker");
        let mut ctx = signed("u1", "n2", "gw-secret");
        ctx.signature = Some("deadbeef".into());
        assert!(accept(&mut conn, &cfg(), &team, &ctx).unwrap_err().contains("invalid_signature"));
    }

    #[test]
    fn stale_timestamp_rejected() {
        let (mut conn, team) = seed();
        bind(&conn, "u1", "m-worker");
        let mut ctx = signed("u1", "n3", "gw-secret");
        ctx.timestamp -= TIMESTAMP_WINDOW_SECS + 1;
        ctx.signature = Some(sign_delegation("gw-secret", &ctx));
        assert!(accept(&mut conn, &cfg(), &team, &ctx).unwrap_err().contains("stale_timestamp"));
    }

    #[test]
    fn duplicate_nonce_rejected() {
        let (mut conn, team) = seed();
        bind(&conn, "u1", "m-worker");
        accept(&mut conn, &cfg(), &team, &signed("u1", "n4", "gw-secret")).unwrap();
        // Same nonce again (fresh signature — still a replay).
        assert!(accept(&mut conn, &cfg(), &team, &signed("u1", "n4", "gw-secret"))
            .unwrap_err()
            .contains("duplicate"));
    }

    #[test]
    fn unbound_identity_rejected() {
        let (mut conn, team) = seed();
        // No binding for u9.
        assert!(accept(&mut conn, &cfg(), &team, &signed("u9", "n5", "gw-secret"))
            .unwrap_err()
            .contains("unbound"));
    }

    #[test]
    fn malformed_context_rejected() {
        let (mut conn, team) = seed();
        let mut ctx = ImDelegatedContext {
            platform: "feishu".into(),
            im_user_id: String::new(),
            ..Default::default()
        };
        ctx.nonce = "n6".into();
        ctx.signature = Some(sign_delegation("gw-secret", &ctx));
        assert!(accept(&mut conn, &cfg(), &team, &ctx).unwrap_err().contains("malformed"));
    }

    #[test]
    fn rate_limit_per_im_user_and_team() {
        let (mut conn, team) = seed();
        bind(&conn, "u1", "m-worker");
        // RATE_LIMIT_MAX accepted delegations, then the next is limited.
        for i in 0..RATE_LIMIT_MAX {
            let nonce = format!("rl-{i}");
            accept(&mut conn, &cfg(), &team, &signed("u1", &nonce, "gw-secret")).unwrap();
        }
        let err = accept(&mut conn, &cfg(), &team, &signed("u1", "rl-over", "gw-secret")).unwrap_err();
        assert!(err.contains("rate_limited"), "{err}");
        // A different im_user has its own budget.
        bind(&conn, "u2", "m-owner");
        accept(&mut conn, &cfg(), &team, &signed("u2", "rl-other", "gw-secret")).unwrap();
    }

    #[test]
    fn rejection_is_audited_im_rejected() {
        let (mut conn, team) = seed();
        // Unbound → rejected + audited.
        let _ = accept(&mut conn, &cfg(), &team, &signed("u9", "n7", "gw-secret"));
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM events WHERE type='im.rejected'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "the rejection must be audited");
        let payload_raw: String = conn
            .query_row("SELECT payload_json FROM events WHERE type='im.rejected'", [], |r| r.get(0))
            .unwrap();
        let payload: Value = serde_json::from_str(&payload_raw).unwrap();
        assert_eq!(payload["reason_code"], json!("unbound"));
        assert_eq!(payload["source"], json!("im:feishu"));
    }
}
