//! Outbound IM notifier (design `docs/design/im-access-requirements.md` §5.4,
//! detail in `docs/57-design-im-outbound-notifier.cn.md`).
//!
//! Watches the team ledger and pushes whitelisted events to IM through an
//! [`ImProvider`]. Runs as a background task inside `serve` (P1: built-in;
//! the standalone gateway process is a P2 concern).
//!
//! Guarantees (design §5.4 / §8):
//! - **不丢不重**：`im_outbox` is keyed by the ledger `(team_id, seq)`; a
//!   restart re-collects at worst a no-op (PRIMARY KEY dedup) and the
//!   collection cursor is persisted in `im_state`.
//! - **digest 聚合**：informational events of the same `(team, event_type)`
//!   within a 60 s window (or 20 events, whichever comes first) render as
//!   ONE message. Management-action results (`direct` rows) are pushed one
//!   by one, immediately — 逐条必达.
//! - **尊重 429**：a provider rate-limit error backs the flusher off
//!   exponentially (60 s doubling, capped at 15 min); pending rows are kept,
//!   never dropped, and 429s do not count toward the failure cap.
//! - **封闭枚举**：only whitelisted event types are ever sent; heartbeat-ish
//!   traffic (`progress.published` with `kind: "session.idle"`) is never
//!   collected. Extending the whitelist is a code change + review.
//! - **IM 故障不阻塞 server**：all work happens on the notifier's own
//!   connection usage inside the server's DB mutex, errors are logged, and
//!   the flusher gives up per-row after [`MAX_ATTEMPTS`] with an
//!   `im.push_failed` audit event (成功/失败同等留痕, §7).

use std::sync::Arc;

use chrono::DateTime;
use rusqlite::{params, Connection, OptionalExtension};

use crate::events;
use crate::im::providers::ImProvider;
use crate::im::types::{ImChannel, ImMessage};

/// Digest window (seconds): informational events younger than this are held
/// back so bursts aggregate into a single message (design §5.4: 60 s,
/// key = (team, event_type)).
pub const DIGEST_WINDOW_SECS: i64 = 60;
/// A digest group this large flushes immediately without waiting for the
/// window (anti-unbounded-memory guard).
pub const DIGEST_BATCH_MAX: usize = 20;
/// Per-row send attempts before the row is marked `failed` and audited as
/// `im.push_failed`. 429 backoffs do NOT count toward this cap.
pub const MAX_ATTEMPTS: i64 = 3;
/// Collect at most this many new ledger events per pass.
const COLLECT_BATCH: i64 = 500;
/// 429 backoff: 60 s doubling, capped at 15 min.
const BACKOFF_BASE_SECS: i64 = 60;
const BACKOFF_MAX_SECS: i64 = 900;

/// Informational event types — digest-aggregated. Closed enum: adding a type
/// is a reviewed code change (design §5.5 治理冻结).
const DIGEST_TYPES: &[&str] = &[
    "progress.published",
    "decision.broadcast",
    "doc.created",
    "doc.claimed",
    "doc.done",
    "doc.updated",
    "doc.acknowledged",
    "doc.verified",
    "doc.rejected",
    "goal.state_changed",
    "member.joined",
    "member.left",
    "member.state_changed",
    "system.nudge",
    "system.nudge_lead",
];

/// Management/action-ish event types — pushed one by one, immediately
/// (逐条必达). Closed enum as above.
const DIRECT_TYPES: &[&str] = &["doc.help_requested", "task.nudge"];

fn is_digestable(event_type: &str) -> bool {
    DIGEST_TYPES.contains(&event_type)
}

fn is_direct(event_type: &str) -> bool {
    DIRECT_TYPES.contains(&event_type)
}

/// Heartbeat filter: `progress.published` events whose payload carries
/// `kind: "session.idle"` are presence noise and must never reach IM
/// (design §5.5: 心跳类永不进枚举).
fn is_heartbeat(event_type: &str, payload: Option<&serde_json::Value>) -> bool {
    event_type == "progress.published"
        && payload
            .and_then(|p| p.get("kind"))
            .and_then(|k| k.as_str())
            .map(|k| k == "session.idle")
            .unwrap_or(false)
}

