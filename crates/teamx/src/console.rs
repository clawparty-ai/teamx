//! console.rs — `teamx console`: member web console (local, view-only).
//!
//! Runs from a workspace and serves a browser console branded "Teamx Console"
//! over HTTPS on 127.0.0.1 with a random per-launch token (same UX as
//! `teamx ui`). Data comes from the same CLI/RPC calls the workspace would
//! make; the console never writes team state. The only file it writes is the
//! cloud-node list (`.teamx/console.json`).
//!
//! Cloud panel: entries point at local ports the user has already `tunnel
//! forward`ed (headless opencode / ttyd). The console proxies them same-origin
//! (`/api/cloud/{id}/...`) so iframes work under the HTTPS console and, for
//! terminal apps like ttyd, relays the WebSocket upgrade.

use std::sync::Arc;

use axum::{
    body::{to_bytes as body_to_bytes, Body, Bytes},
    extract::{
        ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    http::{header, HeaderMap, Method, Request, Response, StatusCode, Uri},
    response::{Html, IntoResponse},
    routing::{delete, get},
    Json, Router,
};
use axum_server::tls_rustls::RustlsConfig;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;

use crate::cli::ConsoleCmd;
use crate::console_data::{
    config_path, fetch_me, fetch_panel, load_config, resolve_mode, save_config, slugify,
    valid_node_url, Ctx, Mode, RunOut,
};
use crate::console_html::{login_html, not_found_html, page_html, unauthorized_html};
use crate::pki;

/// Cookie name holding the auth token.
const COOKIE: &str = "teamx_console_token";

/// Entry point. Blocks until the server stops.
pub fn run(cmd: &ConsoleCmd) -> Result<(), String> {
    let home = crate::db::teamx_home();
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );

    // HTTPS with the workspace's instance server cert (same as `teamx ui`).
    let pk = pki::ensure_pki(&home)?;
    let server_cert_pem = std::fs::read(&pk.server_cert).map_err(|e| format!("read server cert: {e}"))?;
    let server_key_pem = std::fs::read(&pk.server_key).map_err(|e| format!("read server key: {e}"))?;
    let cert_chain = rustls_pemfile::certs(&mut server_cert_pem.as_slice())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("parse server cert chain: {e}"))?;
    let key_der = rustls_pemfile::private_key(&mut server_key_pem.as_slice())
        .map_err(|e| format!("parse server key: {e}"))?
        .ok_or_else(|| "no private key in server.key".to_string())?;
    let tls_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key_der)
        .map_err(|e| format!("tls config: {e}"))?;

    // Data mode: network (server URL discovered) or local (workspace DB).
    let mode = resolve_mode(cmd.server_url.as_deref());
    let cfg_path = config_path(&home, &cmd.config);
    let session = cmd.session.clone().or_else(|| {
        crate::console_data::pick_local_session(&home)
            .or_else(|| std::env::var("TEAMX_SESSION").ok().filter(|s| !s.is_empty()))
    });
    let ctx = Ctx {
        mode,
        session,
        home: home.clone(),
        config_path: cfg_path.clone(),
    };

    let state = Arc::new(StateA {
        token: token.clone(),
        ctx: ctx.clone(),
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/login", get(login_page))
        .route("/api/panels/{id}", get(api_panel))
        .route("/api/me", get(api_me))
        .route("/api/cloud", get(api_cloud_get).post(api_cloud_post))
        .route(
            "/api/cloud/{id}",
            delete(api_cloud_delete).get(api_cloud_proxy_root),
        )
        .route("/api/cloud/{id}/", get(api_cloud_proxy_root))
        .route("/api/cloud/{id}/ws", get(api_cloud_ws))
        .route("/api/cloud/{id}/{*path}", get(api_cloud_proxy_rest))
        .fallback(not_found_page)
        .with_state(state);

    let bind_str = if cmd.addr.contains(':') {
        format!("[{}]:{}", cmd.addr, cmd.port)
    } else {
        format!("{}:{}", cmd.addr, cmd.port)
    };
    let addr: std::net::SocketAddr = bind_str
        .parse()
        .map_err(|e| format!("invalid bind address {bind_str}: {e}"))?;

    match &ctx.mode {
        Mode::Network(url) => eprintln!("teamx console: member web console (network mode, server {url})"),
        Mode::Local => eprintln!("teamx console: member web console (project/local mode)"),
    }
    eprintln!("  open: https://{}/?token={}", addr, token);
    eprintln!("  workspace: {}", home.display());
    eprintln!("  (self-signed cert; your browser will warn about it once)");
    eprintln!("  Ctrl+C to stop");

    let config = RustlsConfig::from_config(Arc::new(tls_config));
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
    rt.block_on(async {
        axum_server::bind_rustls(addr, config)
            .serve(app.into_make_service())
            .await
            .map_err(|e| format!("server error: {e}"))
    })
}

type S = Arc<StateA>;

struct StateA {
    token: String,
    ctx: Ctx,
}

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

fn authorized(state: &S, cookies: Option<&str>) -> bool {
    match cookies {
        Some(c) => c.split(';').any(|kv| {
            let mut it = kv.trim().splitn(2, '=');
            it.next() == Some(COOKIE) && it.next() == Some(&state.token)
        }),
        None => false,
    }
}

async fn index(State(state): State<S>, headers: HeaderMap, request: Request<Body>) -> axum::response::Response {
    // ?token=... sets the cookie and redirects to /.
    let query = request.uri().query().unwrap_or("").to_string();
    if let Some(t) = query_token(&query) {
        if t == state.token {
            let mut resp = axum::response::Redirect::to("/").into_response();
            let val = format!("{COOKIE}={}; HttpOnly; SameSite=Strict; Path=/", state.token);
            resp.headers_mut().insert(header::SET_COOKIE, val.parse().unwrap());
            return resp;
        }
    }
    if authorized(&state, headers.get(header::COOKIE).and_then(|v| v.to_str().ok())) {
        return Html(page_html()).into_response();
    }
    axum::response::Redirect::to("/login").into_response()
}

fn query_token(query: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let mut it = kv.splitn(2, '=');
        if it.next() == Some("token") {
            it.next().map(|v| v.replace("+", " ").to_string())
        } else {
            None
        }
    })
}

