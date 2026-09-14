//! IM action whitelist (P2 foundation, design §5.3): the frozen
//! `method → min_role → source/phase` table that will gate IM write actions
//! from P2 on.
//!
//! Governance (design §5.3, frozen by the access grill):
//! - The whitelist **lives as data** (this table, seeded from the design) so
//!   the permission surface is inspectable; changes are owner decisions
//!   recorded in the ledger — **no runtime hot-editing** (no set/reset RPC).
//! - Enforcement lands with P2 (`approve/deny/verify/reject`); P1 read-only
//!   methods are listed here for completeness but are already guarded by
//!   binding resolution at the RPC layer.
//! - `min_role` values: `member` (any active member), `lead` (owner/co-lead).
//! - 永不进白名单：`goal.close` / `revoke` / `destroy`（红线，§2）；
//!   `role` / `tunnel` 默认不进。

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

/// The design-frozen whitelist rows, seeded idempotently at migrate time.
pub const SEED: &[(&str, &str, &str, &str)] = &[
    // P1 — read-only + binding self-service (already shipped).
    ("im.query", "member", "binding", "P1"),
    ("im.bindings", "member", "binding", "P1"),
    ("im.resolve", "lead", "binding", "P1"),
    // P2 — write actions (interactive cards, double confirmation).
    ("im.approve", "lead", "gateway", "P2"),
    ("im.deny", "lead", "gateway", "P2"),
    ("im.verify", "lead", "gateway", "P2"),
    ("im.reject", "lead", "gateway", "P2"),
    // P2.5 — self-service answers to one's own questions.
    ("im.respond", "member", "gateway", "P2.5"),
    // P3 — productization (lead-scoped publishing/assignment).
    ("im.publish", "lead", "gateway", "P3"),
    ("im.task_assign", "lead", "gateway", "P3"),
];

/// Seed the frozen whitelist rows (idempotent; runs at migrate time).
pub fn seed(conn: &Connection) -> rusqlite::Result<()> {
    for (action, min_role, source, phase) in SEED {
        conn.execute(
            "INSERT OR IGNORE INTO im_action_whitelist (action, min_role, source, phase)
             VALUES (?1, ?2, ?3, ?4)",
            params![action, min_role, source, phase],
        )?;
    }
    Ok(())
}

/// The minimum role required for `action`, if it is whitelisted at all.
pub fn min_role_for(conn: &Connection, action: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT min_role FROM im_action_whitelist WHERE action = ?1",
        params![action],
        |r| r.get(0),
    )
    .optional()
}

/// Read-only listing (any active team member; the table is not secret).
pub fn list(conn: &Connection) -> Result<Value, String> {
    let mut stmt = conn
        .prepare("SELECT action, min_role, source, phase FROM im_action_whitelist ORDER BY phase, action")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(json!({
                "action": r.get::<_, String>(0)?,
                "min_role": r.get::<_, String>(1)?,
                "source": r.get::<_, String>(2)?,
                "phase": r.get::<_, String>(3)?,
            }))
        })
        .map_err(|e| e.to_string())?;
    let items: Vec<Value> = rows
        .collect::<rusqlite::Result<_>>()
        .map_err(|e| e.to_string())?;
    Ok(json!({ "ok": true, "whitelist": items, "frozen": true }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.pragma_update(None, "journal_mode", "WAL").ok();
        db::migrate(&c).unwrap();
        c
    }

    #[test]
    fn seed_is_idempotent_and_covers_the_frozen_surface() {
        let c = conn();
        seed(&c).unwrap();
        seed(&c).unwrap(); // second run must not duplicate
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM im_action_whitelist", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n as usize, SEED.len(), "seed idempotent");
        // Design red lines: high-risk actions never appear.
        for banned in ["goal.close", "revoke", "destroy", "role.set", "tunnel.expose"] {
            assert!(
                min_role_for(&c, banned).unwrap().is_none(),
                "{banned} must never be whitelisted"
            );
        }
        // P2 write actions are lead-scoped; P1 reads are member-scoped.
        assert_eq!(min_role_for(&c, "im.approve").unwrap().as_deref(), Some("lead"));
        assert_eq!(min_role_for(&c, "im.query").unwrap().as_deref(), Some("member"));
        assert_eq!(min_role_for(&c, "im.respond").unwrap().as_deref(), Some("member"));
        // Unknown action → None (P2 enforcement treats it as deny-by-default).
        assert!(min_role_for(&c, "im.nonsense").unwrap().is_none());
    }

    #[test]
    fn list_returns_the_frozen_table() {
        let c = conn();
        seed(&c).unwrap();
        let v = list(&c).unwrap();
        let items = v["whitelist"].as_array().unwrap();
        assert_eq!(items.len(), SEED.len());
        assert_eq!(v["frozen"], json!(true));
        let first = &items[0];
        assert!(first["action"].as_str().unwrap().starts_with("im."));
    }
}