/// One flush pass result (for logs + tests).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct NotifyStats {
    pub collected: usize,
    pub pushed: usize,
    pub failed: usize,
    pub deferred: usize,
}

/// Outbound notifier. `provider` is the transport seam (dry-run honoured
/// inside the provider); `groups` are the target IM group chat ids.
pub struct Notifier {
    pub groups: Vec<String>,
    pub provider: Arc<dyn ImProvider + Send + Sync>,
    /// Digest aggregation window in seconds (design default 60). Configurable
    /// (`TEAMX_IM_DIGEST_SECS`) so acceptance tests can shrink it.
    pub digest_window_secs: i64,
}

impl Notifier {
    /// Run one collect + flush pass against the server DB. `now` is unix
    /// seconds (injected for testability). Never returns an error for
    /// per-row send failures — those are recorded on the rows / audited.
    pub fn run_once(&self, conn: &mut Connection, now: i64) -> NotifyStats {
        let mut stats = NotifyStats { collected: self.collect(conn), ..Default::default() };
        if self.groups.is_empty() {
            return stats; // no target configured: collect-only, nothing to flush
        }
        // 429 backoff gate: while backing off we only collect, never send.
        let backoff_until: i64 = self
            .state_get(conn, "im_push_backoff_until")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if now < backoff_until {
            return stats;
        }
        self.flush_direct(conn, now, &mut stats);
        self.flush_digests(conn, now, &mut stats);
        stats
    }

    // ------------------------------------------------------------------
    // Collect: ledger -> im_outbox
    // ------------------------------------------------------------------

