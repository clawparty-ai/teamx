//! analyze.rs — traffic analysis pipeline built on top of capture.db.
//!
//! Pure, network-free analysis: protocol/flow statistics, HTTP header
//! reconstruction, and (best-effort) TLS decryption using an imported
//! SSLKEYLOGFILE (the "capture-then-decrypt" model from 23-todo-flow-analysis).
//!
//! Decryption scope (v1): TLS 1.3 traffic secrets (CLIENT/SERVER_TRAFFIC_
//! SECRET_0) exported by the *client*. TLS 1.2 RSA/CLIENT_RANDOM master
//! secrets are detected but deferred to a later revision.

use crate::capture::CaptureStore;

/// Aggregate statistics over all captured flows.
pub fn stats(store: &mut CaptureStore) -> Result<serde_json::Value, String> {
    let c = store.conn();
    let total: i64 = c
        .query_row("SELECT COUNT(*) FROM flows", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let bytes: (i64, i64) = c
        .query_row(
            "SELECT COALESCE(SUM(bytes_up),0), COALESCE(SUM(bytes_down),0) FROM flows",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| e.to_string())?;
    // protocol histogram
    let mut protos: Vec<(String, i64)> = Vec::new();
    {
        let mut stmt = c
            .prepare("SELECT proto, COUNT(*) FROM flows GROUP BY proto ORDER BY 2 DESC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows {
            if let Ok(r) = row {
                protos.push(r);
            }
        }
    }
    // top SNIs
    let mut top_sni: Vec<(String, i64)> = Vec::new();
    {
        let mut stmt = c
            .prepare(
                "SELECT sni, SUM(bytes_up+bytes_down) FROM flows WHERE sni IS NOT NULL \
                 GROUP BY sni ORDER BY 2 DESC LIMIT 10",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows {
            if let Ok(r) = row {
                top_sni.push(r);
            }
        }
    }
    let decrypted: i64 = c
        .query_row("SELECT COUNT(*) FROM flows WHERE decrypted=1", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "total_flows": total,
        "bytes_up": bytes.0,
        "bytes_down": bytes.1,
        "decrypted_flows": decrypted,
        "protocols": protos,
        "top_sni": top_sni,
    }))
}

/// List flows with key metadata.
pub fn list_flows(
    store: &mut CaptureStore,
    sni: Option<&str>,
    limit: usize,
) -> Result<Vec<serde_json::Value>, String> {
    let c = store.conn();
    let sql = if sni.is_some() {
        format!(
            "SELECT id, started_at, source, proto, remote_addr, sni, \
                    bytes_up, bytes_down, decrypted FROM flows \
             WHERE sni LIKE ?1 ORDER BY started_at DESC LIMIT {limit}"
        )
    } else {
        format!(
            "SELECT id, started_at, source, proto, remote_addr, sni, \
                    bytes_up, bytes_down, decrypted FROM flows \
             ORDER BY started_at DESC LIMIT {limit}"
        )
    };
    let mut stmt = c.prepare(&sql).map_err(|e| e.to_string())?;
    let rows: Vec<serde_json::Value> = match sni {
        Some(s) => {
            let pat = s.replace('*', "%");
            stmt.query_map([pat], |r| Ok(row_to_json(r)))
                .map_err(|e| e.to_string())?
                .filter_map(|r| r.ok())
                .collect()
        }
        None => stmt
            .query_map([], |r| Ok(row_to_json(r)))
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect(),
    };
    Ok(rows)
}

fn row_to_json(r: &rusqlite::Row) -> serde_json::Value {
    serde_json::json!({
        "id": r.get::<_, i64>(0).ok(),
        "started_at": r.get::<_, f64>(1).ok(),
        "source": r.get::<_, String>(2).ok(),
        "proto": r.get::<_, String>(3).ok(),
        "remote": r.get::<_, String>(4).ok(),
        "sni": r.get::<_, Option<String>>(5).ok().flatten(),
        "bytes_up": r.get::<_, i64>(6).ok(),
        "bytes_down": r.get::<_, i64>(7).ok(),
        "decrypted": r.get::<_, i64>(8).ok(),
    })
}

