//! 平台面（design/014 §6/§7）：**一次性 bootstrap token** + 平台管理员 + **独立审计**。
//!
//! 设计要点：
//! - **复用 design/006 的一次性 token 语义**（兑现即失效 + 可吊销），不另造引导协议（C15）。
//! - token **只存哈希**（明文仅在生成时返回一次），可设 TTL。
//! - **审计独立于租户台账**（NFR-9）：平台审计落在平台库的 `platform_audit`，
//!   租户不可删改（租户台账在各自租户 server 库里）。
//! - 兑现后绑定平台管理员（`users.id` 为「人」的权威源，D8）。

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

/// 生成一次性 bootstrap token：返回明文（**只返回这一次**）+ 落哈希。
pub fn issue_bootstrap_token(conn: &mut Connection, ttl_secs: Option<i64>) -> Result<Value, String> {
    let token = crate::util::gen_access_token(48);
    let hash = token_hash(&token);
    let id = uuid::Uuid::new_v4().to_string();
    let now = crate::db::now();
    let expires_at = ttl_secs.map(|s| {
        let t = chrono::Utc::now() + chrono::Duration::seconds(s.max(60));
        t.to_rfc3339()
    });
    conn.execute(
        "INSERT INTO platform_tokens (id, token_hash, purpose, state, created_at, expires_at)
         VALUES (?1, ?2, 'bootstrap', 'issued', ?3, ?4)",
        params![id, hash, now, expires_at],
    )
    .map_err(|e| format!("db: {e}"))?;
    audit(conn, "platform", "bootstrap_token.issued", Some(&format!("id={id}")))?;
    Ok(json!({
        "ok": true,
        "token_id": id,
        "token": token,
        "expires_at": expires_at,
        "note": "show once; the server stores only its SHA-256 hash. Redeem with `teamx platform redeem --token <t>`",
    }))
}

/// 兑现 bootstrap token → 绑定平台管理员（一次性：兑现后状态转 `redeemed`，复用被拒）。
pub fn redeem_bootstrap_token(
    conn: &mut Connection,
    token: &str,
    user_id: &str,
    user_name: &str,
) -> Result<Value, String> {
    if token.trim().is_empty() {
        return Err("empty token".to_string());
    }
    let hash = token_hash(token.trim());
    let row: Option<(String, String, Option<String>)> = conn
        .query_row(
            "SELECT id, state, expires_at FROM platform_tokens WHERE token_hash = ?1",
            params![hash],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|e| format!("db: {e}"))?;
    let (id, state, expires_at) = row.ok_or_else(|| "invalid bootstrap token".to_string())?;
    if state != "issued" {
        audit(conn, user_id, "bootstrap_token.rejected", Some(&format!("id={id} state={state}")))?;
        return Err(format!("bootstrap token is {state} (single-use; already redeemed or revoked)"));
    }
    if let Some(exp) = expires_at {
        if let Ok(t) = chrono::DateTime::parse_from_rfc3339(&exp) {
            if t.with_timezone(&chrono::Utc) < chrono::Utc::now() {
                conn.execute(
                    "UPDATE platform_tokens SET state='expired' WHERE id=?1",
                    params![id],
                )
                .map_err(|e| format!("db: {e}"))?;
                audit(conn, user_id, "bootstrap_token.rejected", Some(&format!("id={id} expired")))?;
                return Err("bootstrap token expired".to_string());
            }
        }
    }
    if user_id.trim().is_empty() {
        return Err("redeem requires a user id (bind the platform admin to a person)".to_string());
    }
    let now = crate::db::now();
    crate::db::with_write(conn, |tx| {
        tx.execute(
            "UPDATE platform_tokens SET state='redeemed', redeemed_at=?1, redeemed_by=?2 WHERE id=?3",
            params![now, user_id, id],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO platform_admins (user_id, user_name, granted_at, granted_via)
             VALUES (?1, ?2, ?3, ?4)",
            params![user_id, user_name, now, format!("bootstrap:{id}")],
        )?;
        Ok(())
    })
    .map_err(|e| format!("db: {e}"))?;
    audit(conn, user_id, "bootstrap_token.redeemed", Some(&format!("id={id} user={user_name}")))?;
    Ok(json!({
        "ok": true, "token_id": id, "state": "redeemed",
        "admin": { "user_id": user_id, "user_name": user_name },
    }))
}