async fn login_page() -> Html<String> {
    Html(login_html())
}

async fn not_found_page() -> (StatusCode, Html<String>) {
    (StatusCode::NOT_FOUND, Html(not_found_html()))
}

#[allow(clippy::result_large_err)]
fn guard(state: &S, headers: &HeaderMap) -> Result<(), axum::response::Response> {
    if authorized(state, headers.get(header::COOKIE).and_then(|v| v.to_str().ok())) {
        Ok(())
    } else {
        Err((StatusCode::UNAUTHORIZED, Html(unauthorized_html())).into_response())
    }
}

fn err_json(code: StatusCode, msg: String) -> axum::response::Response {
    (code, Json(json!({ "ok": false, "error": msg }))).into_response()
}

// ---------------------------------------------------------------------------
// Panels
// ---------------------------------------------------------------------------

async fn api_panel(
    State(state): State<S>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> axum::response::Response {
    if let Err(resp) = guard(&state, &headers) {
        return resp;
    }
    let ctx = state.ctx.clone();
    let id = id.clone();
    let out: RunOut = tokio::task::spawn_blocking(move || fetch_panel(&ctx, &id))
        .await
        .unwrap_or_else(|_| RunOut {
            ok: false,
            mode: "",
            cmd: "panel".to_string(),
            error: Some("panel task panicked".to_string()),
            data: None,
            elapsed_ms: 0,
        });
    (StatusCode::OK, Json(out)).into_response()
}

/// Header identity: the current workspace member (`/api/me`).
async fn api_me(State(state): State<S>, headers: HeaderMap) -> axum::response::Response {
    if let Err(resp) = guard(&state, &headers) {
        return resp;
    }
    let ctx = state.ctx.clone();
    let out: RunOut = tokio::task::spawn_blocking(move || fetch_me(&ctx))
        .await
        .unwrap_or_else(|_| RunOut {
            ok: false,
            mode: "rpc",
            cmd: "teamx whoami".to_string(),
            error: Some("whoami task panicked".to_string()),
            data: None,
            elapsed_ms: 0,
        });
    (StatusCode::OK, Json(out)).into_response()
}

// ---------------------------------------------------------------------------
// Cloud node list (.teamx/console.json)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CloudAdd {
    name: String,
    url: String,
}

