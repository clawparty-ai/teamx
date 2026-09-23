//! 签发台账 O-2（design/014 §4）。
//!
//! server 按 CA 分代枚举自己签发的**有效**证书 —— "废旧 CA" 的前置条件
//! （旧 CA 有效证书数 = 0）必须可断言，否则轮换无法证明完成（owner 附注：
//! 轮换 v1 默认不启用，直到本台账落地）。
//!
//! - 所有经 CA 签发的证书都在**签发处**落一行（`record`）：
//!   平台侧（create_tenant 的 server 证书、reissue 的成员证书、invite 的成员证书、
//!   service 车道的平台服务证书）记平台库；租户 server 侧（租户模式 invite）记租户库。
//! - 有效 = `revoked_at IS NULL AND (expires_at IS NULL OR expires_at > now)`。
//! - `tenant ca-audit` 合并平台行 + 租户 server 行（经 service 车道 `issued.list`），
//!   给出每代 CA 的活数与"能否废旧 CA" verdict。

use rusqlite::{params, Connection};
use serde_json::{json, Value};

/// 本机签发者身份：租户 server Pod（TEAMX_TENANT_TEAM_ID + CA 目录）记 tenant-server，
/// 其余（平台/普通 server）记 platform。台账合并时用 issuer 区分行来源库。
pub fn local_issuer() -> &'static str {
    if crate::pki::tenant_ca_dir().is_some() {
        "tenant-server"
    } else {
        "platform"
    }
}

/// 落一行签发记录。`validity_days` 必须与签发时用的有效期一致
/// （成员/server 证书 3650 天，service 证书 7 天 —— 见 `pki::{CERT_DAYS→pub, SERVICE_CERT_DAYS}`）。
///
/// `serial_hex` 主键：同一证书重复记录（重试/幂等）用 `INSERT OR IGNORE` 去重，
/// 不报错 —— 台账要的是"签发过"，不是"调用次数"。
#[allow(clippy::too_many_arguments)]
pub fn record(
    conn: &Connection,
    serial_hex: &str,
    cn: &str,
    kind: &str,
    role: &str,
    member_id: &str,
    team_id: &str,
    ca_fingerprint: &str,
    validity_days: i64,
    issuer: &str,
) -> Result<(), String> {
    let now = crate::db::now();
    let expires = (chrono::Utc::now() + chrono::Duration::days(validity_days)).to_rfc3339();
    conn.execute(
        "INSERT OR IGNORE INTO issued_certs
           (serial_hex, cn, kind, role, member_id, team_id, ca_fingerprint,
            issued_at, expires_at, revoked_at, issuer)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10)",
        params![
            serial_hex, cn, kind, role, member_id, team_id, ca_fingerprint,
            now, expires, issuer
        ],
    )
    .map_err(|e| format!("issued.record: {e}"))?;
    Ok(())
}

/// 该 CA 的**有效**证书数（O-2 前置断言用）。
pub fn live_count(conn: &Connection, ca_fingerprint: &str) -> Result<i64, String> {
    let now = crate::db::now();
    conn.query_row(
        "SELECT COUNT(*) FROM issued_certs
         WHERE ca_fingerprint = ?1 AND revoked_at IS NULL
           AND (expires_at IS NULL OR expires_at > ?2)",
        params![ca_fingerprint, now],
        |r| r.get(0),
    )
    .map_err(|e| format!("issued.live_count: {e}"))
}