    fn state_get(&self, conn: &Connection, key: &str) -> Option<String> {
        conn.query_row(
            "SELECT value FROM im_state WHERE key = ?1",
            params![key],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    fn state_set(&self, conn: &Connection, key: &str, value: &str) {
        let _ = conn.execute(
            "INSERT INTO im_state (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        );
    }

    /// Read new ledger events past the persisted cursor and enqueue
    /// whitelisted ones. The cursor advances even over skipped events (the
    /// enum is closed; a skipped type must not be retried forever).
    fn collect(&self, conn: &mut Connection) -> usize {
        let cursor: i64 = self
            .state_get(conn, "notifier_cursor")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let rows: Vec<(i64, String, i64, String, Option<String>)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT id, team_id, seq, type, payload_json FROM events
                     WHERE id > ?1 ORDER BY id ASC LIMIT ?2",
                )
                .expect("im notifier: prepare collect");
            let it = stmt.query_map(params![cursor, COLLECT_BATCH], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                ))
            });
            match it {
                Ok(it) => it.filter_map(|r| r.ok()).collect(),
                Err(_) => Vec::new(),
            }
        };
        let mut collected = 0usize;
        for (id, team_id, seq, event_type, payload_json) in rows {
            let payload: Option<serde_json::Value> = payload_json
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok());
            let whitelisted = is_digestable(&event_type) || is_direct(&event_type);
            if whitelisted && !is_heartbeat(&event_type, payload.as_ref()) {
                let direct = is_direct(&event_type) as i64;
                // PRIMARY KEY dedup makes a re-collect after restart a no-op
                // (不重); events are append-only so the snapshot is stable.
                let _ = conn.execute(
                    "INSERT OR IGNORE INTO im_outbox
                       (team_id, seq, event_type, payload_json, direct, state, attempts, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7)",
                    params![
                        team_id,
                        seq,
                        event_type,
                        payload_json,
                        direct,
                        if direct == 1 { "direct" } else { "pending" },
                        events::db_now(),
                    ],
                );
                collected += 1;
            }
            self.state_set(conn, "notifier_cursor", &id.to_string());
        }
        collected
    }

    // ------------------------------------------------------------------
    // Flush
    // ------------------------------------------------------------------

    fn send_to_groups(&self, body: &str) -> Result<(), String> {
        for g in &self.groups {
            let msg = ImMessage {
                channel: ImChannel::Group(g.clone()),
                title: Some("teamx".into()),
                body: body.to_string(),
                markdown: false,
            };
            self.provider.send(&msg)?;
        }
        Ok(())
    }

    /// Send one message covering `seqs`; on success mark the rows pushed and
    /// audit `im.push`. Returns Ok(true) if pushed, Ok(false) if deferred
    /// (nothing to send / backoff), Err(()) never — errors are recorded.
    fn send_rows(
        &self,
        conn: &mut Connection,
        team_id: &str,
        event_type: &str,
        digest: bool,
        rows: &[(i64, Option<String>)],
        now: i64,
    ) -> Result<bool, String> {
        let seqs: Vec<i64> = rows.iter().map(|(s, _)| *s).collect();
        let body = if digest {
            render_digest_body(event_type, &seqs)
        } else {
            render_direct_body(event_type, seqs[0], rows[0].1.as_deref())
        };
        match self.send_to_groups(&body) {
            Ok(()) => {
                let channels: Vec<String> =
                    self.groups.iter().map(|g| format!("group:{g}")).collect();
                let payload = serde_json::json!({
                    "team_id": team_id,
                    "platform": self.provider.platform(),
                    "channels": channels,
                    "event_type": event_type,
                    "seqs": seqs,
                    "digest": digest,
                    "count": seqs.len(),
                    "dry_run": std::env::var("TEAMX_IM_DRY_RUN")
                        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                        .unwrap_or(true),
                });
                // Mark + audit in one transaction; the audit event itself is
                // `im.*` and therefore never re-collected (no loop).
                crate::db::with_write(conn, |tx| {
                    for (seq, _) in rows {
                        tx.execute(
                            "UPDATE im_outbox SET state='pushed', pushed_at=?3
                              WHERE team_id=?1 AND seq=?2",
                            params![team_id, seq, events::db_now()],
                        )?;
                    }
                    events::emit(tx, team_id, None, "im.push", Some(&payload))?;
                    Ok(())
                })
                .map_err(|e| format!("db: {e}"))?;
                // A successful send resets the 429 streak.
                self.state_set(conn, "im_push_429_streak", "0");
                Ok(true)
            }
            Err(e) if e.contains("429") => {
                // Rate limited: back off exponentially; rows stay pending and
                // a 429 never counts toward the failure cap (尊重 429).
                let streak: i64 = self
                    .state_get(conn, "im_push_429_streak")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0)
                    + 1;
                let wait = (BACKOFF_BASE_SECS * (1 << (streak - 1).min(10))).min(BACKOFF_MAX_SECS);
                let until = now + wait;
                self.state_set(conn, "im_push_429_streak", &streak.to_string());
                self.state_set(conn, "im_push_backoff_until", &until.to_string());
                eprintln!("teamx im notifier: 429 from provider, backing off {wait}s");
                Err(e)
            }
            Err(e) => {
                let attempts: i64 = {
                    let mut cur = 0i64;
                    for (seq, _) in rows {
                        let _ = conn.execute(
                            "UPDATE im_outbox
                                SET attempts = attempts + 1,
                                    last_error = ?3
                              WHERE team_id=?1 AND seq=?2",
                            params![team_id, seq, e],
                        );
                        cur = conn
                            .query_row(
                                "SELECT attempts FROM im_outbox WHERE team_id=?1 AND seq=?2",
                                params![team_id, seq],
                                |r| r.get(0),
                            )
                            .unwrap_or(0);
                    }
                    cur
                };
                if attempts >= MAX_ATTEMPTS {
                    let payload = serde_json::json!({
                        "team_id": team_id,
                        "platform": self.provider.platform(),
                        "event_type": event_type,
                        "seqs": seqs,
                        "reason": e,
                        "retry_count": attempts,
                    });
                    crate::db::with_write(conn, |tx| {
                        for (seq, _) in rows {
                            tx.execute(
                                "UPDATE im_outbox SET state='failed' WHERE team_id=?1 AND seq=?2",
                                params![team_id, seq],
                            )?;
                        }
                        events::emit(tx, team_id, None, "im.push_failed", Some(&payload))?;
                        Ok(())
                    })
                    .map_err(|e| format!("db: {e}"))?;
                    eprintln!("teamx im notifier: push failed permanently: {e}");
                } else {
                    eprintln!("teamx im notifier: push attempt {attempts}/{MAX_ATTEMPTS} failed: {e}");
                }
                Err(e)
            }
        }
    }

    fn flush_direct(&self, conn: &mut Connection, now: i64, stats: &mut NotifyStats) {
        let rows: Vec<(String, i64, String, Option<String>)> = {
            let mut stmt = match conn.prepare(
                "SELECT team_id, seq, event_type, payload_json FROM im_outbox
                  WHERE state = 'direct' ORDER BY team_id, seq LIMIT 50",
            ) {
                Ok(s) => s,
                Err(_) => return,
            };
            let it = stmt.query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            });
            match it {
                Ok(it) => it.filter_map(|r| r.ok()).collect(),
                Err(_) => Vec::new(),
            }
        };
        for (team_id, seq, event_type, payload_json) in rows {
            match self.send_rows(conn, &team_id, &event_type, false, &[(seq, payload_json)], now) {
                Ok(true) => stats.pushed += 1,
                Ok(false) => stats.deferred += 1,
                Err(e) if e.contains("429") => {
                    stats.deferred += 1;
                    return; // backed off: stop flushing this pass
                }
                Err(_) => {
                    stats.failed += 1; // terminal-vs-retry bookkeeping lives in send_rows
                }
            }
        }
    }

    fn flush_digests(&self, conn: &mut Connection, now: i64, stats: &mut NotifyStats) {
        // Group pending (digestable) rows by (team_id, event_type).
        let rows: Vec<(String, i64, String, String)> = {
            let mut stmt = match conn.prepare(
                "SELECT team_id, seq, event_type, created_at FROM im_outbox
                  WHERE state = 'pending' ORDER BY team_id, event_type, seq LIMIT 1000",
            ) {
                Ok(s) => s,
                Err(_) => return,
            };
            let it = stmt.query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            });
            match it {
                Ok(it) => it.filter_map(|r| r.ok()).collect(),
                Err(_) => Vec::new(),
            }
        };
        let mut groups: Vec<DigestGroup> = Vec::new();
        for (team_id, seq, event_type, created_at) in rows {
            match groups
                .iter_mut()
                .find(|(t, et, _)| t == &team_id && et == &event_type)
            {
                Some((_, _, v)) => v.push((seq, created_at)),
                None => groups.push((team_id, event_type, vec![(seq, created_at)])),
            }
        }
        for (team_id, event_type, members) in groups {
            let oldest = members
                .iter()
                .map(|(_, ca)| created_at_epoch(ca).unwrap_or(i64::MAX))
                .min()
                .unwrap_or(i64::MAX);
            let aged = now - oldest >= self.digest_window_secs;
            let burst = members.len() >= DIGEST_BATCH_MAX;
            if !aged && !burst {
                stats.deferred += members.len();
                continue;
            }
            let rows: Vec<(i64, Option<String>)> = {
                let mut stmt = match conn.prepare(
                    "SELECT seq, payload_json FROM im_outbox
                      WHERE team_id=?1 AND event_type=?2 AND state='pending' ORDER BY seq",
                ) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let it = stmt.query_map(params![team_id, event_type], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                });
                match it {
                    Ok(it) => it.filter_map(|r| r.ok()).collect(),
                    Err(_) => continue,
                }
            };
            match self.send_rows(conn, &team_id, &event_type, true, &rows, now) {
                Ok(true) => stats.pushed += rows.len(),
                Ok(false) => stats.deferred += rows.len(),
                Err(e) if e.contains("429") => {
                    stats.deferred += rows.len();
                    return; // backed off: stop flushing this pass
                }
                Err(_) => stats.failed += rows.len(),
            }
        }
    }
}

