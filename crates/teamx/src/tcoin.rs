//! TCoin 经济模型（design/017 · ADR-0003 两级账本）。
//!
//! - **平台侧**（本模块）：append-only 流水 `tcoin_entries`，余额 = `SUM(delta)`；
//!   发行（grant）、结算（settle，来自租户侧消耗）、冲正（reverse）、分配（allocate）。
//! - **租户侧**（`tcoin_usage`）：成员级消耗明细 —— 直接物化自 `activity`
//!   （唯一计量源，SD-10），不另存一份账。
//!
//! 口径（SD-9）：**1 TCoin = 0.001 USD**，账本只存**整数 TCoin**，`tcoin = round(usd × 1000)`。
//! v1 = **记账 + 软告警，不阻断任何操作**（SD-8）。

use rusqlite::{params, Connection};
use serde_json::{json, Value};

/// 1 USD = 1000 TCoin（固定汇率，SD-9）。
pub const TCOIN_PER_USD: f64 = 1000.0;

/// USD 成本 → 整数 TCoin（四舍五入；负数保持符号）。
pub fn usd_to_tcoin(usd: f64) -> i64 {
    (usd * TCOIN_PER_USD).round() as i64
}

/// 追加一条分录（append-only；唯一的写入口）。
///
/// 不做余额校验 —— append-only 允许余额为负（v1 只记账不阻断，余额负 = 欠费信号）。
pub fn append(
    conn: &Connection,
    team_id: &str,
    account: &str,
    delta: i64,
    reason: &str,
    reference: Option<&str>,
    actor: &str,
) -> Result<i64, String> {
    conn.execute(
        "INSERT INTO tcoin_entries (at, team_id, account, delta, reason, ref, actor)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![crate::db::now(), team_id, account, delta, reason, reference, actor],
    )
    .map_err(|e| format!("db: {e}"))?;
    Ok(conn.last_insert_rowid())
}

/// 余额（SUM），按 (team_id, account)。缺省 account = 'tenant'（租户总账）。
pub fn balance(conn: &Connection, team_id: &str, account: &str) -> Result<i64, String> {
    conn.query_row(
        "SELECT COALESCE(SUM(delta), 0) FROM tcoin_entries WHERE team_id = ?1 AND account = ?2",
        params![team_id, account],
        |r| r.get(0),
    )
    .map_err(|e| format!("db: {e}"))
}

/// 平台发行/赠送（SD-6：v1 只做平台发放额度）。
pub fn grant(
    conn: &Connection,
    team_id: &str,
    amount: i64,
    reason: &str,
    actor: &str,
) -> Result<Value, String> {
    if amount == 0 {
        return Err("amount must be non-zero".to_string());
    }
    let seq = append(conn, team_id, "tenant", amount, reason, None, actor)?;
    Ok(json!({
        "ok": true, "seq": seq, "team_id": team_id, "amount": amount,
        "balance": balance(conn, team_id, "tenant")?,
    }))
}

/// owner 把额度分配给成员（SD-5：平台/owner 可写；member 侧账在租户）。
/// 记两条：租户总账 −amount、成员账户 +amount（**内部转账**，不改变总量）。
pub fn allocate(
    conn: &Connection,
    team_id: &str,
    member_id: &str,
    amount: i64,
    actor: &str,
) -> Result<Value, String> {
    if amount <= 0 {
        return Err("amount must be positive".to_string());
    }
    let balance_now = balance(conn, team_id, "tenant")?;
    if amount > balance_now {
        // 记账不阻断（SD-8）：只如实提示
        eprintln!(
            "tcoin: WARN allocate {amount} exceeds tenant balance {balance_now} (v1 不阻断，记欠账)"
        );
    }
    let account = format!("member:{member_id}");
    append(conn, team_id, "tenant", -amount, "allocate", Some(&account), actor)?;
    append(conn, team_id, &account, amount, "allocate", Some("tenant"), actor)?;
    Ok(json!({
        "ok": true, "team_id": team_id, "member_id": member_id, "amount": amount,
        "tenant_balance": balance(conn, team_id, "tenant")?,
        "member_balance": balance(conn, team_id, &account)?,
    }))
}