/// 按条件列出台账行（`issued list` / service 车道合并用）。
pub fn list(
    conn: &Connection,
    team_id: Option<&str>,
    ca_fingerprint: Option<&str>,
) -> Result<Vec<Value>, String> {
    let (sql, args): (String, Vec<String>) = match (team_id, ca_fingerprint) {
        (Some(t), Some(c)) => (
            "SELECT serial_hex, cn, kind, role, member_id, team_id, ca_fingerprint,
                    issued_at, expires_at, revoked_at, issuer FROM issued_certs
             WHERE team_id = ?1 AND ca_fingerprint = ?2 ORDER BY issued_at".to_string(),
            vec![t.to_string(), c.to_string()],
        ),
        (Some(t), None) => (
            "SELECT serial_hex, cn, kind, role, member_id, team_id, ca_fingerprint,
                    issued_at, expires_at, revoked_at, issuer FROM issued_certs
             WHERE team_id = ?1 ORDER BY issued_at".to_string(),
            vec![t.to_string()],
        ),
        (None, Some(c)) => (
            "SELECT serial_hex, cn, kind, role, member_id, team_id, ca_fingerprint,
                    issued_at, expires_at, revoked_at, issuer FROM issued_certs
             WHERE ca_fingerprint = ?1 ORDER BY issued_at".to_string(),
            vec![c.to_string()],
        ),
        (None, None) => (
            "SELECT serial_hex, cn, kind, role, member_id, team_id, ca_fingerprint,
                    issued_at, expires_at, revoked_at, issuer FROM issued_certs
             ORDER BY issued_at LIMIT 200".to_string(),
            vec![],
        ),
    };
    let mut stmt = conn.prepare(&sql).map_err(|e| format!("issued.list: {e}"))?;
    let params: Vec<&dyn rusqlite::ToSql> = args.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
    let rows = stmt
        .query_map(params.as_slice(), |r| {
            Ok(json!({
                "serial_hex": r.get::<_, String>(0)?,
                "cn": r.get::<_, String>(1)?,
                "kind": r.get::<_, String>(2)?,
                "role": r.get::<_, String>(3)?,
                "member_id": r.get::<_, String>(4)?,
                "team_id": r.get::<_, String>(5)?,
                "ca_fingerprint": r.get::<_, String>(6)?,
                "issued_at": r.get::<_, String>(7)?,
                "expires_at": r.get::<_, Option<String>>(8)?,
                "revoked_at": r.get::<_, Option<String>>(9)?,
                "issuer": r.get::<_, String>(10)?,
            }))
        })
        .map_err(|e| format!("issued.list: {e}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| format!("issued.list: {e}"))
}

/// 吊销一张证书（ compromising / 提前作废；返回是否命中）。
pub fn revoke(conn: &Connection, serial_hex: &str) -> Result<bool, String> {
    let now = crate::db::now();
    let n = conn
        .execute(
            "UPDATE issued_certs SET revoked_at = ?1 WHERE serial_hex = ?2 AND revoked_at IS NULL",
            params![now, serial_hex],
        )
        .map_err(|e| format!("issued.revoke: {e}"))?;
    Ok(n > 0)
}

/// 合并审计（`tenant ca-audit`）：平台行 + 租户 server 行（调用方经 service 车道取到），
/// 按 CA 分代给活数；`current_ca`（tenants 表当前指纹）之外且活数 > 0 的代 → 不可废。
pub fn audit(
    conn: &Connection,
    team_id: &str,
    current_ca: &str,
    server_rows: &[Value],
) -> Result<Value, String> {
    let platform_rows = list(conn, Some(team_id), None)?;
    // 按 CA 分代合并：serial 去重（同一证书两边不应重复出现；若出现只计一次并标注）。
    use std::collections::BTreeMap;
    let mut per_ca: BTreeMap<String, Value> = BTreeMap::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let now = crate::db::now();
    let live = |r: &Value| {
        r.get("revoked_at").map(|v| v.is_null()).unwrap_or(false)
            && r
                .get("expires_at")
                .map(|e| e.is_null() || e.as_str().map(|s| s > now.as_str()).unwrap_or(false))
                .unwrap_or(true)
    };
    for (src, rows) in [("platform", &platform_rows), ("tenant-server", &server_rows.to_vec())] {
        for r in rows {
            let serial = r.get("serial_hex").and_then(Value::as_str).unwrap_or("").to_string();
            let ca = r.get("ca_fingerprint").and_then(Value::as_str).unwrap_or("").to_string();
            if serial.is_empty() || ca.is_empty() {
                continue;
            }
            let e = per_ca.entry(ca.clone()).or_insert(json!({
                "ca_fingerprint": ca, "live": 0, "total": 0,
                "platform_rows": 0, "server_rows": 0, "current": ca == current_ca,
            }));
            if seen.insert(serial) {
                *e.get_mut("total").unwrap() = json!(e["total"].as_i64().unwrap_or(0) + 1);
                if live(r) {
                    *e.get_mut("live").unwrap() = json!(e["live"].as_i64().unwrap_or(0) + 1);
                }
            }
            let k = if src == "platform" { "platform_rows" } else { "server_rows" };
            *e.get_mut(k).unwrap() = json!(e[k].as_i64().unwrap_or(0) + 1);
        }
    }
    // verdict：非当前代且活数>0 → 不可废（O-2 前置）；server 行缺失要声明（合并口径不完整）。
    let mut can_retire = json!({});
    for (ca, e) in &per_ca {
        let live_n = e["live"].as_i64().unwrap_or(0);
        let is_current = *ca == current_ca;
        let ok = is_current || live_n == 0;
        can_retire[ca] = json!({
            "can_retire": ok,
            "live": live_n,
            "reason": if is_current { "current CA" }
                      else if live_n == 0 { "no live certs" }
                      else { "old CA still has live certs — reissue first" },
        });
    }
    Ok(json!({
        "ok": true,
        "team_id": team_id,
        "current_ca": current_ca,
        "server_rows_merged": server_rows.len(),
        "per_ca": per_ca.values().collect::<Vec<_>>(),
        "can_retire": can_retire,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memdb() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE issued_certs (
               serial_hex TEXT PRIMARY KEY, cn TEXT NOT NULL, kind TEXT NOT NULL,
               role TEXT NOT NULL DEFAULT '', member_id TEXT NOT NULL DEFAULT '',
               team_id TEXT NOT NULL DEFAULT '', ca_fingerprint TEXT NOT NULL,
               issued_at TEXT NOT NULL, expires_at TEXT, revoked_at TEXT,
               issuer TEXT NOT NULL DEFAULT '');",
        )
        .unwrap();
        c
    }

    #[test]
    fn record_live_revoke_roundtrip() {
        let c = memdb();
        record(&c, "aa", "member:m:contributor", "member", "contributor", "m", "t", "ca1", 3650, "platform").unwrap();
        record(&c, "bb", "service:x", "service", "", "", "t", "ca1", 7, "platform-console").unwrap();
        // 幂等：重复记录不翻倍
        record(&c, "aa", "member:m:contributor", "member", "contributor", "m", "t", "ca1", 3650, "platform").unwrap();
        assert_eq!(live_count(&c, "ca1").unwrap(), 2);
        assert!(revoke(&c, "aa").unwrap());
        assert!(!revoke(&c, "aa").unwrap(), "重复吊销返回 false");
        assert_eq!(live_count(&c, "ca1").unwrap(), 1);
        assert_eq!(live_count(&c, "ca-none").unwrap(), 0);
    }

    #[test]
    fn audit_merges_and_verdicts_old_ca() {
        let c = memdb();
        record(&c, "a1", "member:m1:c", "member", "c", "m1", "t", "ca-new", 3650, "platform").unwrap();
        record(&c, "a2", "member:m2:c", "member", "c", "m2", "t", "ca-old", 3650, "platform").unwrap();
        // 租户 server 侧另有一行旧代证书
        let server_rows = vec![json!({
            "serial_hex": "b1", "cn": "member:m3:c", "kind": "member",
            "ca_fingerprint": "ca-old", "revoked_at": Value::Null,
            "expires_at": "2999-01-01T00:00:00+00:00",
        })];
        let v = audit(&c, "t", "ca-new", &server_rows).unwrap();
        assert_eq!(v["can_retire"]["ca-new"]["can_retire"], json!(true));
        assert_eq!(v["can_retire"]["ca-old"]["can_retire"], json!(false));
        assert_eq!(v["can_retire"]["ca-old"]["live"], json!(2));
        assert_eq!(v["server_rows_merged"], json!(1));
        // 吊销 + 过期后旧代可废
        revoke(&c, "a2").unwrap();
        let server_rows2 = vec![json!({
            "serial_hex": "b1", "cn": "member:m3:c", "kind": "member",
            "ca_fingerprint": "ca-old", "revoked_at": "2026-01-01T00:00:00+00:00",
            "expires_at": "2999-01-01T00:00:00+00:00",
        })];
        let v2 = audit(&c, "t", "ca-new", &server_rows2).unwrap();
        assert_eq!(v2["can_retire"]["ca-old"]["can_retire"], json!(true));
    }
}
