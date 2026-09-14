//! member-to-member direct connectivity — P0: candidate registry + probe.
//!
//! The teamx server is the **control plane**: it records each member's direct
//! candidates (LAN IPs + the server-reflexive `peer_ip:port`) so same-team
//! members can attempt a **direct TCP path** before falling back to the tunnel
//! relay. Data then flows member↔member; control stays on the server.
//!
//! RPC (network mode, mTLS): `direct.advertise` / `direct.candidates` /
//! `direct.probe`. Candidates are only exposed to members of the same team.

use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use crate::db;

/// Probe rate-limit window / max probes per (from → to) pair.
const PROBE_WINDOW_SECS: i64 = 60;
const PROBE_MAX: i64 = 10;
/// Pairing-ticket TTL (seconds).
pub const TICKET_TTL_SECS: i64 = 60;

/// True when the (from → to) pair has issued too many probes recently.
fn rate_limited(conn: &Connection, team_id: &str, from: &str, to: &str) -> Result<bool, String> {
    let cutoff = (chrono::Utc::now() - chrono::Duration::seconds(PROBE_WINDOW_SECS)).to_rfc3339();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM direct_tickets
             WHERE team_id = ?1 AND from_member = ?2 AND to_member = ?3 AND created_at >= ?4",
            params![team_id, from, to, cutoff],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    Ok(n >= PROBE_MAX)
}

/// Issue a one-time pairing ticket bound to (from → to); the P1 handshake must
/// present it, so knowing the peer's address is not enough to connect.
fn issue_ticket(conn: &Connection, team_id: &str, from: &str, to: &str) -> Result<(String, String), String> {
    let ticket = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now();
    let created = now.to_rfc3339();
    let expires = (now + chrono::Duration::seconds(TICKET_TTL_SECS)).to_rfc3339();
    conn.execute(
        "INSERT INTO direct_tickets (ticket, team_id, from_member, to_member, created_at, expires_at)
         VALUES (?1,?2,?3,?4,?5,?6)",
        params![ticket, team_id, from, to, created, expires],
    )
    .map_err(|e| e.to_string())?;
    Ok((ticket, expires))
}