async fn api_cloud_get(State(state): State<S>, headers: HeaderMap) -> axum::response::Response {
    if let Err(resp) = guard(&state, &headers) {
        return resp;
    }
    let cfg = load_config(&state.ctx.config_path);
    (StatusCode::OK, Json(cfg)).into_response()
}

async fn api_cloud_post(
    State(state): State<S>,
    headers: HeaderMap,
    Json(add): Json<CloudAdd>,
) -> axum::response::Response {
    if let Err(resp) = guard(&state, &headers) {
        return resp;
    }
    let name = add.name.trim().to_string();
    let url = add.url.trim().to_string();
    if name.is_empty() || url.is_empty() {
        return err_json(StatusCode::BAD_REQUEST, "name and url required".to_string());
    }
    if !valid_node_url(&url) {
        return err_json(
            StatusCode::BAD_REQUEST,
            "url must be a loopback address (http://127.0.0.1:<port> or http://localhost:<port>)"
                .to_string(),
        );
    }
    let path = state.ctx.config_path.clone();
    let mut cfg = load_config(&path);
    let id = slugify(&name);
    let kind = if name.to_ascii_lowercase().contains("ttyd") {
        "ttyd"
    } else if url.contains(":4199") || name.to_ascii_lowercase().contains("headless") {
        "headless"
    } else {
        "web"
    };
    // Upsert by id.
    if let Some(existing) = cfg.nodes.iter_mut().find(|n| n.id == id) {
        existing.name = name;
        existing.url = url;
        existing.kind = kind.to_string();
    } else {
        cfg.nodes.push(crate::console_data::CloudNode {
            id,
            name,
            kind: kind.to_string(),
            url,
        });
    }
    if let Err(e) = save_config(&path, &cfg) {
        return err_json(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    (StatusCode::OK, Json(cfg)).into_response()
}

async fn api_cloud_delete(
    State(state): State<S>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> axum::response::Response {
    if let Err(resp) = guard(&state, &headers) {
        return resp;
    }
    let path = state.ctx.config_path.clone();
    let mut cfg = load_config(&path);
    cfg.nodes.retain(|n| n.id != id);
    if let Err(e) = save_config(&path, &cfg) {
        return err_json(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    (StatusCode::OK, Json(cfg)).into_response()
}

// ---------------------------------------------------------------------------
// Cloud reverse proxy (same-origin iframe + WebSocket relay)
// ---------------------------------------------------------------------------

/// Parse a node URL into (host, port). Only http(s) loopback is allowed.
fn target_addr(url: &str) -> Result<(String, u16), String> {
    let trimmed = url.trim();
    let rest = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"))
        .ok_or_else(|| "cloud node url must start with http://".to_string())?;
    if rest.starts_with("https://") {
        return Err("https cloud targets are not proxied (use plain http local ports)".to_string());
    }
    let hostport = rest.split('/').next().unwrap_or("");
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (
            h.to_string(),
            p.parse::<u16>().map_err(|_| format!("bad port in {url}"))?,
        ),
        None => (hostport.to_string(), 80),
    };
    Ok((host, port))
}

/// Strip the `/api/cloud/{id}` prefix from a request path; the remainder is
/// the target path (leading slash removed, root becomes empty).
fn tail_after(id: &str, path: &str) -> String {
    let marker = format!("/api/cloud/{id}");
    match path.strip_prefix(&marker) {
        Some(t) => t.trim_start_matches('/').to_string(),
        None => String::new(),
    }
}

async fn api_cloud_proxy_root(
    State(state): State<S>,
    headers: HeaderMap,
    Path(id): Path<String>,
    request: Request<Body>,
) -> axum::response::Response {
    api_cloud_http(state, headers, id, String::new(), request).await
}

async fn api_cloud_proxy_rest(
    State(state): State<S>,
    headers: HeaderMap,
    Path((id, _)): Path<(String, String)>,
    request: Request<Body>,
) -> axum::response::Response {
    let full = request.uri().path().to_string();
    let rest = tail_after(&id, &full);
    api_cloud_http(state, headers, id, rest, request).await
}

async fn api_cloud_http(
    state: S,
    headers: HeaderMap,
    id: String,
    rest: String,
    request: Request<Body>,
) -> axum::response::Response {
    if let Err(resp) = guard(&state, &headers) {
        return resp;
    }
    let cfg = load_config(&state.ctx.config_path);
    let node = match cfg.nodes.iter().find(|n| n.id == id) {
        Some(n) => n.clone(),
        None => return err_json(StatusCode::NOT_FOUND, format!("cloud node `{id}` not found")),
    };
    let (host, port) = match target_addr(&node.url) {
        Ok(a) => a,
        Err(e) => return err_json(StatusCode::BAD_REQUEST, e),
    };
    let method = request.method().clone();
    let uri = request.uri().clone();
    http_proxy(method, uri, &rest, host, port, request).await
}

/// WebSocket relay for terminal apps (ttyd) embedded under
/// `/api/cloud/{id}/ws`.
async fn api_cloud_ws(
    State(state): State<S>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
) -> axum::response::Response {
    if let Err(resp) = guard(&state, &headers) {
        return resp;
    }
    let cfg = load_config(&state.ctx.config_path);
    let node = match cfg.nodes.iter().find(|n| n.id == id) {
        Some(n) => n.clone(),
        None => return err_json(StatusCode::NOT_FOUND, format!("cloud node `{id}` not found")),
    };
    let (host, port) = match target_addr(&node.url) {
        Ok(a) => a,
        Err(e) => return err_json(StatusCode::BAD_REQUEST, e),
    };
    ws.on_upgrade(move |socket| ws_relay(socket, host, port, "/ws".to_string()))
        .into_response()
}

fn proxy_path(rest: &str, query: Option<&str>) -> String {
    let p = if rest.is_empty() { "/".to_string() } else { format!("/{rest}") };
    match query {
        Some(q) if !q.is_empty() => format!("{p}?{q}"),
        _ => p,
    }
}

async fn http_proxy(
    method: Method,
    uri: Uri,
    rest: &str,
    host: String,
    port: u16,
    request: Request<Body>,
) -> axum::response::Response {
    let path = proxy_path(rest, uri.query());
    let body = match body_to_bytes(request.into_body(), 8 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return err_json(StatusCode::BAD_REQUEST, "request body too large".to_string()),
    };
    let response_body = match tcp_http_roundtrip(&host, port, &method, &path, &body).await {
        Ok(b) => b,
        Err(e) => return err_json(StatusCode::BAD_GATEWAY, format!("proxy to {host}:{port}: {e}")),
    };
    split_response(response_body)
}

/// One HTTP/1.1 exchange against the loopback target with `Connection: close`.
/// Reads the full head, then the body according to Content-Length when present
/// (falling back to reading until EOF with a short grace period, since some
/// local servers ignore `Connection: close` and would otherwise hang forever).
async fn tcp_http_roundtrip(
    host: &str,
    port: u16,
    method: &Method,
    path: &str,
    body: &Bytes,
) -> Result<Vec<u8>, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let addr = format!("{host}:{port}");
    let mut tcp = tokio::net::TcpStream::connect(&addr)
        .await
        .map_err(|e| format!("connect {addr}: {e}"))?;
    let head = match method.as_str() {
        "POST" | "PUT" | "PATCH" => format!(
            "{} {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            method,
            body.len()
        ),
        _ => format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"),
    };
    tcp.write_all(head.as_bytes()).await.map_err(|e| format!("write: {e}"))?;
    if !body.is_empty() && method != Method::GET {
        tcp.write_all(body).await.map_err(|e| format!("write body: {e}"))?;
    }
    // Response head (up to \r\n\r\n).
    let mut raw = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let res = tokio::time::timeout(std::time::Duration::from_secs(8), tcp.read(&mut byte)).await;
        match res {
            Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
            Ok(Ok(_)) => {
                raw.push(byte[0]);
                if raw.len() >= 4 && &raw[raw.len() - 4..] == b"\r\n\r\n" {
                    break;
                }
                if raw.len() > 65_536 {
                    return Err("response head too large".to_string());
                }
            }
        }
    }
    let head_text = String::from_utf8_lossy(&raw);
    let content_len: Option<usize> = head_text
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            if k.trim().eq_ignore_ascii_case("content-length") {
                v.trim().parse().ok()
            } else {
                None
            }
        });
    if let Some(n) = content_len {
        let mut rest = vec![0u8; n];
        let mut read = 0;
        while read < n {
            let got = tcp.read(&mut rest[read..]).await.map_err(|e| format!("read body: {e}"))?;
            if got == 0 {
                break;
            }
            read += got;
        }
        rest.truncate(read);
        raw.extend_from_slice(&rest);
    } else {
        // No Content-Length: read until EOF with a short grace period.
        let mut rest = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(3), tcp.read(&mut buf)).await {
                Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
                Ok(Ok(n)) => rest.extend_from_slice(&buf[..n]),
            }
        }
        raw.extend_from_slice(&rest);
    }
    Ok(raw)
}

