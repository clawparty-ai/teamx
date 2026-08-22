//! tunnel_client.rs — CLI-side reverse-tunnel client (provider + consumer).
//!
//! Lets the `teamx tunnel expose` / `teamx tunnel forward` commands work
//! WITHOUT the opencode plugin (pure CLI, mTLS WS).
//!
//! - `expose` (provider): connects to `wss://server/tunnel`, registers a
//!   local service, and relays bytes for each `open_stream` the server sends.
//! - `forward` (consumer): listens on a LOCAL port; each connection opens a
//!   `wss://server/tunnel/forward` WS, sends `{"type":"connect","name"}` and
//!   bridges bytes (local-forward mode; server binds no port).
//!
//! mTLS material resolution (mirrors opencode-plugin/src/client.ts):
//!   1. explicit env: TEAMX_MTLS_CERT / TEAMX_MTLS_KEY / TEAMX_MTLS_CA
//!   2. otherwise auto-discover from ~/.teamx/letters/<id>/ (matching the
//!      server host, falling back to the most recently imported letter).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ClientConfig;

/// Minimal wrapper over the resolved mTLS material.
pub struct MtlsMaterial {
    pub cert: CertificateDer<'static>,
    pub key: PrivateKeyDer<'static>,
    pub ca: Vec<CertificateDer<'static>>,
}

/// Discover the mTLS material for a server URL.
/// Returns None if nothing usable is found (plain CLI mode has no certs).
pub fn mtls_for(server_url: &str) -> Option<MtlsMaterial> {
    if let Some(m) = env_mtls() {
        return Some(m);
    }
    letter_mtls(server_url)
}

fn read_pem(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

fn parse_cert(pem: &str) -> Option<CertificateDer<'static>> {
    rustls_pemfile::certs(&mut pem.as_bytes()).next().transpose().ok().flatten()
}

fn parse_key(pem: &str) -> Option<PrivateKeyDer<'static>> {
    rustls_pemfile::private_key(&mut pem.as_bytes())
        .ok()
        .flatten()
}

/// Build an MtlsMaterial from raw PEM strings. Returns None (with a stderr
/// note) instead of panicking on malformed material.
fn material_from_pem(cert_pem: &str, key_pem: &str, ca_pem: &str, source: &str) -> Option<MtlsMaterial> {
    let cert = match parse_cert(cert_pem) {
        Some(c) => c,
        None => {
            eprintln!("teamx: {source}: no PEM certificate found");
            return None;
        }
    };
    let key = match parse_key(key_pem) {
        Some(k) => k,
        None => {
            eprintln!("teamx: {source}: no PEM private key found");
            return None;
        }
    };
    let ca = match parse_cert(ca_pem) {
        Some(c) => c,
        None => {
            eprintln!("teamx: {source}: no CA certificate found");
            return None;
        }
    };
    Some(MtlsMaterial {
        cert,
        key,
        ca: vec![ca],
    })
}

/// Env override: TEAMX_MTLS_CERT/KEY/CA point at PEM files.
fn env_mtls() -> Option<MtlsMaterial> {
    let cert = std::env::var("TEAMX_MTLS_CERT").ok()?;
    let key = std::env::var("TEAMX_MTLS_KEY").ok()?;
    let ca = std::env::var("TEAMX_MTLS_CA").ok()?;
    if cert.is_empty() || key.is_empty() || ca.is_empty() {
        return None;
    }
    let cert_pem = read_pem(Path::new(&cert))?;
    let key_pem = read_pem(Path::new(&key))?;
    let ca_pem = read_pem(Path::new(&ca))?;
    material_from_pem(&cert_pem, &key_pem, &ca_pem, "TEAMX_MTLS_CERT/KEY/CA")
}

/// Host portion of a URL.
fn host_of(url: &str) -> String {
    url.split("://").nth(1).unwrap_or(url).split('/').next().unwrap_or("").to_string()
}