/// Verify + consume a pairing ticket (used by the P1 direct handshake).
/// Rejects unknown / mismatched-pair / expired / already-used tickets.
pub fn consume_ticket(conn: &mut Connection, ticket: &str, team_id: &str, from: &str, to: &str) -> Result<(), String> {
    let row: Option<(String, String, String, Option<String>, String)> = conn
        .query_row(
            "SELECT team_id, from_member, to_member, used_at, expires_at FROM direct_tickets WHERE ticket = ?1",
            [ticket],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some((t, f, to_m, used, expires)) = row else {
        return Err("unknown ticket".to_string());
    };
    if t != team_id || f != from || to_m != to {
        return Err("ticket does not match this member pair".to_string());
    }
    if used.is_some() {
        return Err("ticket already used".to_string());
    }
    let expired = chrono::DateTime::parse_from_rfc3339(&expires)
        .map(|e| e.with_timezone(&chrono::Utc) < chrono::Utc::now())
        .unwrap_or(true);
    if expired {
        return Err("ticket expired".to_string());
    }
    conn.execute(
        "UPDATE direct_tickets SET used_at = ?1 WHERE ticket = ?2",
        params![chrono::Utc::now().to_rfc3339(), ticket],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Load a member's advertised candidates (None when never advertised).
fn endpoint(conn: &Connection, team_id: &str, member_id: &str) -> Result<Option<Value>, String> {
    conn.query_row(
        "SELECT lan_ips, reflexive_ip, reflexive_port, listener_port, updated_at
         FROM direct_endpoints WHERE team_id = ?1 AND member_id = ?2",
        params![team_id, member_id],
        |r| {
            Ok(json!({
                "member_id": member_id,
                "lan_ips": r.get::<_, String>(0)?,
                "reflexive_ip": r.get::<_, Option<String>>(1)?,
                "reflexive_port": r.get::<_, Option<i64>>(2)?,
                "listener_port": r.get::<_, Option<i64>>(3)?,
                "updated_at": r.get::<_, String>(4)?,
            }))
        },
    )
    .map(Some)
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(other.to_string()),
    })
}

/// Candidate socket addresses for a peer, best-effort ordered (LAN first, then
/// the server-reflexive address). `lan_ips` is a JSON array string.
fn candidate_addrs(ep: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let port = ep.get("listener_port").and_then(|x| x.as_i64()).unwrap_or(0);
    let lan: Vec<String> = ep
        .get("lan_ips")
        .and_then(|x| x.as_str())
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    if port > 0 {
        for ip in lan {
            out.push(format!("{ip}:{port}"));
        }
    }
    if let (Some(ip), Some(rp)) = (
        ep.get("reflexive_ip").and_then(|x| x.as_str()),
        ep.get("reflexive_port").and_then(|x| x.as_i64()),
    ) {
        out.push(format!("{ip}:{rp}"));
    }
    out
}

/// Try a direct TCP connect to `addr` within `timeout`; returns the RTT in ms.
fn probe_addr(addr: &str, timeout: Duration) -> Result<u128, String> {
    let sock: SocketAddr = addr
        .to_socket_addrs()
        .map_err(|e| format!("resolve {addr}: {e}"))?
        .next()
        .ok_or_else(|| format!("no address for {addr}"))?;
    let t0 = Instant::now();
    TcpStream::connect_timeout(&sock, timeout)
        .map_err(|e| format!("{addr}: {e}"))?;
    Ok(t0.elapsed().as_millis())
}

/// Dispatch a `direct.*` RPC. `reflexive` is the peer address the server saw
/// for this connection (IP, port) — the STUN-like candidate.
pub fn rpc(
    method: &str,
    args: &Value,
    conn: &mut Connection,
    team_id: &str,
    member_id: &str,
    reflexive: Option<(String, u16)>,
) -> Result<Value, String> {
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
    match method {
        "direct.advertise" => {
            let lan_ips: Vec<String> = args
                .get("lan_ips")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let listener_port = args.get("listener_port").and_then(Value::as_i64);
            let (rip, rport) = match &reflexive {
                Some((ip, p)) => (Some(ip.clone()), Some(*p as i64)),
                None => (None, None),
            };
            let lan_json = serde_json::to_string(&lan_ips).unwrap_or_else(|_| "[]".into());
            let now = db::now();
            let payload = json!({
                "lan_ips": lan_ips,
                "listener_port": listener_port,
                "reflexive_ip": rip,
                "reflexive_port": rport,
            });
            let (mid, tid) = (member_id.to_string(), team_id.to_string());
            db::with_write(conn, |tx| {
                tx.execute(
                    "INSERT INTO direct_endpoints
                       (member_id, team_id, lan_ips, reflexive_ip, reflexive_port, listener_port, updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7)
                     ON CONFLICT(member_id, team_id) DO UPDATE SET
                       lan_ips=excluded.lan_ips, reflexive_ip=excluded.reflexive_ip,
                       reflexive_port=excluded.reflexive_port, listener_port=excluded.listener_port,
                       updated_at=excluded.updated_at",
                    params![mid, tid, lan_json, rip, rport, listener_port, now],
                )?;
                crate::events::emit(tx, &tid, Some(&mid), "direct.advertised", Some(&payload))?;
                Ok(())
            })
            .map_err(|e: rusqlite::Error| e.to_string())?;
            Ok(json!({ "ok": true, "member_id": member_id, "advertised": payload }))
        }
        "direct.candidates" => {
            let peer = s("member_id").ok_or_else(|| "direct.candidates requires `member_id`".to_string())?;
            if !crate::commands::member_in_team(conn, &peer, team_id).map_err(|e| e.to_string())? {
                return Err(format!("member {peer} is not in team {team_id}"));
            }
            match endpoint(conn, team_id, &peer)? {
                Some(ep) => Ok(json!({ "ok": true, "peer": ep, "candidates": candidate_addrs(&ep) })),
                None => Ok(json!({ "ok": true, "peer": Value::Null, "candidates": [], "note": "peer has not advertised" })),
            }
        }
        "direct.probe" => {
            let peer = s("member_id").ok_or_else(|| "direct.probe requires `member_id`".to_string())?;
            if !crate::commands::member_in_team(conn, &peer, team_id).map_err(|e| e.to_string())? {
                return Err(format!("member {peer} is not in team {team_id}"));
            }
            // Rate limit per (from → to) pair; rejections are audited.
            if rate_limited(conn, team_id, member_id, &peer)? {
                let payload = json!({
                    "reason_code": "rate_limited",
                    "from_member": member_id,
                    "to_member": peer,
                    "window_secs": PROBE_WINDOW_SECS,
                    "max": PROBE_MAX,
                });
                let (mid, tid) = (member_id.to_string(), team_id.to_string());
                db::with_write(conn, |tx| {
                    crate::events::emit(tx, &tid, Some(&mid), "direct.rejected", Some(&payload))?;
                    Ok(())
                })
                .map_err(|e: rusqlite::Error| e.to_string())?;
                return Err(format!(
                    "direct.probe rate limited for {peer} (max {PROBE_MAX}/{PROBE_WINDOW_SECS}s)"
                ));
            }
            let timeout_ms = args.get("timeout_ms").and_then(Value::as_u64).unwrap_or(1500);
            let timeout = Duration::from_millis(timeout_ms.max(50));
            let Some(ep) = endpoint(conn, team_id, &peer)? else {
                return Ok(json!({ "ok": true, "peer": peer, "reachable": false, "results": [], "note": "peer has not advertised" }));
            };
            let addrs = candidate_addrs(&ep);
            let mut results = Vec::new();
            let mut reachable = false;
            for a in &addrs {
                match probe_addr(a, timeout) {
                    Ok(rtt) => {
                        reachable = true;
                        results.push(json!({ "addr": a, "ok": true, "rtt_ms": rtt }));
                    }
                    Err(e) => results.push(json!({ "addr": a, "ok": false, "error": e })),
                }
            }
            let (ticket, expires) = issue_ticket(conn, team_id, member_id, &peer)?;
            Ok(json!({
                "ok": true,
                "peer": peer,
                "reachable": reachable,
                "results": results,
                "ticket": ticket,
                "ticket_expires_at": expires,
            }))
        }
        "direct.ticket.check" => {
            // Called by the PROVIDER (the `to` side of the pairing) during the
            // direct TLS handshake: wraps `consume_ticket` as an RPC so a
            // member process can validate + burn the ticket server-side. The
            // caller's identity comes from its mTLS cert (member_id), which
            // binds the ticket to the exact member pair without trusting the
            // request body. Failures are audited as `direct.rejected`.
            let ticket = s("ticket")
                .ok_or_else(|| "direct.ticket.check requires `ticket`".to_string())?;
            let caller = member_id.to_string();
            let from: Option<String> = conn
                .query_row(
                    "SELECT from_member FROM direct_tickets WHERE ticket = ?1",
                    [&ticket],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            let audit_reject = |conn: &mut Connection, reason: &str| -> Result<Value, String> {
                let (mid, tid) = (caller.clone(), team_id.to_string());
                let payload = json!({
                    "reason_code": reason,
                    "detail": "direct.ticket.check failed",
                    "to_member": caller,
                });
                db::with_write(conn, |tx| {
                    crate::events::emit(tx, &tid, Some(&mid), "direct.rejected", Some(&payload))
                })
                .map_err(|e: rusqlite::Error| e.to_string())?;
                Err(format!("direct ticket rejected: {reason}"))
            };
            let Some(from) = from else {
                return audit_reject(conn, "unknown ticket");
            };
            match consume_ticket(conn, &ticket, team_id, &from, &caller) {
                Ok(()) => Ok(json!({
                    "ok": true,
                    "requester_id": from,
                    "team_id": team_id,
                })),
                Err(e) => audit_reject(conn, &e),
            }
        }
        "direct.report" => {
            // Data-plane path telemetry from the tunnel consumer: records that a
            // direct path was established / failed / handed over to the relay.
            let name = s("name").unwrap_or_default();
            let addr = s("addr").unwrap_or_default();
            let event = s("event").unwrap_or_else(|| "unknown".to_string());
            let ev_type = match event.as_str() {
                "established" | "failed" | "handover" => format!("direct.{event}"),
                _ => return Err(format!("direct.report: unknown event `{event}`")),
            };
            let payload = json!({
                "name": name, "addr": addr, "path": "direct", "event": event,
                "member_id": member_id,
            });
            let (mid, tid) = (member_id.to_string(), team_id.to_string());
            db::with_write(conn, |tx| {
                crate::events::emit(tx, &tid, Some(&mid), &ev_type, Some(&payload))?;
                Ok(())
            })
            .map_err(|e: rusqlite::Error| e.to_string())?;
            Ok(json!({ "ok": true, "event": ev_type }))
        }
        other => Err(format!("unknown direct method `{other}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        db::migrate(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO teams (id,name,owner_member_id,state,invite_token,created_at,updated_at)
             VALUES ('t1','T','m-a','active','tok','now','now');
             INSERT INTO members (id,team_id,session_key,display_name,role,state,joined_at)
             VALUES ('m-a','t1','sa','A','owner','active','now'),
                    ('m-b','t1','sb','B','worker','active','now');",
        )
        .unwrap();
        conn
    }

    #[test]
    fn advertise_and_candidates_are_team_scoped() {
        let mut conn = test_conn();
        let out = rpc(
            "direct.advertise",
            &json!({"lan_ips":["192.168.1.5"], "listener_port": 9911}),
            &mut conn,
            "t1",
            "m-b",
            Some(("203.0.113.9".into(), 41000)),
        )
        .unwrap();
        assert_eq!(out["ok"], json!(true));

        let c = rpc("direct.candidates", &json!({"member_id":"m-b"}), &mut conn, "t1", "m-a", None).unwrap();
        assert_eq!(c["peer"]["reflexive_ip"], json!("203.0.113.9"));
        assert_eq!(c["candidates"][0], json!("192.168.1.5:9911"));
        // reflexive candidate is included as a fallback
        assert!(c["candidates"].as_array().unwrap().iter().any(|x| x == "203.0.113.9:41000"));
    }

    #[test]
    fn probe_dials_a_local_listener() {
        let mut conn = test_conn();
        // A real local listener stands in for the peer's direct listener.
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        rpc(
            "direct.advertise",
            &json!({"lan_ips":["127.0.0.1"], "listener_port": port}),
            &mut conn,
            "t1",
            "m-b",
            None,
        )
        .unwrap();
        let p = rpc("direct.probe", &json!({"member_id":"m-b", "timeout_ms": 500}), &mut conn, "t1", "m-a", None).unwrap();
        assert_eq!(p["reachable"], json!(true), "probe should reach the local listener: {p}");
        assert!(p["results"][0]["rtt_ms"].is_number());
    }

    #[test]
    fn probe_issues_one_time_ticket() {
        let mut conn = test_conn();
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        rpc("direct.advertise", &json!({"lan_ips":["127.0.0.1"], "listener_port": port}), &mut conn, "t1", "m-b", None).unwrap();
        let p = rpc("direct.probe", &json!({"member_id":"m-b"}), &mut conn, "t1", "m-a", None).unwrap();
        let t = p["ticket"].as_str().unwrap().to_string();
        // valid once for the bound pair (from m-a → to m-b)
        consume_ticket(&mut conn, &t, "t1", "m-a", "m-b").unwrap();
        // replay rejected
        assert!(consume_ticket(&mut conn, &t, "t1", "m-a", "m-b").is_err());
        // wrong pair rejected
        let p2 = rpc("direct.probe", &json!({"member_id":"m-b"}), &mut conn, "t1", "m-a", None).unwrap();
        let t2 = p2["ticket"].as_str().unwrap().to_string();
        assert!(consume_ticket(&mut conn, &t2, "t1", "m-b", "m-a").is_err());
    }

    #[test]
    fn probe_is_rate_limited_and_audited() {
        let mut conn = test_conn();
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        rpc("direct.advertise", &json!({"lan_ips":["127.0.0.1"], "listener_port": port}), &mut conn, "t1", "m-b", None).unwrap();
        for _ in 0..PROBE_MAX {
            rpc("direct.probe", &json!({"member_id":"m-b"}), &mut conn, "t1", "m-a", None).unwrap();
        }
        let err = rpc("direct.probe", &json!({"member_id":"m-b"}), &mut conn, "t1", "m-a", None).unwrap_err();
        assert!(err.contains("rate limited"), "{err}");
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM events WHERE team_id='t1' AND type='direct.rejected'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "rate-limit rejection must be audited");
    }

    #[test]
    fn report_records_path_telemetry() {
        let mut conn = test_conn();
        let out = rpc(
            "direct.report",
            &json!({"name":"web","event":"established","addr":"192.168.1.5:8080"}),
            &mut conn, "t1", "m-a", None,
        )
        .unwrap();
        assert_eq!(out["event"], json!("direct.established"));
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM events WHERE team_id='t1' AND type='direct.established'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        // unknown event is rejected
        assert!(rpc("direct.report", &json!({"event":"bogus"}), &mut conn, "t1", "m-a", None).is_err());
    }

    #[test]
    fn ticket_check_rpc_binds_caller_and_audits() {
        let mut conn = test_conn();
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        rpc("direct.advertise", &json!({"lan_ips":["127.0.0.1"], "listener_port": port}), &mut conn, "t1", "m-b", None).unwrap();
        let p = rpc("direct.probe", &json!({"member_id":"m-b"}), &mut conn, "t1", "m-a", None).unwrap();
        let t = p["ticket"].as_str().unwrap().to_string();

        // The provider (m-b, caller from mTLS) validates via the RPC: the
        // requester (from) is resolved from the ticket row, not the request.
        let ok = rpc(
            "direct.ticket.check",
            &json!({"ticket": t}),
            &mut conn,
            "t1",
            "m-b",
            None,
        )
        .unwrap();
        assert_eq!(ok["ok"], json!(true));
        assert_eq!(ok["requester_id"], json!("m-a"));

        // Replay via the RPC: rejected and audited.
        assert!(rpc(
            "direct.ticket.check",
            &json!({"ticket": t}),
            &mut conn,
            "t1",
            "m-b",
            None,
        )
        .is_err());

        // Unknown ticket: rejected and audited.
        assert!(rpc(
            "direct.ticket.check",
            &json!({"ticket": "no-such-ticket"}),
            &mut conn,
            "t1",
            "m-b",
            None,
        )
        .is_err());

        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM events WHERE team_id='t1' AND type='direct.rejected'", [], |r| r.get(0))
            .unwrap();
        assert!(n >= 2, "replay + unknown rejections must be audited, got {n}");
    }
}