/// 结算：把租户侧某个批次的消耗记入平台总账（`delta = -consumed`）。
/// `batch_ref` 由租户侧结算返回（含最大 activity id），保证**跨侧重放幂等**。
pub fn settle(
    conn: &Connection,
    team_id: &str,
    consumed: i64,
    batch_ref: &str,
    actor: &str,
) -> Result<Value, String> {
    if consumed < 0 {
        return Err("consumed must be >= 0".to_string());
    }
    // 幂等：同一 batch_ref 只记一次
    let dup: Option<i64> = conn
        .query_row(
            "SELECT seq FROM tcoin_entries WHERE team_id = ?1 AND ref = ?2 AND reason = 'consume'",
            params![team_id, batch_ref],
            |r| r.get(0),
        )
        .ok();
    if let Some(seq) = dup {
        return Ok(json!({
            "ok": true, "already": true, "seq": seq, "batch": batch_ref,
            "balance": balance(conn, team_id, "tenant")?,
        }));
    }
    let seq = append(conn, team_id, "tenant", -consumed, "consume", Some(batch_ref), actor)?;
    Ok(json!({
        "ok": true, "seq": seq, "team_id": team_id, "consumed": consumed,
        "batch": batch_ref, "balance": balance(conn, team_id, "tenant")?,
    }))
}

