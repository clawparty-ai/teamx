//! replay.rs — traffic replay built on capture.db (ENT-06-traffic-replay).
//!
//! Two TCP modes + an HTTP-exchange layer:
//!   * `serve`   (mode B): mock server that replays a flow's recorded
//!     down-stream to any client connecting to a local port.
//!   * `tcp_replay` (mode A): client replay — reconnect to a target, send the
//!     recorded up-stream, and capture/compare the response.
//!   * HTTP layer: extract plain-HTTP exchanges from flows, export/import HAR,
//!     emit `curl` commands, and re-send a single request.

use crate::capture::CaptureStore;

/// Reassemble a flow's up/down byte streams in sequence order.
pub fn flow_streams(store: &mut CaptureStore, flow_id: i64) -> Result<(Vec<u8>, Vec<u8>), String> {
    let mut stmt = store
        .conn()
        .prepare("SELECT dir, data FROM stream_chunks WHERE flow_id=?1 ORDER BY seq ASC")
        .map_err(|e| e.to_string())?;
    let rows: Vec<(String, Vec<u8>)> = stmt
        .query_map([flow_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)))
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
    Ok((up, down))
}

/// Fetch a flow's recorded remote host:port (target for client replay).
pub fn flow_target(store: &mut CaptureStore, flow_id: i64) -> Result<String, String> {
    let t: Option<String> = store
        .conn()
        .query_row(
            "SELECT remote_addr FROM flows WHERE id=?1",
            [flow_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    Ok(t.unwrap_or_default())
}

// ---------------------------------------------------------------------------
// Mode B — mock server replay
// ---------------------------------------------------------------------------

/// Serve a flow's recorded down-stream on a local TCP port. Blocks forever
/// (spawns a task per connection). Clients get the down-stream bytes
/// unconditionally (v1 single-flow mock; `--match` refinements later).
pub async fn serve_flow(store: &mut CaptureStore, flow_id: i64, port: u16) -> Result<(), String> {
    let (_up, down) = flow_streams(store, flow_id)?;
    if down.is_empty() {
        return Err("flow has no down-stream bytes to serve".into());
    }
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|e| format!("bind 127.0.0.1:{port}: {e}"))?;
    println!("ok replay: mock server on 127.0.0.1:{port} (flow {flow_id}, {}B down)", down.len());
    loop {
        let (mut sock, _peer) = listener
            .accept()
            .await
            .map_err(|e| format!("accept: {e}"))?;
        let down = down.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            // Read everything the client sends (may be partial / incomplete),
            // then serve the recorded down-stream. A v1 mock — no matching.
            let mut client_data = Vec::new();
            let _ = tokio::time::timeout(
                std::time::Duration::from_millis(500),
                sock.read_to_end(&mut client_data),
            ).await;
            let _ = sock.write_all(&down).await;
            let _ = sock.shutdown().await;
        });
    }
}

// ---------------------------------------------------------------------------
// Mode A — client replay
// ---------------------------------------------------------------------------

/// Replay a flow's up-stream to a target (default: the recorded remote),
/// tunnelled through a teamx proxy exit so censored targets stay reachable.
/// Returns a summary JSON object (bytes sent/received, diff ratio vs recorded
/// down-stream).
pub async fn tcp_replay(
    store: &mut CaptureStore,
    flow_id: i64,
    to: Option<&str>,
    server_url: &str,
    exit: &str,
) -> Result<serde_json::Value, String> {
    let (up, down) = flow_streams(store, flow_id)?;
    let target = match to {
        Some(t) => t.to_string(),
        None => flow_target(store, flow_id)?,
    };
    if up.is_empty() {
        return Err("flow has no up-stream bytes to replay".into());
    }
    let start = std::time::Instant::now();
    let mut bridge = crate::tunnel_client::open_tunnel_bridge(server_url, exit, &target)
        .await
        .map_err(|e| format!("bridge to {exit}/{target}: {e}"))?;
    bridge
        .tx
        .send(up.clone())
        .map_err(|_| "bridge send closed".to_string())?;
    // read the response until EOF or idle
    let mut resp = Vec::new();
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(3), bridge.rx.recv()).await {
            Ok(Some(bytes)) => resp.extend_from_slice(&bytes),
            Ok(None) => break,
            Err(_) => break, // idle timeout
        }
    }
    let _ = bridge.close.send(());
    let elapsed_ms = start.elapsed().as_millis() as i64;
    let diff = prefix_match_ratio(&down, &resp);
    Ok(serde_json::json!({
        "status": if diff == 1.0 { "ok" } else { "diff" },
        "target": target,
        "exit": exit,
        "bytes_sent": up.len(),
        "bytes_received": resp.len(),
        "match_ratio": diff,
        "duration_ms": elapsed_ms,
    }))
}