/// Reconstruct HTTP request/response headers from a flow's stream chunks.
/// Returns per-direction header blobs (best-effort; stops at first body byte).
pub fn http_reconstruct(store: &mut CaptureStore, flow_id: i64) -> Result<serde_json::Value, String> {
    let c = store.conn();
    let mut stmt = c
        .prepare(
            "SELECT dir, data FROM stream_chunks WHERE flow_id=?1 ORDER BY seq ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows: Vec<(String, Vec<u8>)> = stmt
        .query_map([flow_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    let mut up = Vec::new();
    let mut down = Vec::new();
    for (dir, data) in rows {
        if dir == "up" {
            up.extend_from_slice(&data);
        } else {
            down.extend_from_slice(&data);
        }
    }
    Ok(serde_json::json!({
        "request_head": head_only(&up),
        "response_head": head_only(&down),
    }))
}

/// Return just the HTTP header section (up to the blank line).
/// Uses raw byte scanning to avoid lossy UTF-8 conversion on binary data.
fn head_only(buf: &[u8]) -> String {
    // Find \r\n\r\n or \n\n in raw bytes.
    if let Some(pos) = find_subsequence(buf, b"\r\n\r\n") {
        String::from_utf8_lossy(&buf[..pos + 4]).to_string()
    } else if let Some(pos) = find_subsequence(buf, b"\n\n") {
        String::from_utf8_lossy(&buf[..pos + 2]).to_string()
    } else {
        String::from_utf8_lossy(buf).to_string()
    }
}

/// Find the first occurrence of `needle` in `haystack`.
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

// ---------------------------------------------------------------------------
// TLS decryption (v1: TLS 1.3 traffic secrets)
// ---------------------------------------------------------------------------

/// Load client/server traffic secrets exported for a given client_random.
/// Returns (client_secret, server_secret) as raw bytes, or None if not found.
pub fn load_traffic_secrets(
    store: &mut CaptureStore,
    client_random: &[u8],
) -> Option<(Vec<u8>, Vec<u8>)> {
    // Query directly by client_random (indexed via PRIMARY KEY).
    let mut rows: Vec<(String, Vec<u8>)> = Vec::new();
    {
        let mut stmt = store
            .conn()
            .prepare("SELECT label, secret FROM tls_keys WHERE client_random = ?1")
            .ok()?;
        let iter = stmt
            .query_map([client_random], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                ))
            })
            .ok()?;
        for row in iter {
            if let Ok(r) = row {
                rows.push(r);
            }
        }
    }
    let mut client = None;
    let mut server = None;
    for (label, secret) in rows {
        match label.as_str() {
            "CLIENT_TRAFFIC_SECRET_0" => client = Some(secret),
            "SERVER_TRAFFIC_SECRET_0" => server = Some(secret),
            _ => {}
        }
    }
    match (client, server) {
        (Some(c), Some(s)) => Some((c, s)),
        _ => None,
    }
}

/// Exact-length HKDF output length key (ring 0.17 requires `fill(out)` with
/// `out.len() == KeyType::len()`; `Algorithm` always yields 32 bytes).
struct OkmLen(usize);
impl ring::hkdf::KeyType for OkmLen {
    fn len(&self) -> usize {
        self.0
    }
}

/// Expand a TLS 1.3 traffic secret into (key, iv) via HKDF-Expand-Label.
/// Uses SHA-256; AES-256-GCM keys are 32 bytes, IVs 12 bytes.
///
/// The traffic secret is used *directly* as the HKDF PRK (RFC 8446 §7.3), so
/// we call `Prk::new_less_safe` rather than `Salt::extract`.
pub fn tls13_expand(secret: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
    use ring::hkdf::{Prk, HKDF_SHA256};
    let prk = Prk::new_less_safe(HKDF_SHA256, secret);
    let key_info = label("tls13 key", 32);
    let iv_info = label("tls13 iv", 12);
    let mut key = vec![0u8; 32];
    prk.expand(&[key_info.as_ref()], OkmLen(32))
        .map_err(|e| format!("hkdf key: {e:?}"))?
        .fill(&mut key)
        .map_err(|e| format!("hkdf key fill: {e:?}"))?;
    let mut iv = vec![0u8; 12];
    prk.expand(&[iv_info.as_ref()], OkmLen(12))
        .map_err(|e| format!("hkdf iv: {e:?}"))?
        .fill(&mut iv)
        .map_err(|e| format!("hkdf iv fill: {e:?}"))?;
    Ok((key, iv))
}

