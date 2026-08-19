//! teamx serve — network-mode server (mTLS, HTTP JSON RPC).
//!
//! Serves the same command surface as the V1 CLI over HTTP RPC so a plugin on
//! another machine can talk to one shared ledger. All command logic lives in
//! `commands::execute`; this module only translates an RPC request into a
//! `Command` value and serializes the result back.
//!
//! Security: the server REQUIRES mutual TLS. Clients must present a certificate
//! signed by the instance CA (`~/.teamx/ca/ca.crt`); the client certificate CN
//! carries the member identity (`member:<id>:<role>`). The RPC handler derives
//! the actor from that certificate — the self-reported `session` in the request
//! is ignored for authorization.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        ConnectInfo,
        State,
    },
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Extension, Json, Router,
};
use axum_server::accept::Accept;
use axum_server::tls_rustls::RustlsConfig;
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::server::TlsStream;
use tower::Layer;

use crate::broadcast::Hub;
use crate::cli::{Cli, Command, GoalCmd, LoopxCmd, MemberCmd, RoleCmd, ServeCmd, TeamCmd};
use crate::commands;
use crate::pki;

type Db = Mutex<rusqlite::Connection>;

#[derive(Clone)]
struct AppState {
    db: std::sync::Arc<Db>,
    hub: Hub,
    tunnels: crate::tunnel::TunnelRegistry,
}

#[derive(serde::Deserialize)]
struct RpcRequest {
    method: String,
    #[serde(default)]
    args: Value,
}

/// The authenticated client identity, extracted from the mTLS peer certificate
/// CN (`member:<id>:<role>` or empty if none was presented).
#[derive(Clone)]
struct PeerIdentity(String);

/// Extract the common name from a DER-encoded X.509 certificate.
fn extract_cn(der: &[u8]) -> Option<String> {
    use x509_parser::prelude::*;
    let (_, cert) = X509Certificate::from_der(der).ok()?;
    let cn = cert
        .subject()
        .iter_common_name()
        .next()
        .and_then(|a| a.as_str().ok())
        .map(str::to_string);
    cn
}

/// A helper so `CertAcceptor` can read the peer CN off any TLS stream.
trait PeerCerts {
    fn peer_identity_cn(&self) -> String;
}

impl<IO> PeerCerts for TlsStream<IO>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    fn peer_identity_cn(&self) -> String {
        let (_io, conn) = self.get_ref();
        conn.peer_certificates()
            .and_then(|certs| certs.first())
            .and_then(|der| extract_cn(der.as_ref()))
            .unwrap_or_default()
    }
}

/// Wraps the rustls acceptor so the verified client cert CN is injected as a
/// request extension that `Extension<PeerIdentity>` can extract.
#[derive(Clone)]
struct CertAcceptor {
    inner: axum_server::tls_rustls::RustlsAcceptor,
}

impl<S> Accept<TcpStream, S> for CertAcceptor
where
    S: Clone + Send + 'static,
{
    type Stream = TlsStream<TcpStream>;
    type Service = tower_http::add_extension::AddExtension<S, PeerIdentity>;
    type Future = Pin<Box<dyn Future<Output = std::io::Result<(Self::Stream, Self::Service)>> + Send>>;

    fn accept(&self, stream: TcpStream, service: S) -> Self::Future {
        let fut = self.inner.accept(stream, service);
        Box::pin(async move {
            let (stream, service) = fut.await?;
            let identity = PeerIdentity(stream.peer_identity_cn());
            let service = tower_http::add_extension::AddExtensionLayer::new(identity).layer(service);
            Ok((stream, service))
        })
    }
}