/// Ratio of the recorded down-stream that is a byte-for-byte prefix of the
/// replayed response (simple v1 diff; LCS deferred).
fn prefix_match_ratio(recorded: &[u8], replayed: &[u8]) -> f64 {
    if recorded.is_empty() {
        return 1.0;
    }
    let n = recorded.len().min(replayed.len());
    if n == 0 {
        return 0.0;
    }
    let mut matched = 0usize;
    for i in 0..n {
        if recorded[i] == replayed[i] {
            matched += 1;
        } else {
            break;
        }
    }
    matched as f64 / recorded.len() as f64
}

// ---------------------------------------------------------------------------
// HTTP exchange layer
// ---------------------------------------------------------------------------

/// A parsed HTTP request/response pair.
#[derive(Debug, Clone, Default)]
pub struct HttpExchange {
    #[allow(dead_code)] // DB row id; used by tests and future diff views
    pub id: i64,
    pub flow_id: i64,
    pub method: String,
    pub url: String,
    pub host: String,
    pub request_headers: String,
    pub request_body: Vec<u8>,
    pub status: i64,
    pub response_headers: String,
    pub response_body: Vec<u8>,
}

/// Parse a plain HTTP request head (method + target + headers) from bytes.
/// Returns (method, url, host, headers, body_offset).
fn parse_request(buf: &[u8]) -> Option<(String, String, String, String, usize)> {
    let text = String::from_utf8_lossy(buf);
    let head_end = text.find("\r\n\r\n").or_else(|| text.find("\n\n"))?;
    let head = &text[..head_end];
    let mut lines = head.split("\r\n");
    let req_line = lines.next()?;
    let mut parts = req_line.split_whitespace();
    let method = parts.next()?.to_string();
    let url = parts.next()?.to_string();
    let mut host = String::new();
    let mut headers = String::new();
    for line in lines {
        headers.push_str(line);
        headers.push_str("\r\n");
        if let Some(rest) = line.strip_prefix("Host:") {
            host = rest.trim().to_string();
        }
    }
    // Compute body offset dynamically based on the separator used.
    let sep_len = if text[head_end..].starts_with("\r\n\r\n") { 4 } else { 2 };
    Some((method, url, host, headers, head_end + sep_len))
}

/// Parse a plain HTTP response head (status + headers) from bytes.
/// Returns (status, headers, body_offset).
fn parse_response(buf: &[u8]) -> Option<(i64, String, usize)> {
    let text = String::from_utf8_lossy(buf);
    let head_end = text.find("\r\n\r\n").or_else(|| text.find("\n\n"))?;
    let head = &text[..head_end];
    let mut lines = head.split("\r\n");
    let status_line = lines.next()?;
    // "HTTP/1.1 200 OK"
    let mut parts = status_line.split_whitespace();
    let _http = parts.next()?;
    let status: i64 = parts.next()?.parse().ok()?;
    let mut headers = String::new();
    for line in lines {
        headers.push_str(line);
        headers.push_str("\r\n");
    }
    // Compute body offset dynamically based on the separator used.
    let sep_len = if text[head_end..].starts_with("\r\n\r\n") { 4 } else { 2 };
    Some((status, headers, head_end + sep_len))
}

/// Extract a plain-HTTP exchange from a flow's streams (best-effort). Only
/// works for non-TLS HTTP; decrypted-TLS exchanges are produced by the
/// analyze decrypt pipeline instead.
pub fn extract_exchange(store: &mut CaptureStore, flow_id: i64) -> Result<HttpExchange, String> {
    let (up, down) = flow_streams(store, flow_id)?;
    let (method, mut url, host, req_headers, req_body_off) =
        parse_request(&up).ok_or("up-stream is not a parseable HTTP request")?;
    let (status, resp_headers, resp_body_off) =
        parse_response(&down).ok_or("down-stream is not a parseable HTTP response")?;
    // SOCKS5 clients send origin-form (relative); make it absolute for HAR/curl.
    if !url.starts_with("http://") && !url.starts_with("https://") {
        url = format!("http://{host}{url}");
    }
    Ok(HttpExchange {
        id: 0,
        flow_id,
        method,
        url,
        host,
        request_headers: req_headers,
        request_body: up[req_body_off..].to_vec(),
        status,
        response_headers: resp_headers,
        response_body: down[resp_body_off..].to_vec(),
    })
}