/// Auto-discover from imported letters, preferring the one whose embedded
/// server URL matches `server_url`; else the most recent.
fn letter_mtls(server_url: &str) -> Option<MtlsMaterial> {
    let home = crate::db::teamx_home();
    let dir = home.join("letters");
    if !dir.is_dir() {
        return None;
    }
    let wanted_host = host_of(server_url);
    // Prefer a letter whose server host matches, and among those the most
    // recently imported. If none matches the host, fall back to the most
    // recent letter overall. (Old letters signed by a rotated CA must not be
    // picked when a newer matching letter exists.)
    let mut best: Option<(PathBuf, u128)> = None;
    let mut best_exact: Option<(PathBuf, u128)> = None;
    if let Ok(entries) = fs::read_dir(&dir) {
        for e in entries.flatten() {
            let sub = e.path();
            let letter_path = sub.join("letter.json");
            if !letter_path.is_file() {
                continue;
            }
            let cert = sub.join("client.crt");
            let key = sub.join("client.key");
            let ca = sub.join("ca.crt");
            if !cert.is_file() || !key.is_file() || !ca.is_file() {
                continue;
            }
            let host = read_pem(&letter_path)
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .and_then(|v| v["teamx_invitation"]["server"]["url"].as_str().map(str::to_string))
                .map(|u| host_of(&u))
                .unwrap_or_default();
            let mtime = letter_path.metadata().and_then(|m| m.modified()).map(|t| {
                t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
            }).unwrap_or(0);
            let is_exact = !host.is_empty() && host == wanted_host;
            if is_exact && best_exact.as_ref().map(|(_, m)| mtime > *m).unwrap_or(true) {
                best_exact = Some((sub.clone(), mtime));
            }
            if best.as_ref().map(|(_, m)| mtime > *m).unwrap_or(true) {
                best = Some((sub.clone(), mtime));
            }
        }
    }
    let chosen = best_exact.or(best).map(|(p, _)| p)?;
    let cert = read_pem(&chosen.join("client.crt"))?;
    let key = read_pem(&chosen.join("client.key"))?;
    let ca = read_pem(&chosen.join("ca.crt"))?;
    material_from_pem(&cert, &key, &ca, "invitation letter")
}

/// Build a rustls ClientConfig with the mTLS material (server cert verified
/// against the letter's CA).
pub fn client_config(mtls: &MtlsMaterial) -> Result<Arc<ClientConfig>, String> {
    let mut roots = rustls::RootCertStore::empty();
    for c in &mtls.ca {
        roots.add(c.clone()).map_err(|e| format!("add CA: {e}"))?;
    }
    let builder = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_client_auth_cert(vec![mtls.cert.clone()], mtls.key.clone_key())
        .map_err(|e| format!("client auth: {e}"))?;
    Ok(Arc::new(builder))
}

/// Synchronous entrypoints for the CLI (block on an internal runtime).
/// The tunnel commands run forever (expose/forward) or return immediately (rpc).
/// `teamx tunnel expose` — block forever relaying bytes.
pub fn expose(server_url: &str, name: &str, port: u16, mode: &str, lan_ip: Option<&str>) -> Result<serde_json::Value, String> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| format!("runtime: {e}"))?;
    rt.block_on(run_expose(server_url, name, port, mode, lan_ip))?;
    Ok(serde_json::json!({ "ok": true }))
}

/// `teamx tunnel forward` — block forever bridging local connections.
pub fn forward(server_url: &str, name: &str, local_port: u16) -> Result<serde_json::Value, String> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| format!("runtime: {e}"))?;
    rt.block_on(run_forward(server_url, name, local_port))?;
    Ok(serde_json::json!({ "ok": true }))
}

/// `teamx proxy start` — block forever serving SOCKS5 on a local port,
/// tunnelling every CONNECT to the team's proxy exit through the server.
pub fn socks5_proxy(server_url: &str, exit_name: &str, local_port: u16) -> Result<serde_json::Value, String> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| format!("runtime: {e}"))?;
    rt.block_on(run_socks5_proxy(server_url, exit_name, local_port))?;
    Ok(serde_json::json!({ "ok": true }))
}

/// `teamx proxy exit` — block forever as a proxy exit (provider side).
/// Registers mode=proxy and dials the SOCKS5 target of each stream.
pub fn proxy_exit(server_url: &str, name: &str) -> Result<serde_json::Value, String> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| format!("runtime: {e}"))?;
    rt.block_on(run_expose(server_url, name, 0, "proxy", None))?;
    Ok(serde_json::json!({ "ok": true }))
}

/// `teamx tunnel list|status|close` — one RPC call, print the JSON result.
pub fn rpc(server_url: &str, method: &str, args: serde_json::Value) -> Result<serde_json::Value, String> {
    run_rpc(server_url, method, args)
}

