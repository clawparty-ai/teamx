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

pub(crate) fn audit(conn: &Connection, actor: &str, action: &str, detail: Option<&str>) -> Result<(), String> {
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

// ---------------------------------------------------------------------------
// 云控制台用户 Phase 1：密码登录（GitHub 见 Phase 3）。
// ---------------------------------------------------------------------------

/// 会话默认有效期（7 天）。
pub const SESSION_DAYS: i64 = 7;
/// PBKDF2 迭代次数（OWASP 2023 对 HMAC-SHA256 的建议值）。
const PBKDF2_ITER: u32 = 600_000;

/// 密码哈希：`pbkdf2-sha256$<iter>$<salt-b64>$<hash-b64>`（ring，无新依赖）。
/// 前缀版本化 —— 将来迁 argon2id 时旧行仍可验（按前缀分发）。
pub fn hash_password(password: &str) -> Result<String, String> {
    if password.len() < 8 {
        return Err("password must be at least 8 characters".to_string());
    }
    use ring::rand::SecureRandom;
    let rng = ring::rand::SystemRandom::new();
    let mut salt = [0u8; 32];
    rng.fill(&mut salt).map_err(|_| "random salt failed".to_string())?;
    let mut out = [0u8; 32];
    ring::pbkdf2::derive(
        ring::pbkdf2::PBKDF2_HMAC_SHA256,
        std::num::NonZeroU32::new(PBKDF2_ITER).unwrap(),
        &salt,
        password.as_bytes(),
        &mut out,
    );
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD;
    Ok(format!(
        "pbkdf2-sha256${PBKDF2_ITER}${}${}",
        b64.encode(salt),
        b64.encode(out)
    ))
}

/// 验密码（失败统一返回 false，不区分"用户不存在/密码错/格式坏" —— 防枚举）。
pub fn verify_password(password: &str, stored: &str) -> bool {
    let parts: Vec<&str> = stored.split('$').collect();
    if parts.len() != 4 || parts[0] != "pbkdf2-sha256" {
        return false;
    }
    let iter: u32 = match parts[1].parse() {
        Ok(n) if n >= 100_000 => n,
        _ => return false,
    };
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD;
    let (salt, expect) = match (b64.decode(parts[2]), b64.decode(parts[3])) {
        (Ok(s), Ok(e)) if !s.is_empty() && !e.is_empty() => (s, e),
        _ => return false,
    };
    ring::pbkdf2::verify(
        ring::pbkdf2::PBKDF2_HMAC_SHA256,
        std::num::NonZeroU32::new(iter).unwrap(),
        &salt,
        password.as_bytes(),
        &expect,
    )
    .is_ok()
}

/// 建用户（后台给用户设密码的入口）。display_name 全局唯一（登录键）。
pub fn create_user(conn: &mut Connection, display_name: &str, password: &str) -> Result<Value, String> {
    let name = display_name.trim();
    if name.is_empty() || name.len() > 64 {
        return Err("display_name must be 1..=64 chars".to_string());
    }
    let hash = hash_password(password)?;
    let id = uuid::Uuid::new_v4().to_string();
    let now = crate::db::now();
    conn.execute(
        "INSERT INTO platform_users (id, display_name, password_hash, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![id, name, hash, now],
    )
    .map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            format!("user `{name}` already exists")
        } else {
            format!("db: {e}")
        }
    })?;
    audit(conn, "platform", "platform_user.created", Some(&format!("id={id} name={name}")))?;
    Ok(json!({ "ok": true, "user_id": id, "display_name": name }))
}

/// 改密码（用户本人凭旧密码，或管理员直接重置 —— 调用方鉴权，见 CLI/路由）。
pub fn set_password(conn: &mut Connection, user_id: &str, password: &str) -> Result<Value, String> {
    let hash = hash_password(password)?;
    let n = conn
        .execute(
            "UPDATE platform_users SET password_hash = ?1 WHERE id = ?2 AND disabled_at IS NULL",
            params![hash, user_id],
        )
        .map_err(|e| format!("db: {e}"))?;
    if n == 0 {
        return Err("user not found or disabled".to_string());
    }
    // 密码变更即吊销该用户全部会话（防旧会话残留）。
    conn.execute("DELETE FROM platform_sessions WHERE user_id = ?1", params![user_id])
        .map_err(|e| format!("db: {e}"))?;
    audit(conn, "platform", "platform_user.password_changed", Some(&format!("id={user_id}")))?;
    Ok(json!({ "ok": true, "user_id": user_id }))
}