/// Persist an exchange into `http_exchanges` and return its row id.
pub fn store_exchange(store: &mut CaptureStore, e: &HttpExchange) -> Result<i64, String> {
    store
        .conn()
        .execute(
            "INSERT INTO http_exchanges \
             (flow_id, method, url, host, request_headers, request_body, status, \
              response_headers, response_body, started_at) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            rusqlite::params![
                e.flow_id,
                e.method,
                e.url,
                e.host,
                e.request_headers,
                e.request_body,
                e.status,
                e.response_headers,
                e.response_body,
                crate::capture::now()
            ],
        )
        .map_err(|e| e.to_string())?;
    let id: i64 = store
        .conn()
        .query_row("SELECT last_insert_rowid()", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    Ok(id)
}

pub fn load_exchange(store: &mut CaptureStore, id: i64) -> Result<HttpExchange, String> {
    let e = store
        .conn()
        .query_row(
            "SELECT id, COALESCE(flow_id,0), COALESCE(method,''), COALESCE(url,''), \
                    COALESCE(host,''), COALESCE(request_headers,''), \
                    COALESCE(request_body, x''), COALESCE(status,0), \
                    COALESCE(response_headers,''), COALESCE(response_body, x'') \
             FROM http_exchanges WHERE id=?1",
            [id],
            |r| {
                Ok(HttpExchange {
                    id: r.get(0)?,
                    flow_id: r.get(1)?,
                    method: r.get(2)?,
                    url: r.get(3)?,
                    host: r.get(4)?,
                    request_headers: r.get(5)?,
                    request_body: r.get(6)?,
                    status: r.get(7)?,
                    response_headers: r.get(8)?,
                    response_body: r.get(9)?,
                })
            },
        )
        .map_err(|e| e.to_string())?;
    Ok(e)
}

/// Build a HAR 1.2 document from all exchanges in the DB.
pub fn export_har(store: &mut CaptureStore) -> Result<serde_json::Value, String> {
    let mut stmt = store
        .conn()
        .prepare("SELECT id, flow_id, method, url, host, request_headers, status, response_headers, started_at FROM http_exchanges ORDER BY id")
        .map_err(|e| e.to_string())?;
    let rows: Vec<(i64, i64, String, String, String, String, i64, String, f64)> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?,
                r.get(6)?, r.get(7)?, r.get(8)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    let mut entries = Vec::new();
    for (id, _flow, method, url, _host, req_headers, status, resp_headers, started) in rows {
        entries.push(serde_json::json!({
            "startedDateTime": timestamp_to_iso(started),
            "request": {
                "method": method,
                "url": url,
                "httpVersion": "HTTP/1.1",
                "headers": parse_header_list(&req_headers),
                "headersSize": -1,
                "bodySize": -1,
            },
            "response": {
                "status": status,
                "statusText": "",
                "httpVersion": "HTTP/1.1",
                "headers": parse_header_list(&resp_headers),
                "headersSize": -1,
                "bodySize": -1,
                "content": { "size": 0, "mimeType": "" },
            },
            "cache": {},
            "timings": { "send": 0, "wait": 0, "receive": 0 },
            "_exchangeId": id,
        }));
    }
    Ok(serde_json::json!({
        "log": {
            "version": "1.2",
            "creator": { "name": "teamx", "version": env!("CARGO_PKG_VERSION") },
            "entries": entries,
        }
    }))
}

fn parse_header_list(raw: &str) -> Vec<serde_json::Value> {
    raw.lines()
        .filter_map(|l| {
            let (name, value) = l.split_once(':')?;
            Some(serde_json::json!({ "name": name.trim(), "value": value.trim() }))
        })
        .collect()
}

