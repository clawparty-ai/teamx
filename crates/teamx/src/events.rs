//! Append-only event ledger with per-team monotonic `seq`.
//!
//! The seq is computed and the row inserted inside the same (single-writer)
//! transaction so concurrent writers can never reorder a team's timeline.

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: i64,
    pub team_id: String,
    pub member_id: Option<String>,
    pub seq: i64,
    pub r#type: String,
    pub payload: Option<serde_json::Value>,
    pub created_at: String,
}

/// Insert one ledger event, assigning the next per-team seq. Must be called
/// inside a write transaction (see `db::with_write`).
pub fn emit(
    tx: &mut Transaction,
    team_id: &str,
    member_id: Option<&str>,
    event_type: &str,
    payload: Option<&serde_json::Value>,
) -> rusqlite::Result<i64> {
    let seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM events WHERE team_id = ?1",
        [team_id],
        |r| r.get(0),
    )?;
    let created_at = db_now();
    let payload_json = payload.map(|p| serde_json::to_string(p).unwrap_or_default());
    tx.execute(
        "INSERT INTO events (team_id, member_id, seq, type, payload_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            team_id,
            member_id,
            seq,
            event_type,
            payload_json,
            created_at
        ],
    )?;
    Ok(seq)
}

pub fn db_now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Read events for a team, optionally from a seq cursor (exclusive).
pub fn list(
    conn: &Connection,
    team_id: &str,
    after: Option<i64>,
) -> rusqlite::Result<Vec<Event>> {
    let sql = if after.is_some() {
        "SELECT id, team_id, member_id, seq, type, payload_json, created_at
         FROM events WHERE team_id = ?1 AND seq > ?2 ORDER BY seq ASC"
    } else {
        "SELECT id, team_id, member_id, seq, type, payload_json, created_at
         FROM events WHERE team_id = ?1 ORDER BY seq ASC"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = if let Some(a) = after {
        stmt.query_map(params![team_id, a], row_to_event)?
    } else {
        stmt.query_map(params![team_id], row_to_event)?
    };
    rows.collect()
}

/// Read the most recent `limit` events for a team (newest first), without
/// loading the whole ledger into memory.
pub fn recent(
    conn: &Connection,
    team_id: &str,
    limit: usize,
) -> rusqlite::Result<Vec<Event>> {
    let mut stmt = conn.prepare(
        "SELECT id, team_id, member_id, seq, type, payload_json, created_at
         FROM events WHERE team_id = ?1 ORDER BY seq DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![team_id, limit as i64], row_to_event)?;
    let mut v: Vec<Event> = rows.collect::<rusqlite::Result<_>>()?;
    v.reverse();
    Ok(v)
}

fn row_to_event(r: &rusqlite::Row) -> rusqlite::Result<Event> {
    let payload_json: Option<String> = r.get(5)?;
    Ok(Event {
        id: r.get(0)?,
        team_id: r.get(1)?,
        member_id: r.get(2)?,
        seq: r.get(3)?,
        r#type: r.get(4)?,
        payload: payload_json.and_then(|s| serde_json::from_str(&s).ok()),
        created_at: r.get(6)?,
    })
}

/// Current per-team cursor (max seq) for a session.
pub fn cursor_for(conn: &Connection, session_key: &str, team_id: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT last_seq FROM sync_cursors WHERE session_key = ?1 AND team_id = ?2",
        params![session_key, team_id],
        |r| r.get(0),
    )
    .optional()
    .map(|o| o.unwrap_or(0))
}