/// 禁用用户（含踢掉全部会话；拒绝登录与会话验证）。
pub fn disable_user(conn: &mut Connection, user_id: &str) -> Result<Value, String> {
    let now = crate::db::now();
    let n = conn
        .execute(
            "UPDATE platform_users SET disabled_at = ?1 WHERE id = ?2 AND disabled_at IS NULL",
            params![now, user_id],
        )
        .map_err(|e| format!("db: {e}"))?;
    if n == 0 {
        return Err("user not found or already disabled".to_string());
    }
    conn.execute("DELETE FROM platform_sessions WHERE user_id = ?1", params![user_id])
        .map_err(|e| format!("db: {e}"))?;
    audit(conn, "platform", "platform_user.disabled", Some(&format!("id={user_id}")))?;
    Ok(json!({ "ok": true, "user_id": user_id }))
}

/// 用户列表（永不返回 password_hash）。
pub fn list_users(conn: &Connection) -> Result<Value, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, display_name, github_id,
                    CASE WHEN disabled_at IS NULL THEN 0 ELSE 1 END, created_at
             FROM platform_users ORDER BY created_at",
        )
        .map_err(|e| format!("db: {e}"))?;
    let rows = stmt
        .query_map([], |r| {
            Ok(json!({
                "user_id": r.get::<_, String>(0)?,
                "display_name": r.get::<_, String>(1)?,
                "github_id": r.get::<_, String>(2)?,
                "disabled": r.get::<_, i64>(3)? != 0,
                "created_at": r.get::<_, String>(4)?,
            }))
        })
        .map_err(|e| format!("db: {e}"))?;
    let users: Vec<Value> = rows.collect::<rusqlite::Result<_>>().map_err(|e| format!("db: {e}"))?;
    Ok(json!({ "ok": true, "users": users }))
}

/// 密码登录：成功返回用户信息（调用方再发会话）。失败统一口径，防枚举。
pub fn authenticate(conn: &Connection, display_name: &str, password: &str) -> Result<Value, String> {
    let row: Option<(String, String, Option<String>)> = conn
        .query_row(
            "SELECT id, password_hash, disabled_at FROM platform_users WHERE display_name = ?1",
            params![display_name.trim()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|e| format!("db: {e}"))?;
    let (id, hash, disabled) = row.ok_or_else(|| "invalid credentials".to_string())?;
    if disabled.is_some() || hash.is_empty() || !verify_password(password, &hash) {
        return Err("invalid credentials".to_string());
    }
    Ok(json!({ "ok": true, "user_id": id, "display_name": display_name.trim() }))
}

/// 发会话（明文只返回这一次；库里只存 SHA-256）。
pub fn issue_session(conn: &mut Connection, user_id: &str) -> Result<Value, String> {
    prune_sessions(conn)?;
    let token = crate::util::gen_access_token(48);
    let now = crate::db::now();
    let expires = (chrono::Utc::now() + chrono::Duration::days(SESSION_DAYS)).to_rfc3339();
    conn.execute(
        "INSERT INTO platform_sessions (token, user_id, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![token_hash(&token), user_id, now, expires],
    )
    .map_err(|e| format!("db: {e}"))?;
    Ok(json!({ "ok": true, "token": token, "expires_at": expires }))
}

/// 验会话 → 用户（过期/禁用/不存在一律 Err；调用方转 401）。
pub fn verify_session(conn: &Connection, token: &str) -> Result<Value, String> {
    prune_sessions(conn)?;
    let now = crate::db::now();
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT u.id, u.display_name FROM platform_sessions s
             JOIN platform_users u ON u.id = s.user_id
             WHERE s.token = ?1 AND s.expires_at > ?2 AND u.disabled_at IS NULL",
            params![token_hash(token.trim()), now],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| format!("db: {e}"))?;
    let (id, name) = row.ok_or_else(|| "invalid or expired session".to_string())?;
    Ok(json!({ "ok": true, "user_id": id, "display_name": name }))
}

/// 登出（删会话；不存在也算成功 —— 幂等）。
pub fn revoke_session(conn: &Connection, token: &str) -> Result<Value, String> {
    conn.execute("DELETE FROM platform_sessions WHERE token = ?1", params![token_hash(token.trim())])
        .map_err(|e| format!("db: {e}"))?;
    Ok(json!({ "ok": true }))
}