/// HKDF-Expand-Label as defined in RFC 8446 section 7.1.
fn label(purpose: &str, len: usize) -> Vec<u8> {
    // struct { uint16 length; opaque label<7..255>; opaque context<0..255>; }
    let full = format!("tls13 {purpose}");
    let lab = full.as_bytes();
    let mut out = Vec::with_capacity(2 + 1 + lab.len() + 1);
    out.extend_from_slice(&(len as u16).to_be_bytes());
    out.push(lab.len() as u8);
    out.extend_from_slice(lab);
    out.push(0); // zero-length context
    out
}

/// Decrypt a single TLS 1.3 application_data record that has already had its
/// 5-byte record header stripped. `seq` is the 64-bit big-endian record
/// sequence number; `payload_tag` is ciphertext + 16-byte GCM tag.
pub fn decrypt_tls13_record(
    key: &[u8],
    iv: &[u8],
    seq: u64,
    payload_tag: &[u8],
) -> Result<Vec<u8>, String> {
    use aes_gcm::aead::{Aead, Payload};
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
    if payload_tag.len() < 16 {
        return Err("record too short for GCM tag".into());
    }
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| format!("key len: {e:?}"))?;
    // nonce = iv XOR seq (seq placed in the last 8 bytes of the 12-byte nonce)
    let mut nonce12 = [0u8; 12];
    nonce12.copy_from_slice(iv);
    let seq_bytes = seq.to_be_bytes();
    for i in 0..8 {
        nonce12[4 + i] ^= seq_bytes[i];
    }
    let nonce = Nonce::from_slice(&nonce12);
    // AAD = sequence(8) + type(1)=23 + version(2)=0x0303 + length(2)
    let len = (payload_tag.len() - 16) as u16;
    let mut aad = Vec::with_capacity(13);
    aad.extend_from_slice(&seq_bytes);
    aad.push(23); // application_data
    aad.extend_from_slice(&[0x03, 0x03]);
    aad.extend_from_slice(&len.to_be_bytes());
    let plaintext = cipher
        .decrypt(nonce, Payload { msg: payload_tag, aad: &aad })
        .map_err(|_| "gcm decrypt failed (bad key/seq/aad)".to_string())?;
    Ok(plaintext)
}

#[allow(dead_code)]
fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[allow(dead_code)]
fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16)).collect::<Result<Vec<_>, _>>().ok()
}

