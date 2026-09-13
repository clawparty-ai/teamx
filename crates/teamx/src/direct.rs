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

use rusqlite::{Connection, params};
use serde_json::{Value, json};

use crate::db;

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
            let timeout_ms = args.get("timeout_ms").and_then(Value::as_u64).unwrap_or(800);
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
            Ok(json!({ "ok": true, "peer": peer, "reachable": reachable, "results": results }))
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
}