// ---------------------------------------------------------------------------
// CLI entrypoints: expose (provider) and forward (consumer).
// ---------------------------------------------------------------------------

use tokio::net::TcpListener;
use tokio_tungstenite::connect_async_tls_with_config;
use tokio_tungstenite::Connector;
use tokio_tungstenite::tungstenite::Message;

/// ws(s):// from an http(s):// server URL.
fn ws_url(server_url: &str, path: &str) -> String {
    let base = server_url.replace("https://", "wss://").replace("http://", "ws://");
    format!("{}{}", base.trim_end_matches('/'), path)
}

async fn connect_tunnel_ws(server_url: &str, path: &str) -> Result<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, String> {
    let mtls = mtls_for(server_url).ok_or_else(|| {
        "no mTLS material: import an invitation letter or set TEAMX_MTLS_CERT/KEY/CA".to_string()
    })?;
    let config = client_config(&mtls)?;
    let connector = Connector::Rustls(config);
    let url = ws_url(server_url, path);
    let (_ws, _resp) = connect_async_tls_with_config(url.as_str(), None, false, Some(connector))
        .await
        .map_err(|e| format!("connect {url}: {e}"))?;
    Ok(_ws)
}

/// Provider side: expose a local service. Registers the tunnel and relays
/// bytes between the server WS and the local service. Runs forever, and
/// automatically reconnects (with backoff) if the WS is dropped — a long-idle
/// tunnel WS can be silently closed by NAT/middleboxes, which would leave the
/// registered tunnel stale and strand consumers (SOCKS5 (5)).
pub async fn run_expose(server_url: &str, name: &str, port: u16, mode: &str, lan_ip: Option<&str>) -> Result<(), String> {
    // Fixed small backoff: reconnect fast (1s) on the first retry, growing to
    // at most 30s so a long outage does not hammer the server.
    let mut backoff = Duration::from_secs(1);
    loop {
        match expose_once(server_url, name, port, mode, lan_ip).await {
            Ok(()) => return Ok(()), // clean shutdown path (unused today)
            Err(e) => {
                eprintln!("tunnel `{name}`: connection lost ({e}); reconnecting in {}s", backoff.as_secs());
                tokio::time::sleep(backoff).await;
                if backoff.as_secs() < 30 {
                    backoff = backoff.saturating_mul(2);
                }
            }
        }
    }
}