/// Entry point for `teamx serve`.
pub fn serve(cmd: &ServeCmd) -> Result<(), String> {
    let db_path = cmd.db.clone().unwrap_or_else(crate::db::default_db_path);
    let conn = crate::db::open(&db_path).map_err(|e| format!("cannot open database {db_path:?}: {e}"))?;
    crate::db::migrate(&conn).map_err(|e| format!("schema init failed: {e}"))?;

    let state = AppState {
        db: std::sync::Arc::new(Mutex::new(conn)),
        hub: Hub::new(),
        tunnels: crate::tunnel::TunnelRegistry::new(),
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/rpc", post(rpc))
        .route("/ws", get(ws_handler))
        .route("/tunnel", get(tunnel_ws_handler))
        .route("/tunnel/forward", get(tunnel_forward_handler))
        .with_state(state);

    // Bind address: support both IPv4 (`0.0.0.0:5781`) and IPv6 (`[::]:5781`).
    let bind_str = if cmd.addr.contains(':') {
        format!("[{}]:{}", cmd.addr, cmd.port)
    } else {
        format!("{}:{}", cmd.addr, cmd.port)
    };
    let addr: SocketAddr = bind_str
        .parse()
        .map_err(|e| format!("invalid bind address {}:{}: {e}", cmd.addr, cmd.port))?;

    // mTLS: build a rustls ServerConfig that requires a client certificate
    // signed by the instance CA, and present the server certificate chain.
    // The server cert is (re)generated to cover any extra SANs (e.g. LAN IP).
    let home = crate::db::teamx_home();
    let pk = pki::ensure_server_sans(&home, &cmd.san)?;
    let tls_config = build_mtls_config(&pk)?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;

    eprintln!(
        "teamx serve listening on https://{addr} (mtls, db: {})",
        db_path.display()
    );
    rt.block_on(async {
        let config = RustlsConfig::from_config(tls_config);
        let server = axum_server::bind_rustls(addr, config)
            .map(|acceptor| CertAcceptor { inner: acceptor });
        server
            .serve(app.into_make_service_with_connect_info::<SocketAddr>())
            .await
            .map_err(|e| format!("server error: {e}"))
    })
}

/// Build a rustls ServerConfig enforcing mutual TLS:
///  - server presents `server.crt`/`server.key`
///  - clients must present a cert verified against the instance CA roots
fn build_mtls_config(pk: &pki::PkiPaths) -> Result<Arc<rustls::ServerConfig>, String> {
    use rustls::server::WebPkiClientVerifier;
    use rustls::{RootCertStore, ServerConfig};

    // Client trust anchor: the instance CA (PEM → DER).
    let ca_pem = std::fs::read(&pk.ca_cert).map_err(|e| format!("read ca cert: {e}"))?;
    let mut roots = RootCertStore::empty();
    for cert in rustls_pemfile::certs(&mut ca_pem.as_slice())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("parse ca cert: {e}"))?
    {
        roots.add(cert).map_err(|e| format!("add ca to roots: {e}"))?;
    }

    let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
        .build()
        .map_err(|e| format!("client verifier: {e}"))?;

    // Server identity.
    let server_cert_pem = std::fs::read(&pk.server_cert).map_err(|e| format!("read server cert: {e}"))?;
    let server_key_pem = std::fs::read(&pk.server_key).map_err(|e| format!("read server key: {e}"))?;
    let cert_chain = rustls_pemfile::certs(&mut server_cert_pem.as_slice())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("parse server cert chain: {e}"))?;
    let key_der = rustls_pemfile::private_key(&mut server_key_pem.as_slice())
        .map_err(|e| format!("parse server key: {e}"))?
        .ok_or_else(|| "no private key found in server.key".to_string())?;

    let config = ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(cert_chain, key_der)
        .map_err(|e| format!("server config: {e}"))?;

    Ok(Arc::new(config))
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "ok": true,
        "service": "teamx",
        "version": env!("CARGO_PKG_VERSION"),
        "connections": state.hub.connection_count(),
    }))
}

/// WebSocket upgrade endpoint (network mode N1). The connection is
/// authenticated by the mTLS client certificate; the peer CN yields the member
/// identity, which is then subscribed to that member's teams.
async fn ws_handler(
    State(state): State<AppState>,
    Extension(identity): Extension<PeerIdentity>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state, identity))
}

/// Reverse-tunnel endpoint: a provider (member-b) opens a persistent WS here,
/// registers a local service, and the server relays consumer traffic over it.
async fn tunnel_ws_handler(
    State(state): State<AppState>,
    Extension(identity): Extension<PeerIdentity>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_tunnel_ws(socket, state, identity))
}