/// Convert a Unix timestamp (f64 seconds) to ISO 8601 UTC string.
fn timestamp_to_iso(ts: f64) -> String {
    let secs = ts as i64;
    let millis = ((ts - secs as f64) * 1000.0) as u32;
    // Simple calendar math (no timezone library dependency).
    // Days since 1970-01-01.
    let days = secs / 86400;
    let day_secs = (secs % 86400) as u32;
    let hours = day_secs / 3600;
    let mins = (day_secs % 3600) / 60;
    let secs_rem = day_secs % 60;
    // Compute year/month/day from days since epoch.
    let (year, month, day) = days_to_ymd(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{hours:02}:{mins:02}:{secs_rem:02}.{millis:03}Z"
    )
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_ymd(days: i64) -> (i64, u32, u32) {
    // Civil calendar from Howard Hinnant's algorithm.
    let z = days + 719468;
    let era = z / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Emit an equivalent `curl` command for an exchange.
pub fn to_curl(e: &HttpExchange) -> String {
    let mut cmd = format!("curl -X {} '{}'", e.method, e.url);
    for line in e.request_headers.lines() {
        let line = line.trim_end_matches('\r').trim();
        if line.is_empty() {
            continue;
        }
        // skip Host (curl sets it) and auto-length headers
        if line.starts_with("Host:") {
            continue;
        }
        // Only skip Content-Length when there IS a body (curl computes it).
        if line.starts_with("Content-Length:") && !e.request_body.is_empty() {
            continue;
        }
        // Use shell-safe quoting: single-quote the header, escape embedded quotes.
        cmd.push_str(&format!(" -H '{}'", line.replace('\'', "'\\''")));
    }
    if !e.request_body.is_empty() {
        let body = String::from_utf8_lossy(&e.request_body);
        cmd.push_str(&format!(" --data-raw '{}'", body.replace('\'', "'\\''")));
    }
    cmd
}

/// Re-send an exchange's request and return the response status/body.
pub async fn http_send(
    e: &HttpExchange,
    vars: &[(String, String)],
    map: &[(String, String)],
) -> Result<(i64, Vec<u8>), String> {
    let mut url = e.url.clone();
    for (k, v) in vars {
        url = url.replace(&format!("{{{{{k}}}}}"), v);
    }
    for (from, to) in map {
        url = url.replace(from, to);
    }
    // determine host:port
    let (host, port) = {
        let h = if let Some(rest) = url.strip_prefix("http://") {
            rest
        } else {
            return Err("only plain http:// URLs supported for send".into());
        };
        let host = h.split('/').next().unwrap_or(h);
        match host.split_once(':') {
            Some((h, p)) => (h.to_string(), p.parse::<u16>().unwrap_or(80)),
            None => (host.to_string(), 80),
        }
    };
    let mut sock = tokio::net::TcpStream::connect((host.as_str(), port))
        .await
        .map_err(|e| format!("connect {host}:{port}: {e}"))?;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let path = url.strip_prefix("http://").unwrap_or(&url).splitn(2, '/').nth(1).map(|p| format!("/{p}")).unwrap_or_else(|| "/".into());
    let mut req = format!("{} {} HTTP/1.1\r\nHost: {}\r\n", e.method, path, host);
    req.push_str(&e.request_headers);
    // Ensure the header section is terminated by a single blank line.
    // request_headers may or may not end with \r\n; normalize to exactly one.
    if !req.ends_with("\r\n\r\n") {
        if req.ends_with("\r\n") {
            req.push_str("\r\n"); // one more \r\n to form the blank line
        } else {
            req.push_str("\r\n\r\n");
        }
    }
    sock.write_all(req.as_bytes()).await.map_err(|e| format!("send: {e}"))?;
    if !e.request_body.is_empty() {
        sock.write_all(&e.request_body).await.map_err(|e| format!("body: {e}"))?;
    }
    let _ = sock.shutdown().await;
    let mut resp = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(5), sock.read(&mut buf)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => resp.extend_from_slice(&buf[..n]),
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }
    let (status, _headers, body_off) = parse_response(&resp).unwrap_or((0, String::new(), 0));
    Ok((status, resp[body_off.min(resp.len())..].to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{Dir, TapEvent};
    use std::path::PathBuf;

    fn tmp_db(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("teamx-replay-test-{}-{}.db", std::process::id(), name));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn seed_flow(store: &mut CaptureStore, up: &[u8], down: &[u8], remote: &str) -> i64 {
        store
            .apply_batch(vec![
                TapEvent::FlowOpen {
                    flow_id: 1,
                    source: "socks5",
                    local: "10.0.0.2".into(),
                    remote_addr: remote.into(),
                    ts: 1.0,
                    first_bytes: up[..up.len().min(32)].to_vec(),
                },
                TapEvent::Chunk { flow_id: 1, dir: Dir::Up, seq: 1, ts: 1.1, data: up.to_vec() },
                TapEvent::Chunk { flow_id: 1, dir: Dir::Down, seq: 1, ts: 1.2, data: down.to_vec() },
            ])
            .unwrap();
        1
    }

    #[test]
    fn flow_streams_reassemble() {
        let db = tmp_db("streams");
        let mut store = CaptureStore::open(&db).unwrap();
        seed_flow(&mut store, b"GET /", b"HTTP/1.1 200 OK", "1.2.3.4:80");
        let mut r = CaptureStore::open(&db).unwrap();
        let (up, down) = flow_streams(&mut r, 1).unwrap();
        assert_eq!(up, b"GET /");
        assert_eq!(down, b"HTTP/1.1 200 OK");
    }

    #[test]
    fn prefix_match_ratio_basic() {
        assert_eq!(prefix_match_ratio(b"abc", b"abc"), 1.0);
        assert_eq!(prefix_match_ratio(b"abc", b"abx"), 2.0 / 3.0);
        assert_eq!(prefix_match_ratio(b"", b"xyz"), 1.0);
    }

    #[test]
    fn parse_request_and_response() {
        let req = b"POST /api HTTP/1.1\r\nHost: example.com\r\nContent-Length: 4\r\n\r\nbody";
        let (m, u, h, headers, off) = parse_request(req).unwrap();
        assert_eq!(m, "POST");
        assert_eq!(u, "/api");
        assert_eq!(h, "example.com");
        assert!(headers.contains("Content-Length"));
        assert_eq!(&req[off..], b"body");

        let resp = b"HTTP/1.1 201 Created\r\nServer: x\r\n\r\nhi";
        let (status, headers, off) = parse_response(resp).unwrap();
        assert_eq!(status, 201);
        assert!(headers.contains("Server"));
        assert_eq!(&resp[off..], b"hi");
    }

    #[test]
    fn extract_and_har_roundtrip() {
        let db = tmp_db("har");
        let mut store = CaptureStore::open(&db).unwrap();
        seed_flow(
            &mut store,
            b"GET /get HTTP/1.1\r\nHost: httpbin.org\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok",
            "httpbin.org:80",
        );
        let mut r = CaptureStore::open(&db).unwrap();
        let e = extract_exchange(&mut r, 1).unwrap();
        assert_eq!(e.method, "GET");
        assert_eq!(e.status, 200);
        assert_eq!(e.response_body, b"ok");
        let id = store_exchange(&mut r, &e).unwrap();
        assert!(id > 0);

        let har = export_har(&mut r).unwrap();
        let entries = har["log"]["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["request"]["method"], "GET");
        assert_eq!(entries[0]["response"]["status"], 200);

        let curl = to_curl(&e);
        assert!(curl.starts_with("curl -X GET"));
        assert!(curl.contains("httpbin.org"));
    }

    #[tokio::test]
    async fn mock_serve_serves_recorded_bytes() {
        let db = tmp_db("serve");
        let mut store = CaptureStore::open(&db).unwrap();
        seed_flow(
            &mut store,
            b"GET / HTTP/1.1\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello",
            "localhost:1",
        );
        let mut r = CaptureStore::open(&db).unwrap();
        let port = 19001u16;
        let serve = tokio::spawn(async move {
            let _ = serve_flow(&mut r, 1, port).await;
        });
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
        let mut buf = Vec::new();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), s.read_to_end(&mut buf)).await;
        let body = String::from_utf8_lossy(&buf);
        assert!(body.contains("hello"), "got {body:?}");
        serve.abort();
    }

    #[tokio::test]
    async fn tcp_replay_reports_connect_failure() {
        // tcp_replay now tunnels through a teamx server exit; with no server
        // reachable it must classify a connection error rather than panic.
        let db = tmp_db("tcp");
        let mut store = CaptureStore::open(&db).unwrap();
        seed_flow(&mut store, b"ping", b"pong", "www.google.com:443");
        let mut r = CaptureStore::open(&db).unwrap();
        let result = tcp_replay(&mut r, 1, None, "https://127.0.0.1:1", "egress2").await;
        assert!(result.is_err(), "expected connect error, got {result:?}");
    }
}