/// One connected lifetime of the provider-side tunnel relay (no reconnect).
async fn expose_once(server_url: &str, name: &str, port: u16, mode: &str, lan_ip: Option<&str>) -> Result<(), String> {
    use futures_util::{SinkExt, StreamExt};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut ws = connect_tunnel_ws(server_url, "/tunnel").await?;
    // register
    let mut reg = serde_json::json!({ "type": "register", "name": name, "port": port, "mode": mode });
    if let Some(ip) = lan_ip {
        reg["lan_ip"] = serde_json::json!(ip);
    }
    ws.send(Message::Text(reg.to_string())).await.map_err(|e| format!("register: {e}"))?;

    // Wait for the `registered` ack to learn the public port.
    let public_port = loop {
        match ws.next().await {
            Some(Ok(Message::Text(t))) => {
                let v: serde_json::Value = serde_json::from_str(t.as_str()).map_err(|e| format!("bad ack: {e}"))?;
                if v["type"] == "registered" {
                    break v["port"].as_u64().unwrap_or(0) as u16;
                }
                if v["type"] == "error" {
                    return Err(format!("server error: {}", v["message"].as_str().unwrap_or("")));
                }
            }
            Some(Ok(_)) => {}
            Some(Err(e)) => return Err(format!("ws error: {e}")),
            None => return Err("server closed the connection".to_string()),
        }
    };
    println!("ok tunnel registered: name={name} mode={mode} port={public_port}");

    // Channel: local-service tasks push frames here; the main loop forwards
    // them to the server WS. Binary frames carry [sid][payload]; text frames
    // carry control messages (e.g. close_stream after a failed local dial).
    let (to_server_tx, mut to_server_rx) = tokio::sync::mpsc::unbounded_channel::<Message>();
    // Per-stream writer channels: server→local bytes for each open stream.
    // sid → Sender that delivers payload bytes to the stream's local socket.
    let writers: Arc<std::sync::Mutex<std::collections::HashMap<u64, tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>> =
        Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));

    loop {
        tokio::select! {
            m = ws.next() => match m {
                Some(Ok(Message::Text(t))) => {
                    let v: serde_json::Value = serde_json::from_str(t.as_str()).unwrap_or_else(|_| serde_json::json!({}));
                    match v["type"].as_str() {
                        Some("ping") => {
                            // Application-level heartbeat: reply so the server
                            // knows we are alive and the WS is not half-open.
                            let _ = ws.send(Message::Text(serde_json::json!({"type":"pong"}).to_string())).await;
                        }
                        _ => {
                            if v["type"] == "open_stream" {
                        let sid = v["stream_id"].as_u64().unwrap_or(0);
                        if sid == 0 {
                            continue;
                        }
                        // Proxy-mode exits carry a dynamic target (host:port);
                        // otherwise dial the fixed local port.
                        let target = v.get("target").and_then(|t| t.as_str()).map(str::to_string);
                        let dial: String = target.unwrap_or_else(|| format!("127.0.0.1:{port}"));
                        let writers2 = writers.clone();
                        let tx2 = to_server_tx.clone();
                        match tokio::net::TcpStream::connect(dial.as_str()).await {
                            Ok(sock) => {
                                let (w_tx, mut w_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
                                writers2.lock().unwrap().insert(sid, w_tx);
                                tokio::spawn(async move {
                                    let (mut r, mut w) = sock.into_split();
                                    let mut buf = [0u8; 8192];
                                    let mut write_err = false;
                                    loop {
                                        tokio::select! {
                                            n = r.read(&mut buf) => match n {
                                                Ok(0) | Err(_) => break,
                                                Ok(n) => {
                                                    let mut frame = Vec::with_capacity(4 + n);
                                                    frame.extend_from_slice(&(sid as u32).to_be_bytes());
                                                    frame.extend_from_slice(&buf[..n]);
                                                    if tx2.send(Message::Binary(frame)).is_err() {
                                                        break;
                                                    }
                                                }
                                            },
                                            bytes = w_rx.recv() => match bytes {
                                                Some(b) => {
                                                    if w.write_all(&b).await.is_err() {
                                                        write_err = true;
                                                        break;
                                                    }
                                                }
                                                None => break,
                                            },
                                        }
                                    }
                                    let _ = write_err;
                                    writers2.lock().unwrap().remove(&sid);
                                    // The local end is gone: tell the server to tear
                                    // the stream down so the consumer is not left
                                    // waiting on a half-open connection.
                                    let _ = tx2.send(Message::Text(
                                        serde_json::json!({ "type": "close_stream", "stream_id": sid }).to_string(),
                                    ));
                                });
                            }
                            Err(e) => {
                                eprintln!("tunnel `{name}`: connect local service {dial}: {e}");
                                // Dial failed: notify the server so the consumer
                                // learns the stream will never carry data.
                                let _ = tx2.send(Message::Text(
                                    serde_json::json!({ "type": "close_stream", "stream_id": sid }).to_string(),
                                ));
                            }
                        }
                    }
                        }
                    }
                }
                Some(Ok(Message::Binary(f))) => {
                    // server → provider: [4B stream_id][payload] → local socket
                    if f.len() >= 4 {
                        let sid = u32::from_be_bytes([f[0], f[1], f[2], f[3]]) as u64;
                        if let Some(tx) = writers.lock().unwrap().get(&sid) {
                            let _ = tx.send(f[4..].to_vec());
                        }
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(format!("ws error: {e}")),
                None => return Err("server closed the connection".to_string()),
            },
            m = to_server_rx.recv() => {
                match m {
                    Some(m) => {
                        if ws.send(m).await.is_err() {
                            return Err("server closed the connection".to_string());
                        }
                    }
                    None => break,
                }
            }
        }
    }
    Ok(())
}

/// Consumer side: listen on a local port and bridge each connection to the
/// provider's tunnel through the server (`/tunnel/forward`). Runs forever.
pub async fn run_forward(server_url: &str, name: &str, local_port: u16) -> Result<(), String> {
    use futures_util::{SinkExt, StreamExt};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = TcpListener::bind(("127.0.0.1", local_port)).await
        .map_err(|e| format!("bind 127.0.0.1:{local_port}: {e}"))?;
    println!("ok forward: name={name} listening on 127.0.0.1:{local_port} (access like a local service)");

    loop {
        let (client, _peer) = listener.accept().await.map_err(|e| format!("accept: {e}"))?;
        let server_url = server_url.to_string();
        let name = name.to_string();
        tokio::spawn(async move {
            let (mut c_read, mut c_write) = client.into_split();
            let mtls = match mtls_for(&server_url) {
                Some(m) => m,
                None => return,
            };
            let config = match client_config(&mtls) {
                Ok(c) => c,
                Err(_) => return,
            };
            let connector = Connector::Rustls(config);
            let url = ws_url(&server_url, "/tunnel/forward");
            let (mut ws, _) = match connect_async_tls_with_config(url.as_str(), None, false, Some(connector)).await {
                Ok(v) => v,
                Err(_) => return,
            };
            let _ = ws
                .send(Message::Text(serde_json::json!({ "type": "connect", "name": name }).to_string()))
                .await;

            // Wait for `stream_open`, buffering bytes read from the local
            // connection meanwhile.
            let mut sid: Option<u64> = None;
            let mut pending: Vec<Vec<u8>> = Vec::new();
            let mut buf = [0u8; 8192];

            loop {
                tokio::select! {
                    m = ws.next() => match m {
                        Some(Ok(Message::Text(t))) => {
                            let v: serde_json::Value = serde_json::from_str(t.as_str()).unwrap_or_else(|_| serde_json::json!({}));
                            match v["type"].as_str() {
                                Some("stream_open") => {
                                    sid = v["stream_id"].as_u64();
                                    if let Some(s) = sid {
                                        // flush buffered local bytes to the server
                                        for b in pending.drain(..) {
                                            let mut frame = Vec::with_capacity(4 + b.len());
                                            frame.extend_from_slice(&(s as u32).to_be_bytes());
                                            frame.extend_from_slice(&b);
                                            if ws.send(Message::Binary(frame)).await.is_err() {
                                                return;
                                            }
                                        }
                                    }
                                }
                                Some("error") => return,
                                Some("ping") => {
                                    // Application-level heartbeat: reply so the
                                    // server knows we are alive.
                                    let _ = ws.send(Message::Text(serde_json::json!({"type":"pong"}).to_string())).await;
                                }
                                _ => {}
                            }
                        }
                        Some(Ok(Message::Binary(f))) => {
                            // server → consumer: [4B stream_id][payload] → local socket
                            if f.len() >= 4 && sid.is_some() {
                                let _ = c_write.write_all(&f[4..]).await;
                            }
                        }
                        Some(Ok(_)) => {}
                        Some(Err(_)) | None => return,
                    },
                    n = c_read.read(&mut buf) => match n {
                        Ok(0) | Err(_) => return,
                        Ok(n) => {
                            match sid {
                                Some(s) => {
                                    let mut frame = Vec::with_capacity(4 + n);
                                    frame.extend_from_slice(&(s as u32).to_be_bytes());
                                    frame.extend_from_slice(&buf[..n]);
                                    if ws.send(Message::Binary(frame)).await.is_err() {
                                        return;
                                    }
                                }
                                None => pending.push(buf[..n].to_vec()),
                            }
                        }
                    },
                }
            }
        });
    }
}

/// Consumer side (proxy): serve SOCKS5 on a local port. Each accepted
/// connection completes the SOCKS5 handshake, parses the CONNECT target,
/// opens a tunnel stream to the team's proxy exit (sending the target), and
/// bridges bytes. Runs forever.
pub async fn run_socks5_proxy(server_url: &str, exit_name: &str, local_port: u16) -> Result<(), String> {
    use futures_util::{SinkExt, StreamExt};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = TcpListener::bind(("127.0.0.1", local_port)).await
        .map_err(|e| format!("bind 127.0.0.1:{local_port}: {e}"))?;
    println!("ok proxy: exit={exit_name} SOCKS5 listening on 127.0.0.1:{local_port} (set curl --socks5-hostname or browser proxy)");

    loop {
        let (client, _peer) = listener.accept().await.map_err(|e| format!("accept: {e}"))?;
        let server_url = server_url.to_string();
        let exit_name = exit_name.to_string();
        tokio::spawn(async move {
            let (mut c_read, mut c_write) = client.into_split();

            // 1. SOCKS5 greeting: VER NMETHODS METHODS... -> reply 05 00.
            let mut gbuf = [0u8; 64];
            let mut gused = 0;
            let greeting_ok = loop {
                match c_read.read(&mut gbuf[gused..]).await {
                    Ok(0) => break false,
                    Ok(n) => {
                        gused += n;
                        match crate::socks5::parse_greeting(&gbuf[..gused]) {
                            Ok(_) => break true,
                            Err(e) if gused >= 2 => {
                                let _ = c_write.write_all(&[0x05, 0xff]).await;
                                eprintln!("proxy: greeting rejected: {e}");
                                break false;
                            }
                            Err(_) => continue, // need more bytes
                        }
                    }
                    Err(_) => break false,
                }
            };
            if !greeting_ok {
                return;
            }
            if c_write.write_all(&[0x05, 0x00]).await.is_err() {
                return;
            }

            // 2. CONNECT request: read until parseable.
            let mut rbuf = [0u8; 512];
            let mut rused = 0;
            let target = loop {
                match c_read.read(&mut rbuf[rused..]).await {
                    Ok(0) => {
                        let _ = c_write.write_all(&crate::socks5::SOCKS5_FAIL_REPLY).await;
                        return;
                    }
                    Ok(n) => {
                        rused += n;
                        match crate::socks5::parse_connect_request(&rbuf[..rused]) {
                            Ok((_, t)) => break t,
                            Err(e) if rused >= 4 => {
                                let _ = c_write.write_all(&crate::socks5::SOCKS5_FAIL_REPLY).await;
                                eprintln!("proxy: connect rejected: {e}");
                                return;
                            }
                            Err(_) => continue, // need more bytes
                        }
                    }
                    Err(_) => return,
                }
            };

            // 3. Open a tunnel stream to the exit with the target address.
            let mtls = match mtls_for(&server_url) {
                Some(m) => m,
                None => {
                    let _ = c_write.write_all(&crate::socks5::SOCKS5_FAIL_REPLY).await;
                    return;
                }
            };
            let config = match client_config(&mtls) {
                Ok(c) => c,
                Err(_) => {
                    let _ = c_write.write_all(&crate::socks5::SOCKS5_FAIL_REPLY).await;
                    return;
                }
            };
            let connector = Connector::Rustls(config);
            let url = ws_url(&server_url, "/tunnel/forward");
            let (mut ws, _) = match connect_async_tls_with_config(url.as_str(), None, false, Some(connector)).await {
                Ok(v) => v,
                Err(_) => {
                    let _ = c_write.write_all(&crate::socks5::SOCKS5_FAIL_REPLY).await;
                    return;
                }
            };
            let _ = ws
                .send(Message::Text(
                    serde_json::json!({ "type": "connect", "name": exit_name, "target": format!("{}:{}", target.host, target.port) })
                        .to_string(),
                ))
                .await;

            // 4. Wait for `stream_open`; buffer local bytes meanwhile.
            let mut sid: Option<u64> = None;
            let mut pending: Vec<Vec<u8>> = Vec::new();
            let mut buf = [0u8; 8192];

            loop {
                tokio::select! {
                    m = ws.next() => match m {
                        Some(Ok(Message::Text(t))) => {
                            let v: serde_json::Value = serde_json::from_str(t.as_str()).unwrap_or_else(|_| serde_json::json!({}));
                            match v["type"].as_str() {
                                Some("stream_open") => {
                                    sid = v["stream_id"].as_u64();
                                    if let Some(s) = sid {
                                        // SOCKS5 success reply, then flush buffered bytes.
                                        let _ = c_write.write_all(&crate::socks5::SOCKS5_SUCCESS_REPLY).await;
                                        for b in pending.drain(..) {
                                            let mut frame = Vec::with_capacity(4 + b.len());
                                            frame.extend_from_slice(&(s as u32).to_be_bytes());
                                            frame.extend_from_slice(&b);
                                            if ws.send(Message::Binary(frame)).await.is_err() {
                                                return;
                                            }
                                        }
                                    }
                                }
                                Some("error") => {
                                    let _ = c_write.write_all(&crate::socks5::SOCKS5_FAIL_REPLY).await;
                                    return;
                                }
                                Some("ping") => {
                                    // Application-level heartbeat: reply so the
                                    // server knows we are alive.
                                    let _ = ws.send(Message::Text(serde_json::json!({"type":"pong"}).to_string())).await;
                                }
                                _ => {}
                            }
                        }
                        Some(Ok(Message::Binary(f))) => {
                            if f.len() >= 4 && sid.is_some() {
                                let _ = c_write.write_all(&f[4..]).await;
                            }
                        }
                        Some(Ok(_)) => {}
                        Some(Err(_)) | None => return,
                    },
                    n = c_read.read(&mut buf) => match n {
                        Ok(0) | Err(_) => return,
                        Ok(n) => {
                            match sid {
                                Some(s) => {
                                    let mut frame = Vec::with_capacity(4 + n);
                                    frame.extend_from_slice(&(s as u32).to_be_bytes());
                                    frame.extend_from_slice(&buf[..n]);
                                    if ws.send(Message::Binary(frame)).await.is_err() {
                                        return;
                                    }
                                }
                                None => pending.push(buf[..n].to_vec()),
                            }
                        }
                    },
                }
            }
        });
    }
}

/// Discover the server URL from the most recently imported letter.
pub fn discover_server_url() -> Option<String> {
    let home = crate::db::teamx_home();
    let dir = home.join("letters");
    if !dir.is_dir() {
        return None;
    }
    let mut best: Option<(String, u128)> = None;
    if let Ok(entries) = fs::read_dir(&dir) {
        for e in entries.flatten() {
            let letter_path = e.path().join("letter.json");
            if !letter_path.is_file() {
                continue;
            }
            let url = read_pem(&letter_path)
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .and_then(|v| v["teamx_invitation"]["server"]["url"].as_str().map(str::to_string));
            let mtime = letter_path.metadata().and_then(|m| m.modified()).ok().and_then(|t| {
                t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_millis())
            }).unwrap_or(0);
            if let Some(url) = url {
                if best.as_ref().map(|(_, m)| mtime > *m).unwrap_or(true) {
                    best = Some((url, mtime));
                }
            }
        }
    }
    best.map(|(u, _)| u)
}

/// Run a short HTTP JSON-RPC call (tunnel.list / status / close).
pub fn run_rpc(server_url: &str, method: &str, args: serde_json::Value) -> Result<serde_json::Value, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    rt.block_on(async {
        let mtls = mtls_for(server_url).ok_or_else(|| "no mTLS material for RPC".to_string())?;
        let config = client_config(&mtls)?;
        let body = serde_json::json!({ "method": method, "args": args }).to_string();
        let resp = https_post(server_url, "/rpc", &body, &config).await?;
        serde_json::from_str(&resp).map_err(|e| format!("bad rpc response: {e}"))
    })
}