/// Parse a raw HTTP response into an axum Response with XFO/CSP removed.
fn split_response(raw: Vec<u8>) -> axum::response::Response {
    let text = String::from_utf8_lossy(&raw);
    let (head, body_bytes) = match text.find("\r\n\r\n") {
        Some(i) => (text[..i].to_string(), raw[i + 4..].to_vec()),
        None => (text.lines().next().unwrap_or("HTTP/1.1 502").to_string(), raw.clone()),
    };
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(502);
    let mut builder = Response::builder().status(status);
    for line in head.lines().skip(1) {
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim().to_string();
            // Hop-by-hop + iframe blockers are stripped.
            let strip = matches!(
                k.as_str(),
                "connection"
                    | "keep-alive"
                    | "transfer-encoding"
                    | "content-length"
                    | "upgrade"
                    | "x-frame-options"
                    | "content-security-policy"
                    | "frame-ancestors"
            );
            if !strip && !k.is_empty() {
                if let Ok(name) = header::HeaderName::from_bytes(k.as_bytes()) {
                    if let Ok(val) = header::HeaderValue::from_str(&v) {
                        builder = builder.header(name, val);
                    }
                }
            }
        }
    }
    builder
        .body(Body::from(body_bytes))
        .unwrap_or_else(|_| Response::new(Body::from("proxy error")))
}