/// Decrypt a whole flow's TLS application_data using imported traffic secrets.
/// Returns the number of decrypted bytes; marks `flows.decrypted=1` on success.
///
/// v1 scope: TLS 1.3 traffic secrets; sequence numbers are counted over
/// application_data records only (encrypted handshake records between
/// ServerHello and the first app record are not separately tracked — a known
/// limitation noted in 23-todo-flow-analysis).
pub fn decrypt_flow(store: &mut CaptureStore, flow_id: i64) -> Result<usize, String> {
    // 1. fetch client_random from the flow
    let cr: Option<Vec<u8>> = store
        .conn()
        .query_row(
            "SELECT client_random FROM flows WHERE id=?1",
            [flow_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let cr = match cr {
        Some(c) => c,
        None => return Err("flow has no client_random".into()),
    };
    // 2. load secrets
    let (client_sec, server_sec) = match load_traffic_secrets(store, &cr) {
        Some(s) => s,
        None => return Err("no matching traffic secret in keylog".into()),
    };
    // 3. expand keys
    let (ckey, civ) = tls13_expand(&client_sec)?;
    let (skey, siv) = tls13_expand(&server_sec)?;

    // 4. gather + reassemble direction streams
    let (up, down) = {
        let mut stmt = store
            .conn()
            .prepare("SELECT dir, data FROM stream_chunks WHERE flow_id=?1 ORDER BY seq ASC")
            .map_err(|e| e.to_string())?;
        let rows: Vec<(String, Vec<u8>)> = stmt
            .query_map([flow_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();
        let mut up = Vec::new();
        let mut down = Vec::new();
        for (dir, data) in &rows {
            if dir == "up" {
                up.extend_from_slice(data);
            } else {
                down.extend_from_slice(data);
            }
        }
        (up, down)
    };

    // 5. decrypt each direction's application_data records
    let mut total = 0usize;
    let (up_recs, _) = crate::tls_parse::split_records(&up, 0);
    let (down_recs, _) = crate::tls_parse::split_records(&down, 0);
    for (i, (ct, payload)) in up_recs.iter().enumerate() {
        if *ct != 23 {
            continue;
        }
        if let Ok(pt) = decrypt_tls13_record(&ckey, &civ, i as u64, payload) {
            total += pt.len();
        }
    }
    for (i, (ct, payload)) in down_recs.iter().enumerate() {
        if *ct != 23 {
            continue;
        }
        if let Ok(pt) = decrypt_tls13_record(&skey, &siv, i as u64, payload) {
            total += pt.len();
        }
    }

    if total > 0 {
        store
            .conn()
            .execute(
                "UPDATE flows SET decrypted=1 WHERE id=?1",
                [flow_id],
            )
            .map_err(|e| e.to_string())?;
    }
    Ok(total)
}

/// CLI entry point for `teamx analyze`.
pub fn handle(cmd: &crate::cli::AnalyzeCmd) -> Result<serde_json::Value, String> {
    use crate::cli::AnalyzeCmd;
    let db = crate::capture_cli::capture_home();
    let mut store = CaptureStore::open(&db)?;
    match cmd {
        AnalyzeCmd::Stats => stats(&mut store),
        AnalyzeCmd::List { sni, limit } => {
            let rows = list_flows(&mut store, sni.as_deref(), *limit)?;
            Ok(serde_json::json!({ "ok": true, "flows": rows }))
        }
        AnalyzeCmd::Http { flow } => {
            let r = http_reconstruct(&mut store, *flow)?;
            Ok(serde_json::json!({ "ok": true, "flow": flow, "http": r }))
        }
        AnalyzeCmd::Decrypt { flow } => {
            let n = decrypt_flow(&mut store, *flow)?;
            Ok(serde_json::json!({ "ok": true, "flow": flow, "decrypted_bytes": n }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{Dir, TapEvent};
    use crate::tls_parse::split_records;
    use aes_gcm::aead::{Aead, Payload};
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
    use std::path::PathBuf;

    fn tmp_db(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("teamx-analyze-test-{}-{}.db", std::process::id(), name));
        let _ = std::fs::remove_file(&p);
        p
    }

    // Build a synthetic TLS 1.3 application_data record the way a client would:
    // encrypt `plaintext` with the derived client/server key at `seq`.
    fn make_record(key: &[u8], iv: &[u8], seq: u64, plaintext: &[u8]) -> Vec<u8> {
        let cipher = Aes256Gcm::new_from_slice(key).unwrap();
        let mut nonce12 = [0u8; 12];
        nonce12.copy_from_slice(iv);
        let sb = seq.to_be_bytes();
        for i in 0..8 {
            nonce12[4 + i] ^= sb[i];
        }
        let len = plaintext.len() as u16;
        let mut aad = Vec::new();
        aad.extend_from_slice(&sb);
        aad.push(23);
        aad.extend_from_slice(&[0x03, 0x03]);
        aad.extend_from_slice(&len.to_be_bytes());
        let ct = cipher
            .encrypt(Nonce::from_slice(&nonce12), Payload { msg: plaintext, aad: &aad })
            .unwrap();
        // TLS record header: type=23, ver=0303, len
        let mut rec = vec![23, 0x03, 0x03];
        rec.extend_from_slice(&(ct.len() as u16).to_be_bytes());
        rec.extend_from_slice(&ct);
        rec
    }

    #[test]
    fn tls13_decrypt_roundtrip() {
        // random-ish 32-byte secrets
        let client_sec = vec![0x11u8; 32];
        let server_sec = vec![0x22u8; 32];
        let (ckey, civ) = tls13_expand(&client_sec).unwrap();
        let (skey, siv) = tls13_expand(&server_sec).unwrap();
        // up record (client->server, seq 0)
        let up_rec = make_record(&ckey, &civ, 0, b"GET /secret HTTP/1.1\r\nHost: x\r\n\r\n");
        // down record (server->client, seq 0)
        let down_rec = make_record(&skey, &siv, 0, b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi");

        let db = tmp_db("dec");
        let mut store = CaptureStore::open(&db).unwrap();
        // client_random (any 32 bytes)
        let cr = vec![0xabu8; 32];
        store
            .apply_batch(vec![
                TapEvent::FlowOpen {
                    flow_id: 1,
                    source: "socks5",
                    local: "10.0.0.2".into(),
                    remote_addr: "93.184.216.34:443".into(),
                    ts: 1.0,
                    first_bytes: vec![], // we won't parse ClientHello here
                },
                TapEvent::Chunk { flow_id: 1, dir: Dir::Up, seq: 1, ts: 1.1, data: up_rec.clone() },
                TapEvent::Chunk { flow_id: 1, dir: Dir::Down, seq: 1, ts: 1.2, data: down_rec.clone() },
            ])
            .unwrap();
        // store the traffic secrets keyed by client_random
        store
            .import_keylog(&format!(
                "CLIENT_TRAFFIC_SECRET_0 {} {}\nSERVER_TRAFFIC_SECRET_0 {} {}\n",
                hex(&cr), hex(&client_sec), hex(&cr), hex(&server_sec)
            ))
            .unwrap();
        drop(store);
        // also set flows.client_random (apply_batch left it NULL) — simulate by
        // writing directly through a fresh store handle.
        let rc = rusqlite::Connection::open(&db).unwrap();
        rc.execute("UPDATE flows SET client_random=?1 WHERE id=1", [cr.clone()]).unwrap();
        drop(rc);

        let mut reader = CaptureStore::open(&db).unwrap();
        let n = decrypt_flow(&mut reader, 1).unwrap();
        assert!(n > 0, "decrypted {n} bytes");

        // verify the reconstructed plaintext matches by re-deriving
        let (up_recs, _) = split_records(&up_rec, 0);
        let (pt,) = (decrypt_tls13_record(&ckey, &civ, 0, &up_recs[0].1).unwrap(),);
        assert_eq!(&pt, b"GET /secret HTTP/1.1\r\nHost: x\r\n\r\n");
        let (down_recs, _) = split_records(&down_rec, 0);
        let pt2 = decrypt_tls13_record(&skey, &siv, 0, &down_recs[0].1).unwrap();
        assert_eq!(&pt2, b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi");
    }

    #[test]
    fn stats_and_list() {
        let db = tmp_db("stats");
        let mut store = CaptureStore::open(&db).unwrap();
        store
            .apply_batch(vec![
                TapEvent::FlowOpen {
                    flow_id: 1,
                    source: "socks5",
                    local: "10.0.0.2".into(),
                    remote_addr: "1.1.1.1:443".into(),
                    ts: 1.0,
                    first_bytes: vec![22, 3, 1, 0, 0],
                },
                TapEvent::Chunk { flow_id: 1, dir: Dir::Up, seq: 1, ts: 1.1, data: b"ping".to_vec() },
            ])
            .unwrap();
        let s = stats(&mut store).unwrap();
        assert_eq!(s["total_flows"], 1);
        let rows = list_flows(&mut store, None, 10).unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn http_reconstruct_head() {
        let db = tmp_db("http");
        let mut store = CaptureStore::open(&db).unwrap();
        store
            .apply_batch(vec![
                TapEvent::FlowOpen {
                    flow_id: 1,
                    source: "socks5",
                    local: "10.0.0.2".into(),
                    remote_addr: "1.1.1.1:80".into(),
                    ts: 1.0,
                    first_bytes: b"GET / HTTP/1.1\r\n".to_vec(),
                },
                TapEvent::Chunk {
                    flow_id: 1,
                    dir: Dir::Up,
                    seq: 1,
                    ts: 1.1,
                    data: b"GET /index.html HTTP/1.1\r\nHost: example.com\r\n\r\nbody".to_vec(),
                },
                TapEvent::Chunk {
                    flow_id: 1,
                    dir: Dir::Down,
                    seq: 1,
                    ts: 1.2,
                    data: b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi".to_vec(),
                },
            ])
            .unwrap();
        let r = http_reconstruct(&mut store, 1).unwrap();
        let req: String = r["request_head"].as_str().unwrap().to_string();
        assert!(req.contains("GET /index.html"));
        let resp: String = r["response_head"].as_str().unwrap().to_string();
        assert!(resp.contains("200 OK"));
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }
}