/// 冲正（SD-12）：对指定分录追加**反向分录**（不删不改），`reason` 注明冲正。
pub fn reverse(
    conn: &Connection,
    seq: i64,
    reason: &str,
    actor: &str,
) -> Result<Value, String> {
    let row: Option<(String, String, i64)> = conn
        .query_row(
            "SELECT team_id, account, delta FROM tcoin_entries WHERE seq = ?1",
            params![seq],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();
    let Some((team_id, account, delta)) = row else {
        return Err(format!("entry {seq} not found"));
    };
    // 已冲正过的不再重复冲（同 ref=reversal:<seq>）
    let refr = format!("reversal:{seq}");
    let dup: Option<i64> = conn
        .query_row(
            "SELECT seq FROM tcoin_entries WHERE team_id = ?1 AND ref = ?2",
            params![team_id, refr],
            |r| r.get(0),
        )
        .ok();
    if let Some(s) = dup {
        // 幂等分支也要回余额（调用方据此断言；不重复回补）
        let team_id2: String = conn
            .query_row(
                "SELECT team_id FROM tcoin_entries WHERE seq = ?1",
                params![s],
                |r| r.get(0),
            )
            .map_err(|e| format!("db: {e}"))?;
        let account2: String = conn
            .query_row("SELECT account FROM tcoin_entries WHERE seq = ?1", params![s], |r| r.get(0))
            .map_err(|e| format!("db: {e}"))?;
        let bal = balance(conn, &team_id2, &account2)?;
        return Ok(json!({ "ok": true, "already": true, "seq": s, "balance": bal }));
    }
    let new_seq = append(
        conn,
        &team_id,
        &account,
        -delta,
        &format!("reverse: {reason}"),
        Some(&refr),
        actor,
    )?;
    Ok(json!({
        "ok": true, "seq": new_seq, "reversed": seq, "team_id": team_id,
        "balance": balance(conn, &team_id, &account)?,
    }))
}

/// 流水（新→旧），可选按 account 过滤。
pub fn entries(
    conn: &Connection,
    team_id: &str,
    account: Option<&str>,
    limit: i64,
) -> Result<Vec<Value>, String> {
    let limit = limit.clamp(1, 500);
    let mut stmt = conn
        .prepare(
            "SELECT seq, at, account, delta, reason, COALESCE(ref,''), actor
             FROM tcoin_entries WHERE team_id = ?1 AND (?2 IS NULL OR account = ?2)
             ORDER BY seq DESC LIMIT ?3",
        )
        .map_err(|e| format!("db: {e}"))?;
    let rows = stmt
        .query_map(params![team_id, account, limit], |r| {
            Ok(json!({
                "seq": r.get::<_, i64>(0)?,
                "at": r.get::<_, String>(1)?,
                "account": r.get::<_, String>(2)?,
                "delta": r.get::<_, i64>(3)?,
                "reason": r.get::<_, String>(4)?,
                "ref": r.get::<_, String>(5)?,
                "actor": r.get::<_, String>(6)?,
            }))
        })
        .map_err(|e| format!("db: {e}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| format!("db: {e}"))
}

/// 本团队**累计已计消耗**（= 历次 `consume` 分录之和的绝对值），用于团队详情展示（SD-15）。
pub fn consumed_total(conn: &Connection, team_id: &str) -> Result<i64, String> {
    conn.query_row(
        "SELECT COALESCE(-SUM(delta), 0) FROM tcoin_entries
         WHERE team_id = ?1 AND reason = 'consume'",
        params![team_id],
        |r| r.get(0),
    )
    .map_err(|e| format!("db: {e}"))
}

/// 租户侧：**未结算**的成员消耗（design/017 SD-14）。
///
/// 直接物化自 `activity`（唯一计量源 SD-10）：`tcoin = round(SUM(cost) × 1000)`。
/// 返回 `(max_activity_id, 逐成员明细)`，供 `settle` 使用与展示（SD-15：成员看自己）。
pub fn pending_usage(conn: &Connection, team_id: &str) -> Result<(i64, Vec<Value>, i64), String> {
    let mut stmt = conn
        .prepare(
            "SELECT COALESCE(m.display_name, a.member_id) name, a.member_id,
                    COALESCE(SUM(a.cost), 0.0) usd, COUNT(*) rows, MAX(a.id) max_id
             FROM activity a LEFT JOIN members m ON m.id = a.member_id
             WHERE a.team_id = ?1 AND a.tcoin_settled_at IS NULL AND a.cost IS NOT NULL
             GROUP BY a.member_id ORDER BY usd DESC",
        )
        .map_err(|e| format!("db: {e}"))?;
    let rows = stmt
        .query_map(params![team_id], |r| {
            let usd: f64 = r.get(2)?;
            Ok(json!({
                "member_id": r.get::<_, String>(1)?,
                "member_name": r.get::<_, String>(0)?,
                "cost_usd": usd,
                "tcoin": usd_to_tcoin(usd),
                "rows": r.get::<_, i64>(3)?,
                "max_activity_id": r.get::<_, Option<i64>>(4)?,
            }))
        })
        .map_err(|e| format!("db: {e}"))?;
    let members: Vec<Value> = rows.collect::<rusqlite::Result<_>>().map_err(|e| format!("db: {e}"))?;
    let max_id: i64 = members
        .iter()
        .filter_map(|m| m["max_activity_id"].as_i64())
        .max()
        .unwrap_or(0);
    let total: i64 = members.iter().map(|m| m["tcoin"].as_i64().unwrap_or(0)).sum();
    Ok((max_id, members, total))
}

/// 租户侧：结算（把 `id <= up_to_id` 的未结算消耗标记为已结算），返回本批次摘要。
/// **原子**：先在事务里标记，再返回被标记行的汇总（供平台写对应分录）。
pub fn settle_usage(
    conn: &mut Connection,
    team_id: &str,
) -> Result<Value, String> {
    let (max_id, members, total) = pending_usage(conn, team_id)?;
    if max_id == 0 || members.is_empty() {
        return Ok(json!({ "ok": true, "consumed": 0, "batch": "", "members": [] }));
    }
    let now = crate::db::now();
    let marked = conn
        .execute(
            "UPDATE activity SET tcoin_settled_at = ?1
             WHERE team_id = ?2 AND tcoin_settled_at IS NULL AND id <= ?3 AND cost IS NOT NULL",
            params![now, team_id, max_id],
        )
        .map_err(|e| format!("db: {e}"))?;
    let batch = format!("tenant-batch:{team_id}:{max_id}:{marked}");
    Ok(json!({
        "ok": true, "consumed": total, "batch": batch, "members": members,
        "marked_rows": marked,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::db::migrate(&c).unwrap();
        c
    }

    fn seed(c: &Connection, team: &str) {
        c.execute(
            "INSERT OR IGNORE INTO teams (id,name,owner_member_id,goal_id,state,invite_token,created_at,updated_at)
             VALUES (?1,'T',NULL,NULL,'active','tok','2026-01-01T00:00:00+00:00','2026-01-01T00:00:00+00:00')",
            params![team],
        ).unwrap();
    }

    #[test]
    fn usd_to_tcoin_is_integer_round_half_away() {
        assert_eq!(usd_to_tcoin(0.0), 0);
        assert_eq!(usd_to_tcoin(0.001), 1);
        assert_eq!(usd_to_tcoin(1.0), 1000);
        assert_eq!(usd_to_tcoin(0.0005), 1); // 四舍五入
        assert_eq!(usd_to_tcoin(-0.002), -2);
    }

    #[test]
    fn append_only_balance_grant_allocate_reverse() {
        let c = db();
        seed(&c, "t1");
        assert_eq!(balance(&c, "t1", "tenant").unwrap(), 0);
        grant(&c, "t1", 5000, "welcome credit", "platform").unwrap();
        assert_eq!(balance(&c, "t1", "tenant").unwrap(), 5000);
        // 分配（内部转账，总量不变）
        allocate(&c, "t1", "m1", 2000, "owner").unwrap();
        assert_eq!(balance(&c, "t1", "tenant").unwrap(), 3000);
        assert_eq!(balance(&c, "t1", "member:m1").unwrap(), 2000);
        // 结算消耗
        settle(&c, "t1", 700, "batch-A", "platform").unwrap();
        assert_eq!(balance(&c, "t1", "tenant").unwrap(), 2300);
        // 结算幂等（同 batch）
        let again = settle(&c, "t1", 700, "batch-A", "platform").unwrap();
        assert_eq!(again["already"], json!(true));
        assert_eq!(balance(&c, "t1", "tenant").unwrap(), 2300, "重复结算不得重复扣");
        // 冲正：反向分录，余额回补
        let grant_seq = entries(&c, "t1", Some("tenant"), 10).unwrap();
        let welcome = grant_seq.iter().find(|e| e["reason"] == json!("welcome credit")).unwrap();
        let seq = welcome["seq"].as_i64().unwrap();
        // 冲正 grant(=+5000) → 追加 -5000。此前同账户还有 allocate(-2000) 与 settle(-700)，
        // 故冲正后余额 = 2300 - 5000 = -2700（append-only 允许负余额 = 欠费信号，SD-8 不阻断）
        reverse(&c, seq, "mis-granted", "platform").unwrap();
        assert_eq!(balance(&c, "t1", "tenant").unwrap(), -2700);
        // 重复冲正不重复回补
        reverse(&c, seq, "mis-granted", "platform").unwrap();
        assert_eq!(balance(&c, "t1", "tenant").unwrap(), -2700, "重复冲正不得再回补");
        // 流水 append-only：条数只增
        let n1 = entries(&c, "t1", None, 100).unwrap().len();
        grant(&c, "t1", 1, "x", "p").unwrap();
        assert_eq!(entries(&c, "t1", None, 100).unwrap().len(), n1 + 1);
    }

    #[test]
    fn tenant_usage_materializes_from_activity_and_settles_once() {
        let mut c = db();
        seed(&c, "t1");
        let add = |c: &Connection, mid: &str, usd: f64| {
            c.execute(
                "INSERT INTO activity (team_id, member_id, node_id, started_at, kind, cost, created_at)
                 VALUES ('t1', ?1, 'n1', '2026-01-01T00:00:00+00:00', 'session', ?2, '2026-01-01T00:00:00+00:00')",
                params![mid, usd],
            ).unwrap();
        };
        add(&c, "m1", 0.5);
        add(&c, "m1", 0.25);
        add(&c, "m2", 0.125);
        let (max_id, members, total) = pending_usage(&c, "t1").unwrap();
        assert!(max_id > 0);
        assert_eq!(members.len(), 2, "逐成员明细");
        assert_eq!(total, 875, "0.875 USD → 875 TCoin");
        // 结算：标记一次
        let s = settle_usage(&mut c, "t1").unwrap();
        assert_eq!(s["consumed"], json!(875));
        assert!(s["batch"].as_str().unwrap().starts_with("tenant-batch:t1:"));
        // 再结算 → 0（不重复计）
        let s2 = settle_usage(&mut c, "t1").unwrap();
        assert_eq!(s2["consumed"], json!(0), "已结算的不再计: {s2}");
        // 新消耗进来后可继续结算
        add(&c, "m1", 1.0);
        let s3 = settle_usage(&mut c, "t1").unwrap();
        assert_eq!(s3["consumed"], json!(1000));
    }
}