/// Advance a session's cursor inside a write transaction. The cursor is
/// monotonic: a concurrent/racing write can never regress it.
pub fn set_cursor(
    tx: &mut Transaction,
    session_key: &str,
    team_id: &str,
    last_seq: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO sync_cursors (session_key, team_id, last_seq) VALUES (?1, ?2, ?3)
         ON CONFLICT(session_key, team_id)
         DO UPDATE SET last_seq = MAX(last_seq, excluded.last_seq)",
        params![session_key, team_id, last_seq],
    )?;
    Ok(())
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

    fn seed_team(conn: &Connection, team_id: &str, member_id: &str) {
        conn.execute(
            "INSERT INTO teams (id, name, owner_member_id, goal_id, state, invite_token, created_at, updated_at)
             VALUES (?1, 'T', ?2, NULL, 'forming', 'tok', 'now', 'now')",
            params![team_id, member_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO members (id, team_id, session_key, display_name, role, state, joined_at)
             VALUES (?1, ?2, 's', 'm', NULL, 'active', 'now')",
            params![member_id, team_id],
        )
        .unwrap();
    }

    #[test]
    fn seq_is_monotonic_and_independent_per_team() {
        let mut conn = test_conn();
        seed_team(&conn, "t1", "m1");
        seed_team(&conn, "t2", "m2");

        // interleave two teams' writes across three transactions
        for i in 0..3 {
            db::with_write(&mut conn, |tx| {
                emit(tx, "t1", Some("m1"), &format!("e{i}"), Some(&serde_json::json!({"i": i})))?;
                emit(tx, "t2", Some("m2"), &format!("e{i}"), None)?;
                Ok(())
            })
            .unwrap();
        }

        let e1 = list(&conn, "t1", None).unwrap();
        let e2 = list(&conn, "t2", None).unwrap();
        let seqs1: Vec<i64> = e1.iter().map(|e| e.seq).collect();
        let seqs2: Vec<i64> = e2.iter().map(|e| e.seq).collect();
        assert_eq!(seqs1, vec![1, 2, 3], "team t1 seq must be 1..3");
        assert_eq!(seqs2, vec![1, 2, 3], "team t2 seq must be independent 1..3");
        assert!(e1.iter().all(|e| e.payload.is_some()));
        assert!(e2.iter().all(|e| e.payload.is_none()));
    }

    #[test]
    fn list_after_cursor_is_exclusive() {
        let mut conn = test_conn();
        seed_team(&conn, "t1", "m1");
        db::with_write(&mut conn, |tx| {
            for i in 1..=5 {
                emit(tx, "t1", Some("m1"), &format!("e{i}"), None)?;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(list(&conn, "t1", None).unwrap().len(), 5);
        let after3 = list(&conn, "t1", Some(3)).unwrap();
        assert_eq!(after3.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![4, 5]);
    }

    #[test]
    fn cursor_advance_is_monotonic() {
        let mut conn = test_conn();
        seed_team(&conn, "t1", "m1");
        db::with_write(&mut conn, |tx| {
            for i in 1..=4 {
                emit(tx, "t1", Some("m1"), &format!("e{i}"), None)?;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(cursor_for(&conn, "s", "t1").unwrap(), 0);
        db::with_write(&mut conn, |tx| set_cursor(tx, "s", "t1", 4)).unwrap();
        assert_eq!(cursor_for(&conn, "s", "t1").unwrap(), 4);
        // a racing lower write must NOT regress the cursor (monotonic advance)
        db::with_write(&mut conn, |tx| set_cursor(tx, "s", "t1", 2)).unwrap();
        assert_eq!(cursor_for(&conn, "s", "t1").unwrap(), 4, "cursor must not regress");
        db::with_write(&mut conn, |tx| set_cursor(tx, "s", "t1", 6)).unwrap();
        assert_eq!(cursor_for(&conn, "s", "t1").unwrap(), 6);
    }

    #[test]
    fn payload_roundtrips_through_json() {
        let mut conn = test_conn();
        seed_team(&conn, "t1", "m1");
        let payload = serde_json::json!({ "message": "进展", "n": 42, "ok": true });
        db::with_write(&mut conn, |tx| {
            emit(tx, "t1", Some("m1"), "progress.published", Some(&payload))
        })
        .unwrap();
        let e = &list(&conn, "t1", None).unwrap()[0];
        assert_eq!(e.payload.as_ref().unwrap(), &payload);
    }
}