/// Relay a browser WebSocket to the loopback target's WebSocket endpoint.
async fn ws_relay(socket: WebSocket, host: String, port: u16, path: String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    let connect = TcpStream::connect(format!("{host}:{port}")).await;
    let mut tcp = match connect {
        Ok(t) => t,
        Err(_) => return,
    };
    // Handshake against the target.
    let key = "teamsocconsole0000deadbeef==";
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
    );
    if tcp.write_all(req.as_bytes()).await.is_err() {
        return;
    }
    // Read until the end of the target's response head.
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if tcp.read(&mut byte).await.is_err() {
            return;
        }
        head.push(byte[0]);
        if head.len() >= 4 && &head[head.len() - 4..] == b"\r\n\r\n" {
            break;
        }
        if head.len() > 8192 {
            return;
        }
    }
    if !head.starts_with(b"HTTP/1.1 101") {
        return;
    }
    let (mut r, mut w) = tcp.into_split();
    let (mut sink, mut stream) = socket.split();

    // target TCP -> client WS
    let up_task = tokio::spawn(async move {
        let mut buf = [0u8; 8192];
        loop {
            let n = match r.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            if sink.send(WsMessage::Binary(buf[..n].to_vec().into())).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });
    // client WS -> target TCP
    while let Some(msg) = stream.next().await {
        match msg {
            Ok(WsMessage::Binary(b)) => {
                if w.write_all(&b).await.is_err() {
                    break;
                }
            }
            Ok(WsMessage::Text(t)) => {
                let s: &str = t.as_ref();
                if w.write_all(s.as_bytes()).await.is_err() {
                    break;
                }
            }
            Ok(WsMessage::Close(_)) | Err(_) => break,
            Ok(WsMessage::Ping(_)) | Ok(WsMessage::Pong(_)) => {}
        }
    }
    up_task.abort();
}