/// Serve one tunnel connection from a provider member.
async fn handle_tunnel_ws(mut socket: WebSocket, state: AppState, identity: PeerIdentity) {
    use futures_util::{SinkExt, StreamExt};

    let member_id = match pki::parse_member_cn(&identity.0) {
        Some((id, _role)) => id,
        None => {
            let _ = socket.send(ws_text(r#"{"type":"error","message":"no_identity"}"#)).await;
            return;
        }
    };

    // Resolve the member's teams (reuse the same check as /ws).
    let teams = {
        let db = state.db.clone();
        let mid = member_id.clone();
        match tokio::task::spawn_blocking(move || {
            let conn = db.lock().unwrap();
            commands::teams_for_member(&conn, &mid)
        })
        .await
        {
            Ok(Ok(v)) => v,
            _ => {
                let _ = socket.send(ws_text(r#"{"type":"error","message":"internal"}"#)).await;
                return;
            }
        }
    };
    if teams.len() != 1 {
        let _ = socket
            .send(ws_text(r#"{"type":"error","message":"tunnel requires membership in exactly one team"}"#))
            .await;
        return;
    }
    let team_id = teams[0].clone();

    let (mut sender, mut receiver) = socket.split();
    // Track which tunnel name this WS currently owns, so a disconnect frees it.
    let mut owned: Option<String> = None;
    let registry = state.tunnels.clone();

    // Outbound channel: relays (run_tcp_relay) push WS messages here for the
    // provider's socket. One channel per WS connection; every registered tunnel
    // shares it. We must keep the receiver alive and forward to the socket.
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<Message>();

    loop {
        tokio::select! {
            msg = receiver.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {                        let v: Value = match serde_json::from_str(text.as_str()) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        let ty = v.get("type").and_then(Value::as_str).unwrap_or("");
                        match ty {
                            "register" => {
                                let name = v.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                                let target = v.get("port").and_then(Value::as_u64).unwrap_or(0) as u16;
                                let lan_ip = v.get("lan_ip").and_then(Value::as_str).map(str::to_string);
                                // mode: "local" (default) or "frp" — see TunnelMode::parse.
                                let mode = crate::tunnel::TunnelMode::parse(v.get("mode").and_then(Value::as_str));
                                if name.is_empty() || target == 0 {
                                    let _ = sender.send(ws_text(r#"{"type":"error","message":"register requires name and port"}"#)).await;
                                    continue;
                                }
                                match registry.register(&team_id, &member_id, &name, target, lan_ip, out_tx.clone(), mode) {
                                    Ok(port) => {
                                        owned = Some(name.clone());
                                        if mode == crate::tunnel::TunnelMode::Frp {
                                            // Spawn the TCP relay for this tunnel (frp mode only).
                                            let reg = registry.clone();
                                            let tid = team_id.clone();
                                            let nm = name.clone();
                                            tokio::spawn(async move {
                                                let _ = crate::tunnel::run_tcp_relay(reg, tid, nm).await;
                                            });
                                        }
                                        let ack = json!({ "type": "registered", "name": name, "port": port, "mode": mode.as_str() });
                                        let _ = sender.send(ws_text(&ack.to_string())).await;
                                    }
                                    Err(e) => {
                                        let _ = sender.send(ws_text(&json!({ "type": "error", "message": e }).to_string())).await;
                                    }
                                }
                            }
                            "unregister" => {
                                if let Some(name) = v.get("name").and_then(Value::as_str) {
                                    registry.remove(&team_id, name);
                                    if owned.as_deref() == Some(name) {
                                        owned = None;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    Some(Ok(Message::Binary(buf))) => {
                        // Provider → consumer data frame. `owned` is the tunnel
                        // name this WS registered; route the bytes to the stream.
                        if let Some(name) = owned.as_deref() {
                            let name = name.to_string();
                            crate::tunnel::route_provider_data(&registry, &team_id, &name, buf.as_ref());
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    _ => {}
                }
            }
            // Relay → provider: forward open_stream / data / close frames to
            // the provider's WebSocket.
            msg = out_rx.recv() => {
                match msg {
                    Some(m) => {
                        if sender.send(m).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
        }
    }

    // Provider disconnected: free any tunnel it owned.
    if let Some(name) = owned.as_deref() {
        registry.remove(&team_id, name);
    }
}

/// Consumer-side local forward endpoint (T2). A consumer opens a mTLS WS here
/// and sends `{"type":"connect","name":"<tunnel>"}`. The server validates the
/// member belongs to the tunnel's team, opens a stream on that tunnel, and
/// bridges bytes between this WS and the provider's WS.
async fn tunnel_forward_handler(
    State(state): State<AppState>,
    Extension(identity): Extension<PeerIdentity>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_tunnel_forward(socket, state, identity))
}

/// Serve one consumer forward connection.
async fn handle_tunnel_forward(mut socket: WebSocket, state: AppState, identity: PeerIdentity) {
    use futures_util::{SinkExt, StreamExt};

    let member_id = match pki::parse_member_cn(&identity.0) {
        Some((id, _role)) => id,
        None => {
            let _ = socket.send(ws_text(r#"{"type":"error","message":"no_identity"}"#)).await;
            return;
        }
    };

    // First text frame must be `connect` with a tunnel name.
    let connect_msg = match socket.recv().await {
        Some(Ok(Message::Text(t))) => t,
        _ => {
            let _ = socket.send(ws_text(r#"{"type":"error","message":"connect requires a tunnel name"}"#)).await;
            return;
        }
    };
    let v: Value = match serde_json::from_str(connect_msg.as_str()) {
        Ok(v) => v,
        Err(_) => {
            let _ = socket.send(ws_text(r#"{"type":"error","message":"invalid connect frame"}"#)).await;
            return;
        }
    };
    let ty = v.get("type").and_then(Value::as_str).unwrap_or("");
    let name = v.get("name").and_then(Value::as_str).unwrap_or("").to_string();
    if ty != "connect" || name.is_empty() {
        let _ = socket.send(ws_text(r#"{"type":"error","message":"expected {\"type\":\"connect\",\"name\":\"<tunnel>\"}"}"#)).await;
        return;
    }

    // Resolve the tunnel and check membership.
    let (team_id, tunnel_name) = {
        let db = state.db.clone();
        let mid = member_id.clone();
        let nm = name.clone();
        let tunnels = state.tunnels.clone();
        match tokio::task::spawn_blocking(move || -> Result<(String, String), String> {
            let conn = db.lock().unwrap();
            let teams = commands::teams_for_member(&conn, &mid).map_err(|e| e.to_string())?;
            // Find the team that owns a tunnel with this name.
            for tid in teams {
                if tunnels.get(&tid, &nm).is_some() {
                    return Ok((tid, nm));
                }
            }
            Err(format!("tunnel `{nm}` not found in any of your teams"))
        })
        .await
        {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                let _ = socket.send(ws_text(&json!({ "type": "error", "message": e }).to_string())).await;
                return;
            }
            Err(_) => {
                let _ = socket.send(ws_text(r#"{"type":"error","message":"internal"}"#)).await;
                return;
            }
        }
    };

    // Open a stream on the tunnel; the provider dials its local target.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let sid = match state.tunnels.open_stream(&team_id, &tunnel_name, tx) {
        Some(sid) => sid,
        None => {
            let _ = socket.send(ws_text(r#"{"type":"error","message":"tunnel disappeared"}"#)).await;
            return;
        }
    };

    let (mut sender, mut receiver) = socket.split();
    // Ack: tell the consumer the stream is open.
    let _ = sender.send(ws_text(&json!({ "type": "stream_open", "stream_id": sid }).to_string())).await;

    // Bridge: consumer WS bytes → provider's tunnel WS; tunnel stream bytes
    // (relayed back by the provider) → consumer WS.
    // Note: consumer→provider bytes carry the server-assigned stream_id in the
    // frame header; forward them as-is to the provider's ws_tx (which expects
    // [stream_id][payload] binary frames). provider→consumer bytes come back
    // through the tunnel's streams table → rx → this WS.
    let registry = state.tunnels.clone();
    let tid = team_id.clone();
    let tnm = tunnel_name.clone();
    let consumer_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            match msg {
                Message::Binary(buf) => {
                    if let Some(t) = registry.get(&tid, &tnm) {
                        let _ = t.ws_tx.send(Message::Binary(buf));
                    }
                }
                Message::Text(_) => { /* control frames from consumer: ignore */ }
                Message::Close(_) | Message::Ping(_) | Message::Pong(_) => {}
            }
        }
        registry.close_stream(&tid, &tnm, sid);
    });

    let provider_task = tokio::spawn(async move {
        while let Some(bytes) = rx.recv().await {
            // Re-attach the stream id header: consumers expect [4B stream_id][payload].
            let mut frame = Vec::with_capacity(4 + bytes.len());
            frame.extend_from_slice(&(sid as u32).to_be_bytes());
            frame.extend_from_slice(&bytes);
            if sender.send(Message::Binary(frame.into())).await.is_err() {
                break;
            }
        }
    });

    let _ = consumer_task.await;
    let _ = provider_task.await;
}

/// Build a text WebSocket frame (axum 0.8 uses `Utf8Bytes` for text frames).
fn ws_text(s: &str) -> Message {
    Message::Text(s.into())
}

/// WS heartbeat interval in seconds (default 30; override for tests).
fn ws_heartbeat_secs() -> u64 {
    std::env::var("TEAMX_WS_HEARTBEAT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30)
}

/// Serve one live WebSocket connection: register for the member's teams, push
/// ledger events as they are written, and keep the connection alive with a
/// 30s heartbeat. Best-effort — the ledger stays the source of truth.
async fn handle_ws(mut socket: WebSocket, state: AppState, identity: PeerIdentity) {
    use futures_util::{SinkExt, StreamExt};

    let member_id = match pki::parse_member_cn(&identity.0) {
        Some((id, _role)) => id,
        None => {
            let _ = socket.send(ws_text(r#"{"type":"error","code":"no_identity"}"#)).await;
            return;
        }
    };

    let (teams, revoked) = {
        let db = state.db.clone();
        let mid = member_id.clone();
        match tokio::task::spawn_blocking(move || {
            let conn = db.lock().unwrap();
            let teams = commands::teams_for_member(&conn, &mid)?;
            let revoked = commands::is_revoked(&conn, &mid)?;
            Ok::<_, rusqlite::Error>((teams, revoked))
        })
        .await
        {
            Ok(Ok(v)) => v,
            _ => {
                let _ = socket.send(ws_text(r#"{"type":"error","code":"internal"}"#)).await;
                return;
            }
        }
    };

    if revoked {
        let _ = socket.send(ws_text(r#"{"type":"error","code":"revoked"}"#)).await;
        return;
    }
    if teams.is_empty() {
        let _ = socket.send(ws_text(r#"{"type":"error","code":"not_a_member"}"#)).await;
        return;
    }

    let mut rx = state.hub.subscribe(&member_id, &teams);
    let registered = json!({
        "type": "registered",
        "member_id": &member_id,
        "teams": &teams,
    })
    .to_string();
    let (mut sender, mut receiver) = socket.split();
    if sender.send(ws_text(&registered)).await.is_err() {
        state.hub.unsubscribe(&member_id, &teams);
        return;
    }

    let mut heartbeat = tokio::time::interval(Duration::from_secs(ws_heartbeat_secs()));
    heartbeat.tick().await; // consume the immediate first tick

    loop {
        tokio::select! {
            ev = rx.recv() => {
                match ev {
                    Some(frame) => {
                        // A `close` sentinel (e.g. invitation revoked) is
                        // forwarded to the client and the connection is dropped.
                        let is_close = frame.get("type").and_then(Value::as_str) == Some("close");
                        if sender.send(ws_text(&frame.to_string())).await.is_err() || is_close {
                            break;
                        }
                    }
                    None => break,
                }
            }
            msg = receiver.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(v) = serde_json::from_str::<Value>(text.as_str()) {
                            if v.get("type").and_then(Value::as_str) == Some("ping") {
                                let _ = sender.send(ws_text(r#"{"type":"pong"}"#)).await;
                            }
                            // `register`/`ack` are accepted but carry no authority:
                            // identity is fixed by the certificate.
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    _ => {}
                }
            }
            _ = heartbeat.tick() => {
                if sender.send(ws_text(r#"{"type":"ping"}"#)).await.is_err() {
                    break;
                }
            }
        }
    }

    state.hub.unsubscribe(&member_id, &teams);
}

async fn rpc(
    State(state): State<AppState>,
    Extension(identity): Extension<PeerIdentity>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<RpcRequest>,
) -> impl IntoResponse {
    let cn = identity.0;
    let hub = state.hub.clone();
    let tunnels = state.tunnels.clone();
    let method = req.method.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mut conn = state.db.lock().unwrap();
        let before = commands::max_event_id(&conn).unwrap_or(0);
        let res = dispatch(&req.method, &req.args, &mut conn, &cn, &tunnels, Some(peer));
        let new_events = commands::events_after(&conn, before).unwrap_or_default();
        (res, new_events)
    })
    .await;

    match result {
        Ok((Ok(data), new_events)) => {
            // Fan newly-written events out to live WS connections per team.
            for ev in &new_events {
                if let Some(team_id) = ev.get("team_id").and_then(Value::as_str) {
                    hub.publish(team_id, &json!({ "type": "event", "event": ev }));
                }
            }
            // On invitation revoke, actively drop the member's live WS (I2).
            if method == "team.invite_revoke" {
                if let Some(mid) = data.get("member_id").and_then(Value::as_str) {
                    hub.disconnect_member(mid);
                }
            }
            (StatusCode::OK, Json(json!({ "ok": true, "data": data })))
        }
        Ok((Err(e), _)) => (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": e }))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": format!("internal: {e}") })),
        ),
    }
}

/// Translate an RPC request into the same `Command` enum the CLI dispatches,
/// then run it through `commands::execute`. The actor identity comes from the
/// verified client certificate CN (`actor_cn`), not the self-reported `session`.
fn dispatch(method: &str, args: &Value, conn: &mut rusqlite::Connection, actor_cn: &str, tunnels: &crate::tunnel::TunnelRegistry, peer: Option<SocketAddr>) -> Result<Value, String> {
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
    let o = |k: &str| s(k);
    let b = |k: &str| args.get(k).and_then(Value::as_bool).unwrap_or(false);

    // Identity from the certificate: `member:<id>:<role>`.
    let member_id = pki::parse_member_cn(actor_cn)
        .map(|(id, _role)| id)
        .ok_or_else(|| format!("no member identity in client certificate CN `{actor_cn}`"))?;

    // Tunnel registry operations: the server's in-memory registry is the only
    // source of truth; these bypass the ledger (no events are written).
    // Authorization: the caller must be a member of the tunnel's team. If the
    // caller belongs to exactly one team, `team` may be omitted.
    match method {
        "tunnel.list" | "tunnel.status" | "tunnel.close" => {
            let tid = match s("team") {
                Some(t) => t,
                None => {
                    let teams = commands::teams_for_member(conn, &member_id).map_err(|e| e.to_string())?;
                    if teams.len() == 1 {
                        teams[0].clone()
                    } else {
                        return Err(format!(
                            "tunnel requires `team` (member belongs to {} teams)",
                            teams.len()
                        ));
                    }
                }
            };
            if !commands::member_in_team(conn, &member_id, &tid).map_err(|e| e.to_string())? {
                return Err(format!("member `{member_id}` is not a member of team {tid}"));
            }
            match method {
                "tunnel.list" => {
                    return Ok(serde_json::json!({ "tunnels": tunnels.list(&tid) }));
                }
                "tunnel.status" => {
                    let name = s("name")
                        .ok_or_else(|| "tunnel.status requires `name`".to_string())?;
                    let t = tunnels
                        .get(&tid, &name)
                        .ok_or_else(|| format!("tunnel `{name}` not found in team"))?;
                    let same_subnet = match (&peer, &t.lan_ip) {
                        (Some(peer_ip), Some(lan_ip)) => {
                            crate::tunnel::TunnelRegistry::same_subnet(peer_ip, lan_ip)
                        }
                        _ => false,
                    };
                    return Ok(serde_json::json!({
                        "name": t.name,
                        "port": t.port,
                        "mode": t.mode.as_str(),
                        "target_port": t.target_port,
                        "lan_ip": t.lan_ip,
                        "provider_member_id": t.provider_member_id,
                        "same_subnet": same_subnet,
                        "direct_addr": t.lan_ip.as_ref().map(|ip| format!("{ip}:{}", t.target_port)),
                        "relay_addr": format!("tcp://<server>:{}", t.port),
                    }));
                }
                "tunnel.close" => {
                    let name = s("name")
                        .ok_or_else(|| "tunnel.close requires `name`".to_string())?;
                    let freed = tunnels.remove(&tid, &name);
                    return Ok(serde_json::json!({ "ok": true, "closed": freed.is_some(), "freed_port": freed }));
                }
                _ => unreachable!(),
            }
        }
        // Enterprise analytics (A1): activity write + read RPCs. Authorization:
        //  - `activity.push`: a member may only write rows for their own member_id;
        //    node_id/node_name must be present (audit).
        //  - read RPCs (`summary`/`by_member`/`by_node`/`tools`/`files`/`rows`/
        //    `human_rows`): owner sees all rows for the team; a member sees only
        //    their own rows.
        "activity.push" => {
            let rows: Vec<crate::activity::ActivityRow> = args
                .get("rows")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|r| serde_json::from_value(r.clone()).ok()).collect())
                .unwrap_or_default();
            if rows.is_empty() {
                return Err("activity.push requires non-empty `rows`".to_string());
            }
            for r in &rows {
                if r.member_id != member_id {
                    return Err(format!(
                        "activity.push: member `{member_id}` may only write their own activity (got `{}`)",
                        r.member_id
                    ));
                }
                if r.node_id.is_empty() {
                    return Err("activity.push: `node_id` is required (audit)".to_string());
                }
                if r.team_id.is_empty() {
                    return Err("activity.push: `team_id` is required".to_string());
                }
                if !commands::member_in_team(conn, &member_id, &r.team_id).map_err(|e| e.to_string())? {
                    return Err(format!(
                        "activity.push: member `{member_id}` is not a member of team `{}`",
                        r.team_id
                    ));
                }
            }
            let n = crate::activity::push_activities(conn, &rows).map_err(|e| e.to_string())?;
            return Ok(serde_json::json!({ "ok": true, "inserted": n }));
        }
        m if m.starts_with("activity.") => {
            // Read RPCs. Resolve the target team, then enforce authorization.
            let tid = s("team").ok_or_else(|| "activity query requires `team`".to_string())?;
            if !commands::member_in_team(conn, &member_id, &tid).map_err(|e| e.to_string())? {
                return Err(format!("member `{member_id}` is not a member of team {tid}"));
            }
            let is_owner = commands::is_team_owner(conn, &member_id, &tid).map_err(|e| e.to_string())?;
            let member = s("member");
            // Owner: can pass any `member` filter. Member: forced to their own id.
            let member_filter = if is_owner {
                member
            } else {
                Some(member_id.clone())
            };
            let node = s("node");
            let kind = s("kind");
            let from = s("from");
            let to = s("to");
            let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(100);
            return match m {
                "activity.summary" => crate::activity::summary(conn, &tid, member_filter.as_deref(), node.as_deref(), kind.as_deref(), from.as_deref(), to.as_deref())
                    .map_err(|e| e.to_string()),
                "activity.by_member" => crate::activity::by_member(conn, &tid, member_filter.as_deref(), node.as_deref(), kind.as_deref(), from.as_deref(), to.as_deref())
                    .map_err(|e| e.to_string()),
                "activity.by_node" => crate::activity::by_node(conn, &tid, member_filter.as_deref(), node.as_deref(), kind.as_deref(), from.as_deref(), to.as_deref())
                    .map_err(|e| e.to_string()),
                "activity.tools" => crate::activity::tools(conn, &tid, member_filter.as_deref(), node.as_deref(), from.as_deref(), to.as_deref())
                    .map_err(|e| e.to_string()),
                "activity.files" => crate::activity::files(conn, &tid, member_filter.as_deref(), node.as_deref(), from.as_deref(), to.as_deref())
                    .map_err(|e| e.to_string()),
                "activity.rows" => crate::activity::rows(conn, &tid, member_filter.as_deref(), node.as_deref(), kind.as_deref(), from.as_deref(), to.as_deref(), limit)
                    .map_err(|e| e.to_string()),
                "activity.human_rows" => crate::activity::human_rows(conn, &tid, member_filter.as_deref(), node.as_deref(), from.as_deref(), to.as_deref(), limit)
                    .map_err(|e| e.to_string()),
                other => return Err(format!("unknown rpc method `{other}`")),
            };
        }
        _ => {}
    }

    // `team.import` registers a member whose seat is pre-allocated in the
    // invitation (the member row may not exist yet). It binds that seat to the
    // certificate-derived member id, so handle it before the generic dispatch.
    if method == "team.import" {
        let letter = s("letter").ok_or_else(|| "team.import requires `letter`".to_string())?;
        let name = o("name");
        return commands::import_with_cert(conn, &letter, name.as_deref(), &member_id)
            .map_err(|e| e.to_string());
    }

    // Reject revoked members (I2): a revoked invitation's cert still passes the
    // mTLS handshake, but it must not be able to act on the ledger.
    if commands::is_revoked(conn, &member_id).map_err(|e| e.to_string())? {
        return Err("member has been revoked".to_string());
    }

    // Enforce team leadership on cross-team reads (network mode): a member may
    // only read the status/roles/events/log of a team they belong to. Without
    // this, any member with a valid cert could read another team's state,
    // members, roles and invite_token.
    match method {
        "team.status" | "role.list" | "events" | "log" => {
            if let Some(tid) = s("team") {
                if !commands::member_in_team(conn, &member_id, &tid).map_err(|e| e.to_string())? {
                    return Err(format!("member `{member_id}` is not a member of team {tid}"));
                }
            }
        }
        _ => {}
    }

    // Every other command requires an existing member: resolve their session_key
    // by the certificate-derived member id (the self-reported `session` arg is
    // ignored for authorization).
    let session = commands::session_key_for_member(conn, &member_id).map_err(|e| e.to_string())?;
    let sess = |_args: &Value| session.clone();

    // Build the Cli with only the command populated; db/json are irrelevant
    // because `commands::execute` only reads `cli.command`.
    let make = |command: Command| Cli {
        db: None,
        json: true,
        command,
    };

    let cmd: Command = match method {
        "init" => Command::Init,

        "team.create" => Command::Team(TeamCmd::Create {
            name: s("name").ok_or_else(|| "team.create requires `name`".to_string())?,
            session: sess(args),
            goal_title: o("goal_title"),
            goal_body: o("goal_body"),
        }),
        "team.join" => Command::Team(TeamCmd::Join {
            token: s("token").ok_or_else(|| "team.join requires `token`".to_string())?,
            name: s("name").ok_or_else(|| "team.join requires `name`".to_string())?,
            session: sess(args),
            loopx_project: None,
        }),
        "team.approve" => Command::Team(TeamCmd::Approve {
            member_id: s("member_id").ok_or_else(|| "team.approve requires `member_id`".to_string())?,
            session: sess(args),
            team: o("team"),
        }),
        "team.deny" => Command::Team(TeamCmd::Deny {
            member_id: s("member_id").ok_or_else(|| "team.deny requires `member_id`".to_string())?,
            session: sess(args),
            team: o("team"),
        }),
        "team.list" => Command::Team(TeamCmd::List { session: sess(args) }),
        "team.status" => Command::Team(TeamCmd::Status { team: o("team"), session: Some(sess(args)) }),
        "team.leave" => Command::Team(TeamCmd::Leave { session: sess(args), team: o("team") }),
        "team.archive" => Command::Team(TeamCmd::Archive { session: sess(args), team: o("team") }),
        "team.destroy" => Command::Team(TeamCmd::Destroy { session: sess(args), team: o("team") }),
        "team.invite" => Command::Team(TeamCmd::Invite {
            role_desc: s("role_desc").ok_or_else(|| "team.invite requires `role_desc`".to_string())?,
            name_hint: o("name_hint"),
            server_url: o("server_url"),
            session: sess(args),
            team: o("team"),
        }),
        "team.invite_list" => Command::Team(TeamCmd::InviteList { session: sess(args), team: o("team") }),
        "team.invite_revoke" => Command::Team(TeamCmd::InviteRevoke {
            id: s("id").ok_or_else(|| "team.invite_revoke requires `id`".to_string())?,
            session: sess(args),
            team: o("team"),
        }),

        "goal.set" => Command::Goal(GoalCmd::Set {
            title: s("title").ok_or_else(|| "goal.set requires `title`".to_string())?,
            body: o("body"),
            session: sess(args),
            team: o("team"),
        }),
        "goal.share" => Command::Goal(GoalCmd::Share { session: sess(args), team: o("team") }),
        "goal.close" => Command::Goal(GoalCmd::Close { session: sess(args), team: o("team") }),

        "member.set_state" => Command::Member(MemberCmd::SetState {
            state: s("state").ok_or_else(|| "member.set_state requires `state`".to_string())?,
            member: o("member"),
            session: sess(args),
            team: o("team"),
        }),

        "role.list" => Command::Role(RoleCmd::List { team: o("team") }),
        "role.set" => Command::Role(RoleCmd::Set {
            role: s("role").ok_or_else(|| "role.set requires `role`".to_string())?,
            session: sess(args),
            member: o("member"),
            team: o("team"),
        }),
        "role.propose" => Command::Role(RoleCmd::Propose {
            role: s("role").ok_or_else(|| "role.propose requires `role`".to_string())?,
            label: s("label").ok_or_else(|| "role.propose requires `label`".to_string())?,
            description: o("description"),
            session: sess(args),
            team: o("team"),
        }),
        "role.approve" => Command::Role(RoleCmd::Approve {
            role: s("role").ok_or_else(|| "role.approve requires `role`".to_string())?,
            session: sess(args),
            team: o("team"),
        }),
        "role.deny" => Command::Role(RoleCmd::Deny {
            role: s("role").ok_or_else(|| "role.deny requires `role`".to_string())?,
            session: sess(args),
            team: o("team"),
        }),
        "role.update" => Command::Role(RoleCmd::Update {
            role: s("role").ok_or_else(|| "role.update requires `role`".to_string())?,
            label: o("label"),
            description: o("description"),
            session: sess(args),
            team: o("team"),
        }),

        "publish" => Command::Publish {
            r#type: s("type").ok_or_else(|| "publish requires `type`".to_string())?,
            data: o("data"),
            assignee: o("assignee"),
            session: sess(args),
            team: o("team"),
        },
        "ask" => Command::Ask {
            member_id: s("member_id").ok_or_else(|| "ask requires `member_id`".to_string())?,
            question: s("question").ok_or_else(|| "ask requires `question`".to_string())?,
            session: sess(args),
            team: o("team"),
        },
        "respond" => Command::Respond {
            ask_id: s("ask_id").ok_or_else(|| "respond requires `ask_id`".to_string())?,
            answer: s("answer").ok_or_else(|| "respond requires `answer`".to_string())?,
            session: sess(args),
        },
        "events" => Command::Events {
            after: args.get("after").and_then(Value::as_i64),
            team: o("team"),
        },
        "log" => Command::Log {
            team: o("team"),
            session: Some(sess(args)),
            limit: args.get("limit").and_then(Value::as_i64),
            after: args.get("after").and_then(Value::as_i64),
        },
        "sync" => Command::Sync { session: sess(args), no_advance: b("no_advance") },
        "loopx.report" => Command::Loopx(LoopxCmd::Report {
            project: s("project").ok_or_else(|| "loopx.report requires `project`".to_string())?.into(),
            session: sess(args),
            team: o("team"),
        }),

        other => return Err(format!("unknown rpc method `{other}`")),
    };

    let cli = make(cmd);
    commands::execute(&cli, conn).map_err(|e| e.to_string())
}