/// One digest group: (team_id, event_type, [(seq, created_at), ...]).
type DigestGroup = (String, String, Vec<(i64, String)>);

#[cfg(test)]
fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

fn created_at_epoch(rfc3339: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(rfc3339)
        .ok()
        .map(|d| d.timestamp())
}

fn render_digest_body(event_type: &str, seqs: &[i64]) -> String {
    let (lo, hi) = (seqs.first().copied().unwrap_or(0), seqs.last().copied().unwrap_or(0));
    let range = if lo == hi { format!("seq {lo}") } else { format!("seq {lo}–{hi}") };
    format!("【teamx·digest】{event_type} × {}（{range}）", seqs.len())
}

fn render_direct_body(event_type: &str, seq: i64, payload: Option<&str>) -> String {
    let mut parts = vec![format!("【teamx】[{event_type}] seq {seq}")];
    if let Some(p) = payload.and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok()) {
        for k in ["title", "task", "id", "message", "detail"] {
            if let Some(s) = p.get(k).and_then(|v| v.as_str()) {
                if !s.is_empty() {
                    parts.push(s.chars().take(80).collect());
                    break;
                }
            }
        }
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use std::sync::Mutex;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        db::migrate(&conn).unwrap();
        conn
    }

    fn seed_team(conn: &Connection, team_id: &str) {
        conn.execute(
            "INSERT INTO teams (id, name, owner_member_id, goal_id, state, invite_token, created_at, updated_at)
             VALUES (?1, 'T', ?2, NULL, 'forming', 'tok', 'now', 'now')",
            params![team_id, "m1"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO members (id, team_id, session_key, display_name, role, state, joined_at)
             VALUES ('m1', ?1, 's', 'm', NULL, 'active', 'now')",
            params![team_id],
        )
        .unwrap();
    }

    fn emit(conn: &mut Connection, team: &str, etype: &str, payload: Option<&serde_json::Value>) -> i64 {
        let mut seq = 0i64;
        db::with_write(conn, |tx| {
            seq = events::emit(tx, team, Some("m1"), etype, payload)?;
            Ok(())
        })
        .unwrap();
        seq
    }

    fn notifier() -> Notifier {
        Notifier {
            groups: vec!["oc_test".into()],
            provider: Arc::new(OkProvider),
            digest_window_secs: DIGEST_WINDOW_SECS,
        }
    }

    struct OkProvider;
    struct FailProvider {
        err: &'static str,
        calls: Mutex<usize>,
    }

    impl ImProvider for OkProvider {
        fn platform(&self) -> &'static str { "stub" }
        fn send(&self, _msg: &ImMessage) -> Result<(), String> { Ok(()) }
        fn verify(&self, _ctx: &crate::im::types::ImDelegatedContext) -> Result<(), String> { Ok(()) }
        fn parse_event(&self, _raw: &str) -> Result<crate::im::types::ImEvent, String> { unimplemented!() }
        fn resolve_identity(&self, _ctx: &crate::im::types::ImDelegatedContext) -> Result<String, String> { unimplemented!() }
    }

    impl ImProvider for FailProvider {
        fn platform(&self) -> &'static str { "stub" }
        fn send(&self, _msg: &ImMessage) -> Result<(), String> {
            *self.calls.lock().unwrap() += 1;
            Err(self.err.into())
        }
        fn verify(&self, _ctx: &crate::im::types::ImDelegatedContext) -> Result<(), String> { Ok(()) }
        fn parse_event(&self, _raw: &str) -> Result<crate::im::types::ImEvent, String> { unimplemented!() }
        fn resolve_identity(&self, _ctx: &crate::im::types::ImDelegatedContext) -> Result<String, String> { unimplemented!() }
    }

    fn outbox_state(conn: &Connection, team: &str, seq: i64) -> String {
        conn.query_row(
            "SELECT state FROM im_outbox WHERE team_id=?1 AND seq=?2",
            params![team, seq],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn event_count(conn: &Connection, etype: &str) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM events WHERE type=?1", params![etype], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn collect_enqueues_whitelisted_skips_heartbeat_and_unknown() {
        let mut conn = test_conn();
        seed_team(&conn, "t1");
        emit(&mut conn, "t1", "progress.published", Some(&serde_json::json!({"kind":"session.idle"}))); // heartbeat: skip
        emit(&mut conn, "t1", "progress.published", Some(&serde_json::json!({"message":"进展"})));       // digest
        emit(&mut conn, "t1", "team.created", None);                                                     // unknown: skip
        emit(&mut conn, "t1", "doc.help_requested", None);                                               // direct
        let n = notifier();
        let stats = n.run_once(&mut conn, now_unix());
        assert_eq!(stats.collected, 2, "heartbeat + unknown type must be skipped");
        let n_rows: i64 = conn.query_row("SELECT COUNT(*) FROM im_outbox", [], |r| r.get(0)).unwrap();
        assert_eq!(n_rows, 2);
        // digest rows stay pending within the window; the direct row is
        // already pushed in the SAME pass (逐条必达 = immediate flush)
        assert_eq!(outbox_state(&conn, "t1", 2), "pending");
        assert_eq!(outbox_state(&conn, "t1", 4), "pushed");
        assert_eq!(event_count(&conn, "im.push"), 1, "exactly the direct row was pushed");
        assert_eq!(event_count(&conn, "im.push_failed"), 0);
    }

    #[test]
    fn cursor_persists_and_recollection_is_idempotent() {
        let mut conn = test_conn();
        seed_team(&conn, "t1");
        emit(&mut conn, "t1", "decision.broadcast", None);
        let n = notifier();
        n.run_once(&mut conn, now_unix());
        let rows: i64 = conn.query_row("SELECT COUNT(*) FROM im_outbox", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 1);
        // second pass: nothing new, PK dedup keeps one row
        let s2 = n.run_once(&mut conn, now_unix());
        assert_eq!(s2.collected, 0);
        let rows: i64 = conn.query_row("SELECT COUNT(*) FROM im_outbox", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 1, "im.push audit events must not be re-collected (closed enum)");
    }

    #[test]
    fn direct_rows_push_immediately_with_audit() {
        let mut conn = test_conn();
        seed_team(&conn, "t1");
        let seq = emit(&mut conn, "t1", "doc.help_requested", Some(&serde_json::json!({"title":"救火"})));
        let n = notifier();
        let stats = n.run_once(&mut conn, now_unix());
        assert_eq!(stats.pushed, 1);
        assert_eq!(outbox_state(&conn, "t1", seq), "pushed");
        assert_eq!(event_count(&conn, "im.push"), 1);
        // audit references the ledger seq
        let payload: String = conn
            .query_row("SELECT payload_json FROM events WHERE type='im.push'", [], |r| r.get(0))
            .unwrap();
        assert!(payload.contains(&format!("\"seqs\":[{seq}]")), "{payload}");
    }

    #[test]
    fn digest_defers_within_window_then_aggregates() {
        let mut conn = test_conn();
        seed_team(&conn, "t1");
        emit(&mut conn, "t1", "progress.published", None);
        emit(&mut conn, "t1", "progress.published", None);
        let n = notifier();
        let t0 = now_unix();
        let s1 = n.run_once(&mut conn, t0);
        assert_eq!(s1.deferred, 2, "within the 60s window rows are held, not sent");
        assert_eq!(event_count(&conn, "im.push"), 0);
        let s2 = n.run_once(&mut conn, t0 + n.digest_window_secs + 1);
        assert_eq!(s2.pushed, 2);
        assert_eq!(event_count(&conn, "im.push"), 1);
        let body = {
            let p: String = conn
                .query_row("SELECT payload_json FROM events WHERE type='im.push'", [], |r| r.get(0))
                .unwrap();
            let v: serde_json::Value = serde_json::from_str(&p).unwrap();
            v["seqs"].to_string()
        };
        assert_eq!(body, "[1,2]", "digest covers the aggregated seq range");
    }

    #[test]
    fn digest_flushes_early_on_batch_cap() {
        let mut conn = test_conn();
        seed_team(&conn, "t1");
        for _ in 0..DIGEST_BATCH_MAX {
            emit(&mut conn, "t1", "decision.broadcast", None);
        }
        let n = notifier();
        let stats = n.run_once(&mut conn, now_unix());
        assert_eq!(stats.pushed, DIGEST_BATCH_MAX, "a full digest group must flush without waiting");
    }

    #[test]
    fn failure_fails_permanently_after_cap_and_audits() {
        let mut conn = test_conn();
        seed_team(&conn, "t1");
        let seq = emit(&mut conn, "t1", "doc.help_requested", None);
        let n = Notifier {
            groups: vec!["oc_test".into()],
            provider: Arc::new(FailProvider { err: "boom", calls: Mutex::new(0) }),
            digest_window_secs: DIGEST_WINDOW_SECS,
        };
        for _ in 0..(MAX_ATTEMPTS - 1) {
            n.run_once(&mut conn, now_unix());
            assert_eq!(outbox_state(&conn, "t1", seq), "direct", "still retrying");
        }
        n.run_once(&mut conn, now_unix());
        assert_eq!(outbox_state(&conn, "t1", seq), "failed");
        assert_eq!(event_count(&conn, "im.push_failed"), 1);
        assert_eq!(event_count(&conn, "im.push"), 0);
    }

    #[test]
    fn rate_limit_backs_off_and_does_not_count_toward_failure_cap() {
        let mut conn = test_conn();
        seed_team(&conn, "t1");
        let seq = emit(&mut conn, "t1", "doc.help_requested", None);
        let provider = Arc::new(FailProvider { err: "feishu send: 429 rate limited", calls: Mutex::new(0) });
        let n = Notifier { groups: vec!["oc_test".into()], provider: provider.clone(), digest_window_secs: DIGEST_WINDOW_SECS };
        let t0 = now_unix();
        // First pass: send attempted, 429 → backoff set, row stays direct.
        let s = n.run_once(&mut conn, t0);
        assert_eq!(s.deferred, 1);
        assert_eq!(outbox_state(&conn, "t1", seq), "direct", "429 must never fail the row");
        assert_eq!(*provider.calls.lock().unwrap(), 1);
        // While backing off the flusher is gated: no sends, rows untouched.
        for _ in 0..3 {
            let s = n.run_once(&mut conn, t0 + 1);
            assert_eq!(s.pushed, 0, "backoff gate must short-circuit flush");
            assert_eq!(s.deferred, 0, "gated pass flushes nothing");
            assert_eq!(outbox_state(&conn, "t1", seq), "direct");
        }
        // backoff gate: the provider is NOT called while backing off
        let calls_before = *provider.calls.lock().unwrap();
        let _ = n.run_once(&mut conn, t0 + 1);
        assert_eq!(*provider.calls.lock().unwrap(), calls_before, "backoff must gate sends");
        // after the backoff expires, sends resume (still failing with 429 here)
        let _ = n.run_once(&mut conn, t0 + BACKOFF_BASE_SECS + 1);
        assert!(*provider.calls.lock().unwrap() > calls_before, "backoff expiry resumes sends");
    }

    #[test]
    fn no_target_groups_means_collect_only() {
        let mut conn = test_conn();
        seed_team(&conn, "t1");
        emit(&mut conn, "t1", "decision.broadcast", None);
        let n = Notifier { groups: vec![], provider: Arc::new(OkProvider), digest_window_secs: DIGEST_WINDOW_SECS };
        let stats = n.run_once(&mut conn, now_unix());
        assert_eq!(stats.collected, 1);
        assert_eq!(stats.pushed, 0);
        assert_eq!(outbox_state(&conn, "t1", 1), "pending");
        assert_eq!(event_count(&conn, "im.push"), 0);
    }

    #[test]
    fn whitelist_rendering_is_closed_and_digest_body_mentions_range() {
        assert!(is_digestable("decision.broadcast"));
        assert!(is_direct("task.nudge"));
        assert!(!is_digestable("im.push"));
        assert!(!is_direct("im.push"));
        assert!(!is_digestable("team.destroy"));
        assert!(is_heartbeat("progress.published", Some(&serde_json::json!({"kind":"session.idle"}))));
        assert!(!is_heartbeat("progress.published", Some(&serde_json::json!({"kind":"real"}))));
        let b = render_digest_body("progress.published", &[7, 8, 9]);
        assert!(b.contains("× 3") && b.contains("seq 7–9"), "{b}");
    }
}