/// Minimal HTTPS POST with mTLS (rustls client over tokio TcpStream).
/// Hardened with a 15s overall timeout and an HTTP status check so a stalled
/// server or an error page is surfaced as an error, not garbage JSON.
async fn https_post(server_url: &str, path: &str, body: &str, config: &Arc<ClientConfig>) -> Result<String, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_rustls::TlsConnector;

    let host = server_url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/');
    let (host_name, port) = match host.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().unwrap_or(443)),
        None => (host.to_string(), 443),
    };
    let addr = format!("{host_name}:{port}");
    let request = async {
        let tcp = tokio::net::TcpStream::connect(&addr)
            .await
            .map_err(|e| format!("connect {addr}: {e}"))?;
        let server_name = rustls::pki_types::ServerName::try_from(host_name.clone())
            .map_err(|e| format!("invalid server name {host_name}: {e}"))?;
        let connector = TlsConnector::from(config.clone());
        let mut tls = connector.connect(server_name, tcp).await.map_err(|e| format!("tls: {e}"))?;

        let req = format!(
            "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        tls.write_all(req.as_bytes()).await.map_err(|e| format!("write: {e}"))?;
        let mut resp = Vec::new();
        tls.read_to_end(&mut resp).await.map_err(|e| format!("read: {e}"))?;
        Ok::<_, String>(String::from_utf8_lossy(&resp).to_string())
    };
    let text = tokio::time::timeout(std::time::Duration::from_secs(15), request)
        .await
        .map_err(|_| "rpc timed out after 15s".to_string())??;

    // Split status line / headers / body at the blank line.
    let (head, body_part) = match text.split_once("\r\n\r\n") {
        Some((h, b)) => (h, b),
        None => (text.as_str(), text.as_str()),
    };
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("malformed http response: {}", head.lines().next().unwrap_or("empty")))?;
    if !(200..300).contains(&status) {
        return Err(format!("http {status}: {}", body_part.trim().chars().take(200).collect::<String>()));
    }
    Ok(body_part.trim().to_string())
}