/// 吊销一个未兑现的 token（已兑现的不能吊销——那是管理员身份，走 `admin revoke`）。
pub fn revoke_bootstrap_token(conn: &mut Connection, token_id: &str) -> Result<Value, String> {
    let n = conn
        .execute(
            "UPDATE platform_tokens SET state='revoked' WHERE id=?1 AND state='issued'",
            params![token_id],
        )
        .map_err(|e| format!("db: {e}"))?;
    audit(conn, "platform", "bootstrap_token.revoked", Some(&format!("id={token_id} changed={n}")))?;
    Ok(json!({ "ok": n > 0, "token_id": token_id, "revoked": n > 0 }))
}

/// 列 token（不含哈希明文之外的信息；只给状态与时间，便于审计）。
pub fn list_tokens(conn: &Connection) -> Result<Vec<Value>, String> {
    let mut stmt = conn
        .prepare("SELECT id, purpose, state, created_at, expires_at, redeemed_at, redeemed_by FROM platform_tokens ORDER BY created_at DESC")
        .map_err(|e| format!("db: {e}"))?;
    let rows = stmt
        .query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "purpose": r.get::<_, String>(1)?,
                "state": r.get::<_, String>(2)?,
                "created_at": r.get::<_, String>(3)?,
                "expires_at": r.get::<_, Option<String>>(4)?,
                "redeemed_at": r.get::<_, Option<String>>(5)?,
                "redeemed_by": r.get::<_, Option<String>>(6)?,
            }))
        })
        .map_err(|e| format!("db: {e}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| format!("db: {e}"))
}

/// 是否平台管理员（fail-closed：查不到即 false）。
pub fn is_platform_admin(conn: &Connection, user_id: &str) -> bool {
    if user_id.trim().is_empty() {
        return false;
    }
    conn.query_row(
        "SELECT COUNT(*) FROM platform_admins WHERE user_id = ?1",
        params![user_id],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
    .unwrap_or(false)
}

/// 列平台管理员。
pub fn list_admins(conn: &Connection) -> Result<Vec<Value>, String> {
    let mut stmt = conn
        .prepare("SELECT user_id, user_name, granted_at, granted_via FROM platform_admins ORDER BY granted_at")
        .map_err(|e| format!("db: {e}"))?;
    let rows = stmt
        .query_map([], |r| {
            Ok(json!({
                "user_id": r.get::<_, String>(0)?,
                "user_name": r.get::<_, String>(1)?,
                "granted_at": r.get::<_, String>(2)?,
                "granted_via": r.get::<_, String>(3)?,
            }))
        })
        .map_err(|e| format!("db: {e}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| format!("db: {e}"))
}

/// 撤销平台管理员（不能撤最后一个）。
pub fn revoke_admin(conn: &mut Connection, user_id: &str) -> Result<Value, String> {
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM platform_admins", [], |r| r.get(0))
        .unwrap_or(0);
    if total <= 1 {
        return Err("cannot revoke the last platform admin".to_string());
    }
    let n = conn
        .execute("DELETE FROM platform_admins WHERE user_id = ?1", params![user_id])
        .map_err(|e| format!("db: {e}"))?;
    audit(conn, "platform", "platform_admin.revoked", Some(&format!("user={user_id}")))?;
    Ok(json!({ "ok": n > 0, "user_id": user_id, "revoked": n > 0 }))
}

/// 读平台审计（独立于租户台账）。
pub fn audit_log(conn: &Connection, limit: i64) -> Result<Vec<Value>, String> {
    let mut stmt = conn
        .prepare("SELECT at, actor, action, detail FROM platform_audit ORDER BY id DESC LIMIT ?1")
        .map_err(|e| format!("db: {e}"))?;
    let rows = stmt
        .query_map(params![limit.clamp(1, 500)], |r| {
            Ok(json!({
                "at": r.get::<_, String>(0)?,
                "actor": r.get::<_, String>(1)?,
                "action": r.get::<_, String>(2)?,
                "detail": r.get::<_, Option<String>>(3)?,
            }))
        })
        .map_err(|e| format!("db: {e}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| format!("db: {e}"))
}

fn audit(conn: &Connection, actor: &str, action: &str, detail: Option<&str>) -> Result<(), String> {
    conn.execute(
        "INSERT INTO platform_audit (at, actor, action, detail) VALUES (?1, ?2, ?3, ?4)",
        params![crate::db::now(), actor, action, detail],
    )
    .map_err(|e| format!("audit: {e}"))?;
    Ok(())
}

/// token 哈希（只存哈希，不存明文）。
fn token_hash(token: &str) -> String {
    let d = ring::digest::digest(&ring::digest::SHA256, token.as_bytes());
    d.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::db::migrate(&c).unwrap();
        c
    }

    #[test]
    fn bootstrap_token_is_single_use_and_binds_admin() {
        let mut c = conn();
        let out = issue_bootstrap_token(&mut c, Some(3600)).unwrap();
        let token = out["token"].as_str().unwrap().to_string();
        let tid = out["token_id"].as_str().unwrap().to_string();
        // 明文不落库：库里只有哈希
        let stored: String = c
            .query_row("SELECT token_hash FROM platform_tokens WHERE id=?1", params![tid], |r| r.get(0))
            .unwrap();
        assert_ne!(stored, token, "不得明文存 token");
        assert_eq!(stored.len(), 64, "sha256 hex");
        // 兑现 → 成为平台管理员
        let r = redeem_bootstrap_token(&mut c, &token, "u1", "alice").unwrap();
        assert_eq!(r["state"], json!("redeemed"));
        assert!(is_platform_admin(&c, "u1"));
        assert!(!is_platform_admin(&c, "u2"), "fail-closed");
        // 一次性：复用被拒
        let e = redeem_bootstrap_token(&mut c, &token, "u2", "bob").unwrap_err();
        assert!(e.contains("single-use"), "{e}");
        assert!(!is_platform_admin(&c, "u2"), "复用不得授予管理员");
        // 审计留痕（独立表）
        let log = audit_log(&c, 50).unwrap();
        assert!(log.iter().any(|e| e["action"] == json!("bootstrap_token.redeemed")));
        assert!(log.iter().any(|e| e["action"] == json!("bootstrap_token.rejected")));
    }

    #[test]
    fn revoked_and_expired_tokens_are_rejected() {
        let mut c = conn();
        // 吊销
        let a = issue_bootstrap_token(&mut c, None).unwrap();
        let aid = a["token_id"].as_str().unwrap().to_string();
        let atok = a["token"].as_str().unwrap().to_string();
        assert!(revoke_bootstrap_token(&mut c, &aid).unwrap()["revoked"].as_bool().unwrap());
        assert!(redeem_bootstrap_token(&mut c, &atok, "u1", "alice").is_err());
        // 过期（TTL 下限 60s，用负值走 clamp 不可行 → 直接改库时间）
        let b = issue_bootstrap_token(&mut c, Some(60)).unwrap();
        let bid = b["token_id"].as_str().unwrap().to_string();
        let btok = b["token"].as_str().unwrap().to_string();
        c.execute(
            "UPDATE platform_tokens SET expires_at=?1 WHERE id=?2",
            params!["2000-01-01T00:00:00+00:00", bid],
        )
        .unwrap();
        let e = redeem_bootstrap_token(&mut c, &btok, "u1", "alice").unwrap_err();
        assert!(e.contains("expired"), "{e}");
        // 最后一个管理员不可撤
        assert!(revoke_admin(&mut c, "u1").is_err(), "only admin cannot be revoked");
    }
}