fn prune_sessions(conn: &Connection) -> Result<(), String> {
    conn.execute("DELETE FROM platform_sessions WHERE expires_at <= ?1", params![crate::db::now()])
        .map_err(|e| format!("db: {e}"))?;
    Ok(())
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

// ---------------------------------------------------------------------------
// 用户归属 team Phase 2：配额 + 归属查询。
// ---------------------------------------------------------------------------

/// 默认租户配额（用户无显式配额行时）。
pub const DEFAULT_TENANT_QUOTA: i64 = 3;

/// 用户配额（显式行，否则默认）。
pub fn tenant_quota(conn: &Connection, user_id: &str) -> Result<i64, String> {
    let q: Option<i64> = conn
        .query_row(
            "SELECT tenant_quota FROM platform_quotas WHERE user_id = ?1",
            params![user_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("db: {e}"))?;
    Ok(q.unwrap_or(DEFAULT_TENANT_QUOTA))
}

/// 设配额（管理员操作；调用方鉴权）。
pub fn set_quota(conn: &mut Connection, user_id: &str, quota: i64) -> Result<Value, String> {
    if !(0..=100).contains(&quota) {
        return Err("quota must be 0..=100".to_string());
    }
    // 用户须存在
    let exists: Option<String> = conn
        .query_row("SELECT id FROM platform_users WHERE id = ?1", params![user_id], |r| r.get(0))
        .optional()
        .map_err(|e| format!("db: {e}"))?;
    if exists.is_none() {
        return Err("user not found".to_string());
    }
    let now = crate::db::now();
    conn.execute(
        "INSERT INTO platform_quotas (user_id, tenant_quota, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(user_id) DO UPDATE SET tenant_quota = ?2, updated_at = ?3",
        params![user_id, quota, now],
    )
    .map_err(|e| format!("db: {e}"))?;
    audit(conn, "platform", "platform_user.quota_set", Some(&format!("id={user_id} quota={quota}")))?;
    Ok(json!({ "ok": true, "user_id": user_id, "tenant_quota": quota }))
}

/// 配额检查：名下租户数 < 配额才允许新建。
pub fn check_tenant_quota(conn: &Connection, user_id: &str) -> Result<Value, String> {
    let quota = tenant_quota(conn, user_id)?;
    let used: i64 = conn
        .query_row("SELECT COUNT(*) FROM tenant_owners WHERE user_id = ?1", params![user_id], |r| r.get(0))
        .map_err(|e| format!("db: {e}"))?;
    if used >= quota {
        return Err(format!("tenant quota exceeded ({used}/{quota})"));
    }
    Ok(json!({ "ok": true, "used": used, "quota": quota }))
}

/// 租户归属用户吗（鉴权用；operator/launch-token 走旁路，不调这里）。
pub fn tenant_owned_by(conn: &Connection, team_id: &str, user_id: &str) -> Result<bool, String> {
    let hit: Option<String> = conn
        .query_row(
            "SELECT user_id FROM tenant_owners WHERE team_id = ?1",
            params![team_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("db: {e}"))?;
    Ok(hit.as_deref() == Some(user_id))
}

/// 名下租户 id 列表。
pub fn owned_teams(conn: &Connection, user_id: &str) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare("SELECT team_id FROM tenant_owners WHERE user_id = ?1 ORDER BY created_at")
        .map_err(|e| format!("db: {e}"))?;
    let rows = stmt
        .query_map(params![user_id], |r| r.get(0))
        .map_err(|e| format!("db: {e}"))?;
    rows.collect::<rusqlite::Result<Vec<String>>>().map_err(|e| format!("db: {e}"))
}

/// 标记归属（租户创建时调用；重复标记幂等拒绝）。
pub fn tag_tenant_owner(conn: &mut Connection, team_id: &str, user_id: &str) -> Result<(), String> {
    let now = crate::db::now();
    conn.execute(
        "INSERT INTO tenant_owners (team_id, user_id, created_at) VALUES (?1, ?2, ?3)",
        params![team_id, user_id, now],
    )
    .map_err(|e| format!("db: {e}"))?;
    audit(conn, &format!("user:{user_id}"), "tenant.claimed", Some(&format!("team={team_id}")))?;
    Ok(())
}

#[cfg(test)]
mod user_tests {
    use super::*;
    use rusqlite::Connection;

    fn testdb() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE platform_users (id TEXT PRIMARY KEY, display_name TEXT NOT NULL,
              github_id TEXT NOT NULL DEFAULT '', password_hash TEXT NOT NULL DEFAULT '',
              disabled_at TEXT, created_at TEXT NOT NULL);
             CREATE UNIQUE INDEX idx_platform_users_name ON platform_users(display_name);
             CREATE TABLE platform_sessions (token TEXT PRIMARY KEY, user_id TEXT NOT NULL,
              created_at TEXT NOT NULL, expires_at TEXT NOT NULL);
             CREATE TABLE platform_audit (id INTEGER PRIMARY KEY, at TEXT NOT NULL,
              actor TEXT NOT NULL, action TEXT NOT NULL, detail TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn password_hash_verify_roundtrip() {
        let h = hash_password("correct-horse-123").unwrap();
        assert!(h.starts_with("pbkdf2-sha256$600000$"));
        assert!(verify_password("correct-horse-123", &h));
        assert!(!verify_password("wrong", &h));
        assert!(!verify_password("x", "garbage"));
        assert!(!verify_password("x", "pbkdf2-sha256$10$AAA$BBB"));
        assert!(hash_password("short").is_err(), "太短必须拒绝");
    }

    #[test]
    fn user_lifecycle_auth_session_disable() {
        let mut c = testdb();
        let u = create_user(&mut c, "alice", "s3cret-pw!").unwrap();
        let uid = u["user_id"].as_str().unwrap().to_string();
        // 重名拒绝
        assert!(create_user(&mut c, "alice", "another-pw!").is_err());
        // 登录
        assert!(authenticate(&c, "alice", "s3cret-pw!").is_ok());
        assert!(authenticate(&c, "alice", "nope-nope-no").is_err());
        assert!(authenticate(&c, "nobody", "s3cret-pw!").is_err());
        // 会话
        let s = issue_session(&mut c, &uid).unwrap();
        let tok = s["token"].as_str().unwrap().to_string();
        assert!(verify_session(&c, &tok).is_ok());
        assert!(verify_session(&c, "bogus").is_err());
        // 改密码即踢掉旧会话
        set_password(&mut c, &uid, "brand-new-pw!").unwrap();
        assert!(verify_session(&c, &tok).is_err(), "改密码必须吊销旧会话");
        assert!(authenticate(&c, "alice", "brand-new-pw!").is_ok());
        // 禁用：登录与会话双拒
        disable_user(&mut c, &uid).unwrap();
        assert!(authenticate(&c, &uid, "brand-new-pw!").is_err());
        let s2 = issue_session(&mut c, &uid).unwrap();
        assert!(verify_session(&c, s2["token"].as_str().unwrap()).is_err());
        // 列表不含 hash
        let l = list_users(&c).unwrap();
        assert_eq!(l["users"][0]["display_name"], json!("alice"));
        assert!(l["users"][0].get("password_hash").is_none());
        assert!(l["users"][0].get("hash").is_none());
    }
}

#[cfg(test)]
mod quota_tests {
    use super::*;
    use rusqlite::Connection;

    fn testdb() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE platform_users (id TEXT PRIMARY KEY, display_name TEXT NOT NULL,
              github_id TEXT NOT NULL DEFAULT '', password_hash TEXT NOT NULL DEFAULT '',
              disabled_at TEXT, created_at TEXT NOT NULL);
             CREATE UNIQUE INDEX idx_platform_users_name ON platform_users(display_name);
             CREATE TABLE platform_quotas (user_id TEXT PRIMARY KEY, tenant_quota INTEGER NOT NULL,
              updated_at TEXT NOT NULL);
             CREATE TABLE tenant_owners (team_id TEXT PRIMARY KEY, user_id TEXT NOT NULL,
              created_at TEXT NOT NULL);
             CREATE TABLE platform_audit (id INTEGER PRIMARY KEY, at TEXT NOT NULL,
              actor TEXT NOT NULL, action TEXT NOT NULL, detail TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn quota_default_enforced_and_raisable() {
        let mut c = testdb();
        let u = create_user(&mut c, "bob", "password-1").unwrap();
        let uid = u["user_id"].as_str().unwrap();
        assert_eq!(tenant_quota(&c, uid).unwrap(), DEFAULT_TENANT_QUOTA);
        // 默认 3：前 3 次过，第 4 次拒
        for i in 0..3 {
            check_tenant_quota(&c, uid).unwrap();
            tag_tenant_owner(&mut c, &format!("t{i}"), uid).unwrap();
        }
        assert!(check_tenant_quota(&c, uid).is_err());
        assert_eq!(owned_teams(&c, uid).unwrap().len(), 3);
        assert!(tenant_owned_by(&c, "t0", uid).unwrap());
        assert!(!tenant_owned_by(&c, "t0", "other").unwrap());
        // 提额后可过；归零即禁建
        set_quota(&mut c, uid, 4).unwrap();
        check_tenant_quota(&c, uid).unwrap();
        assert!(set_quota(&mut c, uid, 101).is_err());
        assert!(set_quota(&mut c, "no-such-user", 1).is_err());
    }
}
