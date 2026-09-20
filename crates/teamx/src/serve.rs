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
    body::{Body, Bytes},
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        ConnectInfo,
        DefaultBodyLimit,
        Path,
        State,
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
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
use crate::cli::{Cli, Command, DiscussionCmd, GoalCmd, LoopxCmd, MemberCmd, NodeCmd, RoleCmd, ServeCmd, TeamCmd, UserCmd};
use crate::commands;
use crate::pki;

use rusqlite::params;
use rusqlite::OptionalExtension;

type Db = Mutex<rusqlite::Connection>;

#[derive(Clone)]
struct AppState {
    db: std::sync::Arc<Db>,
    hub: Hub,
    tunnels: crate::tunnel::TunnelRegistry,
    metrics: crate::metrics::SharedMetrics,
}

#[derive(serde::Deserialize, serde::Serialize)]
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

/// Env var overriding the `/rpc` request body limit (bytes).
pub const RPC_MAX_BODY_ENV: &str = "TEAMX_MAX_BODY";

/// Default `/rpc` request body limit: 512 MiB — the same cap the smart-HTTP
/// git path uses (`read_body`), so `git.receive` (base64 bundle inside JSON)
/// can carry the same payloads as `git-receive-pack`. axum's built-in default
/// (2 MiB) rejected any bundle over ~1.5 MiB with 413 (Issue #12).
pub const RPC_MAX_BODY_DEFAULT: usize = 512 * 1024 * 1024;

/// Parse a body-limit override: must be a positive integer; anything else
/// (unset/invalid/0) falls back to the default.
pub fn parse_rpc_max_body(raw: Option<&str>) -> usize {
    raw.and_then(|s| s.trim().parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(RPC_MAX_BODY_DEFAULT)
}

/// Effective `/rpc` body limit: `TEAMX_MAX_BODY` (bytes) or the 512 MiB default.
pub fn rpc_max_body() -> usize {
    parse_rpc_max_body(std::env::var(RPC_MAX_BODY_ENV).ok().as_deref())
}

/// Entry point for `teamx serve`.
pub fn serve(cmd: &ServeCmd) -> Result<(), String> {    // When launched by the opencode plugin, stop as soon as opencode exits.
    crate::procwatch::spawn_parent_monitor();

    let db_path = cmd.db.clone().unwrap_or_else(crate::db::default_db_path);
    let conn = crate::db::open(&db_path).map_err(|e| format!("cannot open database {db_path:?}: {e}"))?;
    crate::db::migrate(&conn).map_err(|e| format!("schema init failed: {e}"))?;

    let state = AppState {
        db: std::sync::Arc::new(Mutex::new(conn)),
        hub: Hub::new(),
        tunnels: crate::tunnel::TunnelRegistry::new(),
        metrics: std::sync::Arc::new(crate::metrics::MetricsRegistry::new()),
    };

    let app = Router::new()
        .route("/health", get(health))
        // Large `git.receive` bundles travel base64-encoded inside the JSON
        // body — axum's 2 MiB default would 413 them (Issue #12). The limit
        // is scoped to /rpc only; smart-HTTP git routes keep their own cap.
        .route("/rpc", post(rpc).layer(DefaultBodyLimit::max(rpc_max_body())))
        .route("/ws", get(ws_handler))
        .route("/tunnel", get(tunnel_ws_handler))
        .route("/tunnel/forward", get(tunnel_forward_handler))
        // Git routes
        .route("/git/repos", get(git_list_repos).post(git_create_repo))
        .route("/git/repos/{repo}", get(git_get_repo).delete(git_delete_repo))
        .route("/git/repos/{repo}/permissions", get(git_list_permissions).post(git_grant_permission))
        .route("/git/repos/{repo}/clone", post(git_clone_repo))
        .route("/git/repos/{repo}/pull", post(git_pull_repo))
        .route("/git/repos/{repo}/push", post(git_push_repo))
        // Git Smart HTTP (standard git protocol over mTLS). URL shape:
        //   https://server/git/<team_id>/<repo>
        .route("/git/{team_id}/{repo}/info/refs", get(git_http_info_refs))
        .route("/git/{team_id}/{repo}/git-upload-pack", post(git_http_upload_pack))
        .route("/git/{team_id}/{repo}/git-receive-pack", post(git_http_receive_pack))
        // Clone for the background nudge task before `state` moves into the router.
        .with_state(state.clone());

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
        // Start the idle-team nudge task: periodically write a `system.nudge`
        // ledger event for teams that have gone silent with an unfinished goal,
        // and fan it out to live WS connections. Offline members get it on the
        // next `sync` (it is a ledger event). Configurable via env:
        //   TEAMX_NUDGE_ENABLED      (default 1)
        //   TEAMX_NUDGE_INTERVAL_SECS (default 60)  how often we check
        //   TEAMX_NUDGE_AFTER_SECS    (default 300) silent-before-nudge
        let nudge_state = state.clone();
        tokio::spawn(async move { crate::serve::nudge_task(nudge_state).await });

        // Start the IM outbound notifier (docs/57): watches the ledger and
        // pushes whitelisted events to the configured IM groups. Disabled
        // unless TEAMX_IM_NOTIFY_GROUPS names at least one group; dry-run
        // (TEAMX_IM_DRY_RUN, default on) logs instead of sending.
        if let Ok(notify_state) = im_notifier_task(state.clone(), &crate::im::ImConfig::from_env()) {
            tokio::spawn(notify_state);
        }

        let config = RustlsConfig::from_config(tls_config);
        let server = axum_server::bind_rustls(addr, config)
            .map(|acceptor| CertAcceptor { inner: acceptor });
        server
            .serve(app.into_make_service_with_connect_info::<SocketAddr>())
            .await
            .map_err(|e| format!("server error: {e}"))
    })
}

/// Background task: periodically nudge teams whose goal is unfinished but which
/// have had no ledger activity for `TEAMX_NUDGE_AFTER_SECS` (default 300s).
/// The nudge is a `system.nudge` ledger event + live WS fan-out.
async fn nudge_task(state: AppState) {
    let enabled = std::env::var("TEAMX_NUDGE_ENABLED")
        .map(|s| s != "0" && s != "false")
        .unwrap_or(true);
    if !enabled {
        return;
    }
    let interval_secs = std::env::var("TEAMX_NUDGE_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60u64);
    let after_secs = std::env::var("TEAMX_NUDGE_AFTER_SECS")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(300);

    let mut tick = tokio::time::interval(Duration::from_secs(interval_secs));
    tick.tick().await; // consume the immediate first tick
    loop {
        tick.tick().await;
        let db = state.db.clone();
        let hub = state.hub.clone();
        // DB access is blocking (rusqlite); do it off the async runtime. The
        // nudge pass covers both team-level silence and per-member open tasks.
        let events = tokio::task::spawn_blocking(move || {
            let mut conn = db.lock().unwrap();
            let mut out = commands::nudge_idle_teams(&mut conn, after_secs, None)?;
            out.extend(commands::nudge_open_tasks(&mut conn, after_secs, None, None)?);
            out.extend(commands::nudge_unclaimed_tasks(&mut conn, after_secs, None)?);
            Ok::<_, rusqlite::Error>(out)
        })
        .await;
        let Ok(Ok(events)) = events else { eprintln!("teamx serve: nudge pass failed"); continue };
        if events.is_empty() {
            continue;
        }
        eprintln!("teamx serve: nudged {} item(s)", events.len());
        for ev in &events {
            if let Some(team_id) = ev.get("team_id").and_then(Value::as_str) {
                hub.publish(team_id, &json!({ "type": "event", "event": ev }));
            }
        }
    }
}

/// IM outbound notifier loop (docs/57). Returns the spawn-ready future, or
/// `Err(())` when the notifier is disabled (no `TEAMX_IM_NOTIFY_GROUPS`).
/// Every pass: collect whitelisted ledger events into `im_outbox`, then
/// flush due rows through the provider (direct rows immediately, digest
/// rows after the 60 s window). IM platform failures never abort the loop.
fn im_notifier_task(
    state: AppState,
    cfg: &crate::im::ImConfig,
) -> Result<impl std::future::Future<Output = ()>, ()> {
    use std::sync::Arc as StdArc;
    if cfg.notify_groups.is_empty() {
        return Err(());
    }
    let provider: StdArc<dyn crate::im::providers::ImProvider + Send + Sync> =
        StdArc::new(crate::im::FeishuProvider::new(cfg.clone()));
    let digest_window_secs = std::env::var("TEAMX_IM_DIGEST_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(crate::im::notifier::DIGEST_WINDOW_SECS);
    let notifier = StdArc::new(crate::im::Notifier {
        groups: cfg.notify_groups.clone(),
        provider,
        digest_window_secs,
    });
    let interval_secs = std::env::var("TEAMX_IM_NOTIFY_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(15u64);
    Ok(async move {
        eprintln!(
            "teamx serve: im notifier started ({} group(s), interval {interval_secs}s)",
            notifier.groups.len()
        );
        let mut tick = tokio::time::interval(Duration::from_secs(interval_secs));
        tick.tick().await; // consume the immediate first tick
        loop {
            tick.tick().await;
            let db = state.db.clone();
            let n = notifier.clone();
            let stats = tokio::task::spawn_blocking(move || {
                let mut conn = db.lock().unwrap();
                n.run_once(&mut conn, chrono::Utc::now().timestamp())
            })
            .await
            .unwrap_or_default();
            if stats.collected + stats.pushed + stats.failed > 0 {
                eprintln!(
                    "teamx im notifier: collected {} pushed {} failed {} deferred {}",
                    stats.collected, stats.pushed, stats.failed, stats.deferred
                );
            }
        }
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
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state, identity, Some(peer)))
}

/// Reverse-tunnel endpoint: a provider (member-b) opens a persistent WS here,
/// registers a local service, and the server relays consumer traffic over it.
async fn tunnel_ws_handler(
    State(state): State<AppState>,
    Extension(identity): Extension<PeerIdentity>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_tunnel_ws(socket, state, identity, Some(peer)))
}

/// Serve one tunnel connection from a provider member.
async fn handle_tunnel_ws(mut socket: WebSocket, state: AppState, identity: PeerIdentity, peer: Option<SocketAddr>) {
    use futures_util::{SinkExt, StreamExt};

    let (member_id, provider_user_id) = match pki::parse_member_identity(&identity.0) {
        Some((id, _role, user)) => (id, user),
        None => {
            let _ = socket.send(ws_text(r#"{"type":"error","message":"no_identity"}"#)).await;
            return;
        }
    };

    // Resolve the member's teams (reuse the same check as /ws). A revoked
    // invitation still passes the mTLS handshake, so the tunnel data plane
    // must reject it here too — same rule as /ws.
    let teams = {
        let db = state.db.clone();
        let mid = member_id.clone();
        match tokio::task::spawn_blocking(move || -> Result<Vec<String>, String> {
            let conn = db.lock().unwrap();
            if commands::is_revoked(&conn, &mid).map_err(|e| e.to_string())? {
                return Err("revoked".to_string());
            }
            commands::teams_for_member(&conn, &mid).map_err(|e| e.to_string())
        })
        .await
        {
            Ok(Ok(v)) => v,
            Ok(Err(e)) if e == "revoked" => {
                let _ = socket.send(ws_text(r#"{"type":"error","code":"revoked"}"#)).await;
                return;
            }
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

    // Audit: record the tunnel connection (provider side).
    if let Some(ip) = peer.map(|p| p.ip().to_string()) {
        let db = state.db.clone();
        let mid = member_id.clone();
        let tid = team_id.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db.lock().unwrap();
            let _ = crate::db::log_connection(&conn, &mid, &tid, &ip, "tunnel");
        });
    }

    let (mut sender, mut receiver) = socket.split();
    // Track every tunnel this WS registered so a disconnect frees them all
    // (registering a second tunnel must not leak the first one).
    let mut owned: std::collections::HashSet<String> = std::collections::HashSet::new();
    let registry = state.tunnels.clone();

    // Outbound channel: relays (run_tcp_relay) push WS messages here for the
    // provider's socket. One channel per WS connection; every registered tunnel
    // shares it. We must keep the receiver alive and forward to the socket.
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<Message>();

    // Keep the tunnel control plane alive across NAT/proxies: same 30s
    // heartbeat as the /ws channel. An idle tunnel WS that neither side probes
    // can be silently dropped by middleboxes, leaving a stale registered
    // tunnel and half-open connections (proxy flows then fail with SOCKS5 (5)).
    let mut heartbeat = tokio::time::interval(Duration::from_secs(ws_heartbeat_secs()));
    heartbeat.tick().await; // consume the immediate first tick

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
                            "ping" => {
                                // Application-level ping: reply so the client
                                // can detect a dead peer without relying on
                                // transport-level timeouts.
                                let _ = sender.send(ws_text(r#"{"type":"pong"}"#)).await;
                            }
                            "register" => {
                                let name = v.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                                let target = v.get("port").and_then(Value::as_u64).unwrap_or(0) as u16;
                                let lan_ip = v.get("lan_ip").and_then(Value::as_str).map(str::to_string);
                                // mode: "local" (default), "frp" or "proxy" — see TunnelMode::parse.
                                let mode = crate::tunnel::TunnelMode::parse(v.get("mode").and_then(Value::as_str));
                                // Proxy exits have no fixed target port; Local/Frp require one.
                                let port_ok = match mode {
                                    crate::tunnel::TunnelMode::Proxy => true,
                                    _ => target != 0,
                                };
                                if name.is_empty() || !port_ok {
                                    let _ = sender.send(ws_text(r#"{"type":"error","message":"register requires name and port"}"#)).await;
                                    continue;
                                }
                                match registry.register(&team_id, &member_id, &name, target, lan_ip.clone(), out_tx.clone(), mode, provider_user_id.clone()) {
                                    Ok(port) => {
                                        owned.insert(name.clone());
                                        // Mirror the registration into the DB so
                                        // DB-reading tools (admin-console /api/topology)
                                        // can draw the live reverse-tunnel edge.
                                        {
                                            let db = state.db.clone();
                                            let tid = team_id.clone();
                                            let mid = member_id.clone();
                                            let nm = name.clone();
                                            let md = mode.as_str().to_string();
                                            let lip = lan_ip.clone();
                                            let pp = port as i64;
                                            let tp = if mode == crate::tunnel::TunnelMode::Proxy { None } else { Some(target as i64) };
                                            tokio::task::spawn_blocking(move || {
                                                let conn = db.lock().unwrap();
                                                let _ = crate::db::tunnel_upsert(&conn, &tid, &nm, &mid, &md, pp, tp, lip.as_deref());
                                            });
                                        }
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
                                    owned.remove(name);
                                    let db = state.db.clone();
                                    let tid = team_id.clone();
                                    let nm = name.to_string();
                                    tokio::task::spawn_blocking(move || {
                                        let conn = db.lock().unwrap();
                                        let _ = crate::db::tunnel_remove(&conn, &tid, &nm);
                                    });
                                }
                            }
                            "resolve_result" => {
                                let sid = v.get("stream_id").and_then(Value::as_u64).unwrap_or(0);
                                let ips = v.get("ips")
                                    .and_then(Value::as_array)
                                    .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                                    .unwrap_or_default();
                                registry.complete_resolve(sid, ips);
                            }
                            _ => {}
                        }
                    }
                    Some(Ok(Message::Binary(buf)))
                        // Provider → consumer data frame ([4B stream_id][payload]).
                        // A provider may register several tunnels over one WS and
                        // stream ids come from one server-wide counter, so exactly
                        // one owned tunnel holds each id.
                        if !owned.is_empty() => {
                            crate::tunnel::route_provider_data_owned(
                                &registry,
                                &team_id,
                                &owned,
                                buf.as_ref(),
                            );
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
            _ = heartbeat.tick() => {
                if sender.send(ws_text(r#"{"type":"ping"}"#)).await.is_err() {
                    break;
                }
            }
        }
    }

    // Provider disconnected: free every tunnel it registered on this WS.
    for name in &owned {
        registry.remove(&team_id, name);
        let db = state.db.clone();
        let tid = team_id.clone();
        let nm = name.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db.lock().unwrap();
            let _ = crate::db::tunnel_remove(&conn, &tid, &nm);
        });
    }
    // Audit: mark the tunnel connection closed.
    {
        let db = state.db.clone();
        let mid = member_id;
        tokio::task::spawn_blocking(move || {
            let conn = db.lock().unwrap();
            let _ = crate::db::close_connection(&conn, &mid, "tunnel");
        });
    }
}

/// Consumer-side local forward endpoint (T2). A consumer opens a mTLS WS here
/// and sends `{"type":"connect","name":"<tunnel>"}`. The server validates the
/// member belongs to the tunnel's team, opens a stream on that tunnel, and
/// bridges bytes between this WS and the provider's WS.
async fn tunnel_forward_handler(
    State(state): State<AppState>,
    Extension(identity): Extension<PeerIdentity>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_tunnel_forward(socket, state, identity, Some(peer)))
}

/// Serve one consumer forward connection.
async fn handle_tunnel_forward(mut socket: WebSocket, state: AppState, identity: PeerIdentity, _peer: Option<SocketAddr>) {
    use futures_util::{SinkExt, StreamExt};

    let (member_id, consumer_user) = match pki::parse_member_identity(&identity.0) {
        Some((id, _role, user)) => (id, user),
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
    // Optional SOCKS5 target (host:port) for Proxy-mode exits.
    let target = v.get("target").and_then(Value::as_str).map(str::to_string);
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
        let cu = consumer_user.clone();
        match tokio::task::spawn_blocking(move || -> Result<(String, String), String> {
            let conn = db.lock().unwrap();
            if commands::is_revoked(&conn, &mid).map_err(|e| e.to_string())? {
                return Err("revoked".to_string());
            }
            let teams = commands::teams_for_member(&conn, &mid).map_err(|e| e.to_string())?;
            // Find the team that owns a tunnel with this name, then enforce
            // user-scoped access: a user-bound tunnel is reachable only by the
            // same user's devices or by a team lead; legacy (unbound) tunnels
            // stay team-scoped.
            for tid in teams {
                if let Some(t) = tunnels.get(&tid, &nm) {
                    let owner_ok = commands::is_team_owner(&conn, &mid, &tid);
                    let allowed = match &t.provider_user_id {
                        None => true,
                        Some(pu) => {
                            cu.as_deref() == Some(pu.as_str()) || owner_ok.unwrap_or(false)
                        }
                    };
                    if !allowed {
                        return Err(format!("tunnel `{nm}` belongs to another user"));
                    }
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

    // Open a stream on the tunnel; the provider dials its local target (or
    // the SOCKS5 target for Proxy-mode exits).
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let sid = match state.tunnels.open_stream(&team_id, &tunnel_name, tx, target.as_deref()) {
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

    // Keep this consumer forward channel alive across NAT/proxies: a 30s
    // heartbeat mirrors the provider /tunnel channel so an idle `proxy start`
    // / `tunnel forward` connection is not silently dropped by middleboxes
    // (which would strand the local SOCKS5 listener).
    let mut heartbeat = tokio::time::interval(Duration::from_secs(ws_heartbeat_secs()));
    heartbeat.tick().await; // consume the immediate first tick

    loop {
        tokio::select! {
            msg = receiver.next() => {
                match msg {
                    Some(Ok(Message::Binary(buf))) => {
                        if let Some(t) = registry.get(&tid, &tnm) {
                            let _ = t.ws_tx.send(Message::Binary(buf));
                        }
                    }
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(v) = serde_json::from_str::<Value>(text.as_str()) {
                            if v.get("type").and_then(Value::as_str) == Some("ping") {
                                let _ = sender.send(ws_text(r#"{"type":"pong"}"#)).await;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    _ => {}
                }
            }
            bytes = rx.recv() => {
                match bytes {
                    Some(bytes) => {
                        // Re-attach the stream id header: consumers expect [4B stream_id][payload].
                        let mut frame = Vec::with_capacity(4 + bytes.len());
                        frame.extend_from_slice(&(sid as u32).to_be_bytes());
                        frame.extend_from_slice(&bytes);
                        if sender.send(Message::Binary(frame.into())).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
            _ = heartbeat.tick() => {
                if sender.send(ws_text(r#"{"type":"ping"}"#)).await.is_err() {
                    break;
                }
            }
        }
    }
    registry.close_stream(&tid, &tnm, sid);
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
async fn handle_ws(mut socket: WebSocket, state: AppState, identity: PeerIdentity, peer: Option<SocketAddr>) {
    use futures_util::{SinkExt, StreamExt};

    let member_id = match pki::parse_member_cn(&identity.0) {
        Some((id, _role)) => id,
        None => {
            let _ = socket.send(ws_text(r#"{"type":"error","message":"no_identity"}"#)).await;
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

    let (mut rx, sub) = state.hub.subscribe(&member_id, &teams);
    // Audit: record the connection (peer IP + endpoint).
    if let Some(ip) = peer.map(|p| p.ip().to_string()) {
        if let Some(tid) = teams.first() {
            let db = state.db.clone();
            let mid = member_id.clone();
            let tid = tid.clone();
            tokio::task::spawn_blocking(move || {
                let conn = db.lock().unwrap();
                let _ = crate::db::log_connection(&conn, &mid, &tid, &ip, "ws");
            });
        }
    }
    let registered = json!({
        "type": "registered",
        "member_id": &member_id,
        "teams": &teams,
    })
    .to_string();
    let (mut sender, mut receiver) = socket.split();
    if sender.send(ws_text(&registered)).await.is_err() {
        state.hub.unsubscribe(&sub);
        return;
    }

    let mut heartbeat = tokio::time::interval(Duration::from_secs(ws_heartbeat_secs()));
    heartbeat.tick().await; // consume the immediate first tick
    // RTT measurement: timestamp of the last app-level ping we sent.
    let mut last_ping: Option<std::time::Instant> = None;
    let metrics = state.metrics.clone();
    let rtt_member = member_id.clone();

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
                            match v.get("type").and_then(Value::as_str) {
                                Some("ping") => {
                                    let _ = sender.send(ws_text(r#"{"type":"pong"}"#)).await;
                                }
                                Some("pong") => {
                                    // RTT = now - when we sent the ping.
                                    if let Some(t) = last_ping.take() {
                                        let ms = t.elapsed().as_secs_f64() * 1000.0;
                                        metrics.record_rtt(&rtt_member, ms);
                                    }
                                }
                                _ => {}
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
                last_ping = Some(std::time::Instant::now());
                if sender.send(ws_text(r#"{"type":"ping"}"#)).await.is_err() {
                    break;
                }
            }
        }
    }

    state.hub.unsubscribe(&sub);
    // Audit: mark the connection closed.
    {
        let db = state.db.clone();
        let mid = member_id;
        tokio::task::spawn_blocking(move || {
            let conn = db.lock().unwrap();
            let _ = crate::db::close_connection(&conn, &mid, "ws");
        });
    }
}

async fn rpc(
    State(state): State<AppState>,
    Extension(identity): Extension<PeerIdentity>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<RpcRequest>,
) -> impl IntoResponse {
    let cn = identity.0;
    let hub = state.hub.clone();
    let tunnels = state.tunnels.clone();
    let method = req.method.clone();
    // Host header as dialed by the caller — used to embed a reachable server
    // URL in newly issued letters (e.g. clone).
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    // Metrics: count the request bytes (member -> server).
    let member_for_metrics = pki::parse_member_cn(&cn).map(|(id, _)| id);
    if let Some(mid) = &member_for_metrics {
        let body_len = serde_json::to_vec(&req).map(|b| b.len() as u64).unwrap_or(64);
        state.metrics.record_rx(mid, body_len);
    }
    let metrics_for_resp = state.metrics.clone();

    // Live member network metrics (not a ledger command).
    if method == "team.metrics" {
        let snap = metrics_for_resp.snapshot_all();
        return (StatusCode::OK, Json(json!({ "ok": true, "data": { "metrics": snap } })));
    }

    // `events.rebroadcast` — a local CLI that wrote the ledger directly asks
    // the server to fan those events out to live WS connections (N1). The
    // server publishes only rows that verifiably exist in its own ledger and
    // only for teams the caller is an active member of: the broadcast stays a
    // strict subset of the ledger.
    if method == "events.rebroadcast" {
        let Some((caller, _)) = pki::parse_member_cn(&cn) else {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "ok": false, "error": format!("no member identity in client certificate CN `{cn}`") })),
            );
        };
        let events = req.args.get("events").and_then(Value::as_array).cloned().unwrap_or_default();
        if events.is_empty() {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "ok": false, "error": "events.rebroadcast requires a non-empty `events` array" })),
            );
        }
        if events.len() > 64 {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "ok": false, "error": "events.rebroadcast accepts at most 64 events" })),
            );
        }
        let db = state.db.clone();
        let verified = tokio::task::spawn_blocking(move || {
            let conn = db.lock().unwrap();
            commands::verified_rebroadcast_events(&conn, &caller, &events)
        })
        .await;
        return match verified {
            Ok(rows) => {
                let published = rows.len();
                for (team_id, ev) in &rows {
                    hub.publish(team_id, &json!({ "type": "event", "event": ev }));
                }
                (StatusCode::OK, Json(json!({ "ok": true, "data": { "published": published } })))
            }
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "ok": false, "error": format!("internal: {e}") })),
            ),
        };
    }

    // DNS resolution via a proxy exit (uncensored resolver). Async: forwards a
    // `resolve` frame to the named exit and waits for its `resolve_result`.
    if method == "team.resolve_dns" {
        let name = req.args.get("name").and_then(Value::as_str).unwrap_or("").to_string();
        let exit = req.args.get("exit").and_then(Value::as_str).unwrap_or("").to_string();
        if name.is_empty() || exit.is_empty() {
            return (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": "resolve_dns requires name and exit" })));
        }
        let mid = member_for_metrics.clone();
        let tunnels = tunnels.clone();
        let team_ids: Vec<String> = {
            let db = state.db.clone();
            let mid = mid.clone().unwrap_or_default();
            tokio::task::spawn_blocking(move || {
                let conn = db.lock().unwrap();
                commands::teams_for_member(&conn, &mid).unwrap_or_default()
            })
            .await
            .unwrap_or_default()
        };
        for tid in team_ids {
            if let Some((sid, rx)) = tunnels.resolve(&tid, &exit, &name) {
                match tokio::time::timeout(Duration::from_secs(6), rx).await {
                    Ok(Ok(ips)) => {
                        return (StatusCode::OK, Json(json!({ "ok": true, "data": { "ips": ips } })));
                    }
                    // Exit never answered: drop the waiter so it does not leak.
                    Ok(Err(_)) | Err(_) => {
                        tunnels.complete_resolve(sid, Vec::new());
                        break;
                    }
                }
            }
        }
        return (StatusCode::OK, Json(json!({ "ok": true, "data": { "ips": [] } })));
    }

    let result = tokio::task::spawn_blocking(move || {
        let mut conn = state.db.lock().unwrap();
        let before = commands::max_event_id(&conn).unwrap_or(0);
        let res = dispatch(&req.method, &req.args, &mut conn, &cn, &tunnels, Some(peer), host.as_deref());
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
            // Metrics: count response bytes (server -> member).
            if let Some(mid) = &member_for_metrics {
                let resp_len = serde_json::to_vec(&data).map(|b| b.len() as u64).unwrap_or(64);
                metrics_for_resp.record_tx(mid, resp_len);
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

/// Parse an RPC array argument from either `rows` (a JSON array) or `data`
/// (a JSON array serialized as a string — the plugin's `--data` flag path).
fn parse_rows<T: serde::de::DeserializeOwned>(args: &Value) -> Vec<T> {
    if let Some(arr) = args.get("rows").and_then(Value::as_array) {
        return arr.iter().filter_map(|r| serde_json::from_value(r.clone()).ok()).collect();
    }
    if let Some(s) = args.get("data").and_then(Value::as_str) {
        if let Ok(v) = serde_json::from_str::<Vec<T>>(s) {
            return v;
        }
    }
    Vec::new()
}

/// Translate an RPC request into the same `Command` enum the CLI dispatches,
/// then run it through `commands::execute`. The actor identity comes from the
/// Methods an IM-gateway **service** identity may invoke (closed list,
/// fail-closed). The gateway is zero-privilege: anything not listed is refused.
/// Extending this list is a reviewed code change, not a runtime toggle.
const SERVICE_METHOD_WHITELIST: &[&str] = &[
    "im.query",                                        // P1 read-only
    "im.approve", "im.deny", "im.verify", "im.reject", // P2 write actions (gated by gateway B)
    "im.card.request", "im.card.execute",              // P2 interactive cards (double confirmation)
    // P2 binding: the one pre-binding action. The user is not yet a member, so
    // `dispatch_service` skips member resolution for it (see below). The
    // authorization is the one-time binding code (created by an authenticated
    // member, single-use, TTL'd, rate-limited) and the resulting binding always
    // carries the lowest role — no privilege elevation.
    "im.bind",
    // IM chat audit (方案 1 sidecar): the gateway logs each free-text turn.
    // Audit-only; it never mutates team state.
    "im.chat.log",
];

/// Whether `method` is callable by a service identity.
fn service_method_allowed(method: &str) -> bool {
    SERVICE_METHOD_WHITELIST.contains(&method)
}

/// Dispatch a call made by a **service** identity (currently the IM gateway).
///
/// Security rules (DQ-0004):
/// - fail-closed method whitelist;
/// - the acting member is resolved from the delegation
///   (`platform`/`im_user_id`/`team` → `im_bindings`), **never** from a
///   caller-supplied `member_id` (a compromised gateway must not impersonate);
/// - the resolved member is re-dispatched with a synthesized member CN so the
///   downstream role/permission checks are unchanged (no privilege elevation).
#[allow(clippy::too_many_arguments)] // delegation envelope fields are all required
fn dispatch_service(
    service: &str,
    instance: &str,
    method: &str,
    args: &Value,
    conn: &mut rusqlite::Connection,
    tunnels: &crate::tunnel::TunnelRegistry,
    peer: Option<SocketAddr>,
    host: Option<&str>,
) -> Result<Value, String> {
    if service != "im-gateway" {
        return Err(format!("unknown service identity `service:{service}:{instance}`"));
    }
    if !service_method_allowed(method) {
        return Err(format!("service `{service}` may not call `{method}` (method whitelist)"));
    }
    // Never trust a caller-supplied member id.
    if args
        .get("member_id")
        .and_then(Value::as_str)
        .map(|s| !s.is_empty())
        .unwrap_or(false)
    {
        return Err("service call must not carry `member_id` (delegated identity required)".to_string());
    }
    let platform = args
        .get("platform")
        .and_then(Value::as_str)
        .ok_or_else(|| "service call requires `platform`".to_string())?;
    let im_user_id = args
        .get("im_user_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "service call requires `im_user_id`".to_string())?;
    let team = args
        .get("team")
        .and_then(Value::as_str)
        .ok_or_else(|| "service call requires `team`".to_string())?;

    // Delegation verification (design §5.4): HMAC signature over the
    // delegated fields + timestamp freshness ±5min + one-time nonce replay
    // protection + per-(im_user, team) rate limit — every failure audited as
    // `im.rejected` (gateway.rs pipeline, PR #51). Presence checks alone
    // would let any service-cert holder impersonate a bound member (see
    // .teamx/weiye-mini/reviews/pr-57.md finding ②).
    let ctx = crate::im::types::ImDelegatedContext {
        platform: platform.to_string(),
        im_user_id: im_user_id.to_string(),
        im_team: args.get("im_team").and_then(Value::as_str).map(str::to_string),
        signature: args.get("signature").and_then(Value::as_str).map(str::to_string),
        timestamp: args.get("timestamp").and_then(Value::as_i64).unwrap_or(0),
        nonce: args.get("nonce").and_then(Value::as_str).unwrap_or_default().to_string(),
    };
    let cfg = crate::im::config::ImConfig::from_env();
    // `im.bind` is the only action that runs BEFORE a binding exists: the IM
    // user is not a member yet, so there is no member to resolve. The
    // one-time code (issued by an authenticated member, single-use, TTL'd)
    // is the authorization; the new binding gets the lowest role. Every other
    // whitelisted method must resolve its actor from `im_bindings`.
    let is_bind = method == "im.bind";
    crate::im::gateway::accept_with(conn, &cfg, team, &ctx, !is_bind)?;

    if is_bind {
        return crate::im::rpc(method, args, conn, "", team);
    }

    let resolved = match crate::im::binding::resolve_member(conn, platform, im_user_id, team)? {
        Some(v) => v,
        None => {
            let payload = serde_json::json!({
                "platform": platform,
                "im_user_id": im_user_id,
                "im_msg_id": args.get("im_msg_id").and_then(Value::as_str),
                "reason_code": "unbound",
                "source": format!("im:{platform}"),
            });
            let _ = crate::db::with_write(conn, |tx| {
                crate::events::emit(tx, team, None, "im.rejected", Some(&payload))?;
                Ok(())
            });
            return Err("im.rejected: unbound".to_string());
        }
    };
    let member_id = resolved
        .get("member_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if member_id.is_empty() {
        return Err("im.rejected: unbound (binding resolved without member id)".to_string());
    }
    // Role comes from the binding resolution (DB-backed), so the gateway cannot
    // elevate privileges.
    let role = resolved
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("member")
        .to_string();
    let cn = format!("member:{member_id}:{role}");
    dispatch(method, args, conn, &cn, tunnels, peer, host)
}

/// verified client certificate CN (`actor_cn`), not the self-reported `session`.
/// 吊销判定（design/006 §2「吊销 member 证书」）：pki 无 CRL，改按 **seat 状态** 判定。
///
/// `left` / `denied` → 已吊销；座位不存在（尚未认领）→ 视同吊销（只放行 team.import）。
pub fn member_revoked_for_rpc(state: Option<&str>) -> bool {
    match state {
        Some("left") | Some("denied") => true,
        Some(_) => false,
        None => true,
    }
}

#[cfg(test)]
mod revoke_tests {
    use super::member_revoked_for_rpc;

    #[test]
    fn revoked_seats_are_rejected_but_active_ones_are_not() {
        assert!(member_revoked_for_rpc(Some("left")));
        assert!(member_revoked_for_rpc(Some("denied")));
        assert!(member_revoked_for_rpc(None), "unknown seat: only team.import allowed");
        for ok in ["pending", "active", "idle", "waiting"] {
            assert!(!member_revoked_for_rpc(Some(ok)), "{ok} must not be treated as revoked");
        }
    }
}

fn dispatch(method: &str, args: &Value, conn: &mut rusqlite::Connection, actor_cn: &str, tunnels: &crate::tunnel::TunnelRegistry, peer: Option<SocketAddr>, host: Option<&str>) -> Result<Value, String> {
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
    let o = |k: &str| s(k);
    let b = |k: &str| args.get(k).and_then(Value::as_bool).unwrap_or(false);

    // Identity from the certificate: `member:<id>:<role>`, or a SERVICE
    // identity `service:im-gateway:<instance>` which may act **only** for a
    // delegated IM binding and only on whitelisted methods (DQ-0004).
    let member_id = match pki::parse_member_cn(actor_cn) {
        Some((id, _role)) => id,
        None => {
            if let Some((service, instance)) = pki::parse_service_cn(actor_cn) {
                return dispatch_service(&service, &instance, method, args, conn, tunnels, peer, host);
            }
            return Err(format!("no member identity in client certificate CN `{actor_cn}`"));
        }
    };

    // 吊销校验（design/006 §2：「吊销 member 证书」）。节点销毁后该 seat 被置为
    // left，其证书在 TLS 层仍有效（pki 无 CRL），因此在**派发前**按 seat 状态拦截：
    // 只允许尚未认领的座位走 team.import（认领本身需要它），其余一律拒绝。
    // 未知 member（尚未认领）同理只放行 team.import。
    {
        let state: Option<String> = conn
            .query_row(
                "SELECT state FROM members WHERE id = ?1",
                rusqlite::params![member_id],
                |r| r.get(0),
            )
            .ok();
        if member_revoked_for_rpc(state.as_deref()) && method != "team.import" {
            return Err(format!(
                "member {member_id} is revoked (seat state: {}); certificate no longer accepted",
                state.as_deref().unwrap_or("unknown")
            ));
        }
    }

    // 成员工作区目录上报：客户端（插件 / CLI）随任意 RPC 附带 `workspace`，
    // 落库到 members.workspace，供 `team status` / console 展示「每个成员在哪工作」。
    // 只在值变化时写，避免每个 RPC 都产生一次无谓 UPDATE。
    if let Some(ws) = s("workspace").map(|w| w.trim().to_string()).filter(|w| !w.is_empty() && w.len() <= 512) {
        let _ = conn.execute(
            "UPDATE members SET workspace = ?1 WHERE id = ?2 AND COALESCE(workspace, '') != ?1",
            rusqlite::params![ws, member_id],
        );
    }

    // taskx (server-backed tasks for network teams). The task's team is the
    // caller's single team unless `team` is given.
    if method.starts_with("task.") {
        let tid = match s("team") {
            Some(t) => t,
            None => {
                let teams = commands::teams_for_member(conn, &member_id).map_err(|e| e.to_string())?;
                match teams.len() {
                    1 => teams[0].clone(),
                    0 => return Err("task: member has no active team".to_string()),
                    _ => {
                        return Err(format!(
                            "task requires `team` (member belongs to {} teams)",
                            teams.len()
                        ))
                    }
                }
            }
        };
        if !commands::member_in_team(conn, &member_id, &tid).map_err(|e| e.to_string())? {
            return Err(format!("member `{member_id}` is not a member of team {tid}"));
        }
        return crate::server_tasks::rpc(method, args, conn, &tid, &member_id);
    }

    // direct.* — member-to-member direct connectivity (candidate registry +
    // probe). Team = the caller's single team unless `team` is given; the
    // server records the observed peer address as the reflexive candidate.
    if method.starts_with("direct.") {
        let tid = match s("team") {
            Some(t) => t,
            None => {
                let teams = commands::teams_for_member(conn, &member_id).map_err(|e| e.to_string())?;
                match teams.len() {
                    1 => teams[0].clone(),
                    0 => return Err("direct: member has no active team".to_string()),
                    _ => {
                        return Err(format!(
                            "direct requires `team` (member belongs to {} teams)",
                            teams.len()
                        ))
                    }
                }
            }
        };
        if !commands::member_in_team(conn, &member_id, &tid).map_err(|e| e.to_string())? {
            return Err(format!("member `{member_id}` is not a member of team {tid}"));
        }
        let reflexive = peer.map(|p| (p.ip().to_string(), p.port()));
        return crate::direct::rpc(method, args, conn, &tid, &member_id, reflexive);
    }

    // IM access (P1): identity binding. The team is the caller's single team
    // unless `team` is given; the certificate identity stays the only source of
    // the actor, so a member can never act on another team's bindings.
    if method.starts_with("im.") {
        let tid = match s("team") {
            Some(t) => t,
            None => {
                let teams = commands::teams_for_member(conn, &member_id).map_err(|e| e.to_string())?;
                match teams.len() {
                    1 => teams[0].clone(),
                    0 => return Err("im: member has no active team".to_string()),
                    _ => {
                        return Err(format!(
                            "im requires `team` (member belongs to {} teams)",
                            teams.len()
                        ))
                    }
                }
            }
        };
        if !commands::member_in_team(conn, &member_id, &tid).map_err(|e| e.to_string())? {
            return Err(format!("member `{member_id}` is not a member of team {tid}"));
        }
        return crate::im::rpc(method, args, conn, &member_id, &tid);
    }

    // `team.force_leave` — owner/co-lead retires a stale member seat (e.g. an
    // old non-user-bound worker) by marking it `left`.
    if method == "team.force_leave" {
        let tid = match s("team") {
            Some(t) => t,
            None => {
                let teams = commands::teams_for_member(conn, &member_id).map_err(|e| e.to_string())?;
                match teams.len() {
                    1 => teams[0].clone(),
                    0 => return Err("team.force_leave: member has no active team".to_string()),
                    _ => return Err("team.force_leave requires `team` (member in several teams)".to_string()),
                }
            }
        };
        let lead: bool = conn
            .query_row(
                "SELECT t.owner_member_id = ?1 OR m.is_lead = 1
                 FROM teams t JOIN members m ON m.team_id = t.id
                 WHERE t.id = ?2 AND m.id = ?1",
                params![&member_id, &tid],
                |r| r.get::<_, bool>(0),
            )
            .optional()
            .map_err(|e| e.to_string())?
            .unwrap_or(false);
        if !lead {
            return Err("team.force_leave: only the team owner or a co-lead may retire members".to_string());
        }
        let target = s("member_id").ok_or_else(|| "team.force_leave requires `member_id`".to_string())?;
        let display: Option<String> = conn
            .query_row(
                "SELECT display_name FROM members WHERE team_id = ?1 AND id = ?2",
                params![&tid, target],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if display.is_none() {
            return Err(format!("member `{target}` is not in team {tid}"));
        }
        let now = crate::db::now();
        crate::db::with_write(conn, |tx| {
            tx.execute(
                "UPDATE members SET state = 'left', left_at = ?1 WHERE team_id = ?2 AND id = ?3",
                params![now, &tid, target],
            )?;
            crate::events::emit(
                tx,
                &tid,
                Some(&member_id),
                "member.left",
                Some(&json!({
                    "display_name": display,
                    "by": "team.force_leave",
                    "member_id": target,
                })),
            )?;
            Ok(())
        })
        .map_err(|e: rusqlite::Error| format!("db: {e}"))?;
        return Ok(json!({ "ok": true, "member_id": target, "team": tid, "state": "left" }));
    }

    // `team.clone` — clone a team (owner/co-lead only). Uses CN-based
    // authorization (not session), so it dispatches directly to cmd_team_clone
    // with the actor's member_id.
    if method == "team.clone" {
        let src_team_id = match s("team") {
            Some(t) => t,
            None => {
                let teams = commands::teams_for_member(conn, &member_id).map_err(|e| e.to_string())?;
                match teams.len() {
                    1 => teams[0].clone(),
                    0 => return Err("team.clone: member has no active team".to_string()),
                    _ => return Err("team.clone requires `team` (member belongs to multiple teams)".to_string()),
                }
            }
        };
        if !commands::member_in_team(conn, &member_id, &src_team_id).map_err(|e| e.to_string())? {
            return Err(format!("member `{member_id}` is not a member of team {src_team_id}"));
        }
        let name = s("name").ok_or_else(|| "team.clone requires `name`".to_string())?;
        let yes = b("yes");
        // Option A: derive the public server URL from the request Host header
        // (the address the caller actually dialed), so newly issued clone
        // letters are reachable by remote members — not `127.0.0.1`.
        let server_url = commands::resolve_server_url_with_host(o("server_url").as_deref(), host)
            .map_err(|e| e.to_string())?;
        // Call cmd_team_clone directly with CN-based member_id
        let result = commands::cmd_team_clone(
            conn,
            &name,
            Some(&src_team_id),
            Some(&server_url),
            yes,
            "",
            Some(&member_id),
        ).map_err(|e| e.to_string())?;
        return Ok(result);
    }

    // `team.inbox` — unified per-member inbox (tasks + my questions + unread
    // nudges). Member's nudge cursor lives server-side (`inbox_cursors`).
    if method == "team.inbox" {
        let tid = match s("team") {
            Some(t) => t,
            None => {
                let teams = commands::teams_for_member(conn, &member_id).map_err(|e| e.to_string())?;
                match teams.len() {
                    1 => teams[0].clone(),
                    0 => return Err("team.inbox: member has no active team".to_string()),
                    _ => return Err("team.inbox requires `team` (member in several teams)".to_string()),
                }
            }
        };
        if !commands::member_in_team(conn, &member_id, &tid).map_err(|e| e.to_string())? {
            return Err(format!("member `{member_id}` is not a member of team {tid}"));
        }
        return crate::server_tasks::inbox(conn, &tid, &member_id, b("ack"));
    }

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
                        "provider_user_id": t.provider_user_id,
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
            let rows: Vec<crate::activity::ActivityRow> = parse_rows(args);
            if rows.is_empty() {
                return Err("activity.push requires non-empty `rows`".to_string());
            }
            // The certificate identity is authoritative for member_id: whatever
            // the client claims, the row is attributed to the cert CN member.
            let teams = commands::teams_for_member(conn, &member_id).map_err(|e| e.to_string())?;
            if teams.is_empty() {
                return Err(format!("activity.push: member `{member_id}` has no active team"));
            }
            let mut rows = rows;
            for r in &mut rows {
                r.member_id = member_id.clone();
                if r.team_id.is_empty() {
                    // Only auto-fill when the member belongs to exactly one team.
                    if teams.len() != 1 {
                        return Err(format!(
                            "activity.push: `team_id` required (member belongs to {} teams)",
                            teams.len()
                        ));
                    }
                    r.team_id = teams[0].clone();
                }
                if r.node_id.is_empty() {
                    return Err("activity.push: `node_id` is required (audit)".to_string());
                }
                if !commands::member_in_team(conn, &member_id, &r.team_id).map_err(|e| e.to_string())? {
                    return Err(format!(
                        "activity.push: member `{member_id}` is not a member of team `{}`",
                        r.team_id
                    ));
                }
            }
            // Resolve cost: opencode cost stays authoritative; missing cost is
            // computed from tokens × pricing when the model is known.
            crate::activity::apply_pricing(conn, &mut rows).map_err(|e| e.to_string())?;
            // Budget alert: snapshot the cumulative cost per (team, member)
            // before the batch lands, so a threshold crossing can be detected
            // after insertion. Disabled unless TEAMX_BUDGET_ALERT_COST_USD > 0.
            let budget = crate::activity::budget_threshold_from_env();
            let mut before_sums: std::collections::HashMap<(String, String), f64> =
                std::collections::HashMap::new();
            if budget.is_some() {
                for r in &rows {
                    let key = (r.team_id.clone(), r.member_id.clone());
                    if let std::collections::hash_map::Entry::Vacant(e) = before_sums.entry(key) {
                        let sum = crate::activity::member_cost_sum(conn, &r.team_id, &r.member_id)
                            .map_err(|e| e.to_string())?;
                        e.insert(sum);
                    }
                }
            }
            let n = crate::activity::push_activities(conn, &rows).map_err(|e| e.to_string())?;
            if let Some(threshold) = budget {
                for ((tid, mid), before) in &before_sums {
                    let after =
                        crate::activity::member_cost_sum(conn, tid, mid).map_err(|e| e.to_string())?;
                    crate::activity::budget_alert_on_crossing(conn, tid, mid, *before, after, threshold)
                        .map_err(|e| e.to_string())?;
                }
            }
            return Ok(serde_json::json!({ "ok": true, "inserted": n }));
        }
        // Enterprise cost: plugin reports the opencode model pricing it observes
        // (`source = 'opencode'`). Any member may sync; owner rows are protected
        // from being overwritten (priority: owner > opencode > builtin).
        "model.pricing_sync" => {
            let prices: Vec<crate::activity::ModelPrice> = parse_rows(args);
            if prices.is_empty() {
                return Err("model.pricing_sync requires non-empty `rows`".to_string());
            }
            let n = crate::activity::upsert_prices(conn, &prices, "opencode").map_err(|e| e.to_string())?;
            return Ok(serde_json::json!({ "ok": true, "upserted": n }));
        }
        // Enterprise presence: the plugin pushes a live snapshot (active
        // sessions / running tools / open terminals). Latest-only, keyed by the
        // cert-derived member id.
        "presence.push" => {
            let p: crate::activity::Presence = args
                .get("data")
                .and_then(Value::as_str)
                .and_then(|s| serde_json::from_str(s).ok())
                .or_else(|| serde_json::from_value(args.clone()).ok())
                .unwrap_or_default();
            crate::activity::upsert_presence(conn, &member_id, &p).map_err(|e| e.to_string())?;
            return Ok(serde_json::json!({ "ok": true }));
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
                "activity.summary" => crate::activity::summary(conn, Some(&tid), member_filter.as_deref(), node.as_deref(), kind.as_deref(), from.as_deref(), to.as_deref())
                    .map_err(|e| e.to_string()),
                "activity.by_member" => crate::activity::by_member(conn, Some(&tid), member_filter.as_deref(), node.as_deref(), kind.as_deref(), from.as_deref(), to.as_deref())
                    .map_err(|e| e.to_string()),
                "activity.by_node" => crate::activity::by_node(conn, Some(&tid), member_filter.as_deref(), node.as_deref(), kind.as_deref(), from.as_deref(), to.as_deref())
                    .map_err(|e| e.to_string()),
                "activity.tools" => crate::activity::tools(conn, Some(&tid), member_filter.as_deref(), node.as_deref(), from.as_deref(), to.as_deref())
                    .map_err(|e| e.to_string()),
                "activity.files" => crate::activity::files(conn, Some(&tid), member_filter.as_deref(), node.as_deref(), from.as_deref(), to.as_deref())
                    .map_err(|e| e.to_string()),
                "activity.rows" => crate::activity::rows(conn, Some(&tid), member_filter.as_deref(), node.as_deref(), kind.as_deref(), from.as_deref(), to.as_deref(), limit)
                    .map_err(|e| e.to_string()),
                "activity.human_rows" => crate::activity::human_rows(conn, Some(&tid), member_filter.as_deref(), node.as_deref(), from.as_deref(), to.as_deref(), limit)
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

    // Git repository operations (network mode). Repos are scoped per team; the
    // caller must be a member of the repo's team. Authz:
    //   - list/bundle       -> read  permission on the repo
    //   - receive           -> write permission
    //   - create            -> team owner only
    //   - delete/grant      -> admin permission on the repo
    if method.starts_with("git.") {
        // Plugin sends `git list`; the server RPC calls this `git.repos`.
        let method = if method == "git.list" { "git.repos" } else { method };
        return git_dispatch(method, args, conn, &member_id);
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
            user_name: o("user_name"),
            user: o("user"),
            auto_approve: b("auto_approve"),
            as_lead: b("as_lead"),
            provision_node: b("provision_node"),
            provider: o("provider"),
            region: o("region"),
            size: o("size"),
            ttl: args.get("ttl").and_then(Value::as_u64).map(|n| n as u32),
            os_user: o("os_user"),
        }),
        // Cloud-host provisioning (design/006).
        "node.provision" => Command::Node(NodeCmd::Provision {
            member: s("member").ok_or_else(|| "node.provision requires `member`".to_string())?,
            provider: o("provider"),
            region: o("region"),
            size: o("size"),
            ttl: args.get("ttl").and_then(Value::as_u64).map(|n| n as u32),
            os_user: o("os_user"),
            session: sess(args),
            team: o("team"),
        }),
        "node.list" => Command::Node(NodeCmd::List { session: sess(args), team: o("team") }),
        "node.status" => Command::Node(NodeCmd::Status {
            node_id: s("node_id").ok_or_else(|| "node.status requires `node_id`".to_string())?,
            session: sess(args),
            team: o("team"),
        }),
        "node.ssh" => Command::Node(NodeCmd::Ssh {
            node_id: s("node_id").ok_or_else(|| "node.ssh requires `node_id`".to_string())?,
            session: sess(args),
            team: o("team"),
        }),
        "node.stop" => Command::Node(NodeCmd::Stop {
            node_id: s("node_id").ok_or_else(|| "node.stop requires `node_id`".to_string())?,
            session: sess(args),
            team: o("team"),
        }),
        "node.start" => Command::Node(NodeCmd::Start {
            node_id: s("node_id").ok_or_else(|| "node.start requires `node_id`".to_string())?,
            session: sess(args),
            team: o("team"),
        }),
        "node.destroy" => Command::Node(NodeCmd::Destroy {
            node_id: s("node_id").ok_or_else(|| "node.destroy requires `node_id`".to_string())?,
            session: sess(args),
            team: o("team"),
        }),
        "team.invite_list" => Command::Team(TeamCmd::InviteList { session: sess(args), team: o("team") }),
        "team.invite_revoke" => Command::Team(TeamCmd::InviteRevoke {
            id: s("id").ok_or_else(|| "team.invite_revoke requires `id`".to_string())?,
            session: sess(args),
            team: o("team"),
        }),

        "user.list" => Command::User(UserCmd::List { session: sess(args), team: o("team") }),

        "goal.set" => Command::Goal(GoalCmd::Set {
            title: s("title").ok_or_else(|| "goal.set requires `title`".to_string())?,
            body: o("body"),
            session: sess(args),
            team: o("team"),
        }),
        "goal.share" => Command::Goal(GoalCmd::Share { session: sess(args), team: o("team") }),
        "goal.close" => Command::Goal(GoalCmd::Close { session: sess(args), team: o("team") }),

        "member.rename" => Command::Member(MemberCmd::Rename {
            name: s("name").ok_or_else(|| "member.rename requires `name`".to_string())?,
            member: o("member"),
            session: sess(args),
            team: o("team"),
        }),
        "member.set_user" => Command::Member(MemberCmd::SetUser {
            user_name: o("user_name"),
            user: o("user"),
            member: s("member").ok_or_else(|| "member.set_user requires `member`".to_string())?,
            session: sess(args),
            team: o("team"),
        }),
        "member.set_state" => Command::Member(MemberCmd::SetState {
            state: s("state").ok_or_else(|| "member.set_state requires `state`".to_string())?,
            member: o("member"),
            session: sess(args),
            team: o("team"),
        }),

        "member.discussion_quota" => Command::Member(MemberCmd::DiscussionQuota {
            action: s("action").ok_or_else(|| "member.discussion_quota requires `action` (get|set)".to_string())?,
            // Accept both JSON numbers and numeric strings (CLI flags arrive
            // as strings through cliArgsToRpc).
            value: args.get("value").and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|x| x.trim().parse::<i64>().ok()))
            }),
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

        "discussion.create" => Command::Discussion(DiscussionCmd::Create {
            title: s("title").ok_or_else(|| "discussion.create requires `title`".to_string())?,
            body: o("body"),
            task: o("task"),
            mode: o("mode"),
            window_secs: args.get("window_secs").and_then(Value::as_i64),
            min_ideas: args.get("min_ideas").and_then(Value::as_i64),
            session: sess(args),
            team: o("team"),
        }),
        "discussion.list" => Command::Discussion(DiscussionCmd::List {
            all: b("all"),
            session: sess(args),
            team: o("team"),
        }),
        "discussion.open" => Command::Discussion(DiscussionCmd::Open {
            id: s("id").ok_or_else(|| "discussion.open requires `id`".to_string())?,
            session: sess(args),
            team: o("team"),
        }),
        "discussion.say" => Command::Discussion(DiscussionCmd::Say {
            id: s("id").ok_or_else(|| "discussion.say requires `id`".to_string())?,
            body: s("body").ok_or_else(|| "discussion.say requires `body`".to_string())?,
            quotes: o("quotes"),
            kind: o("kind"),
            session: sess(args),
            team: o("team"),
        }),
        "discussion.reply" => Command::Discussion(DiscussionCmd::Reply {
            id: s("id").ok_or_else(|| "discussion.reply requires `id`".to_string())?,
            message_id: s("message_id").ok_or_else(|| "discussion.reply requires `message_id`".to_string())?,
            body: s("body").ok_or_else(|| "discussion.reply requires `body`".to_string())?,
            kind: o("kind"),
            session: sess(args),
            team: o("team"),
        }),
        "discussion.close" => Command::Discussion(DiscussionCmd::Close {
            id: s("id").ok_or_else(|| "discussion.close requires `id`".to_string())?,
            summary: o("summary"),
            directions: o("directions"),
            session: sess(args),
            team: o("team"),
        }),
        "discussion.handoff" => Command::Discussion(DiscussionCmd::Handoff {
            id: s("id").ok_or_else(|| "discussion.handoff requires `id`".to_string())?,
            direction_id: s("direction_id").ok_or_else(|| "discussion.handoff requires `direction_id`".to_string())?,
            target_kind: s("target_kind").ok_or_else(|| "discussion.handoff requires `target_kind`".to_string())?,
            target_id: s("target_id").ok_or_else(|| "discussion.handoff requires `target_id`".to_string())?,
            session: sess(args),
            team: o("team"),
        }),
        "discussion.subscribe" => Command::Discussion(DiscussionCmd::Subscribe {
            id: s("id").ok_or_else(|| "discussion.subscribe requires `id`".to_string())?,
            session: sess(args),
            team: o("team"),
        }),
        "discussion.unsubscribe" => Command::Discussion(DiscussionCmd::Unsubscribe {
            id: s("id").ok_or_else(|| "discussion.unsubscribe requires `id`".to_string())?,
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
        "ask.cancel" => Command::AskCancel {
            ask_id: s("ask_id").ok_or_else(|| "ask.cancel requires `ask_id`".to_string())?,
            reason: o("reason"),
            session: sess(args),
            team: o("team"),
        },
        "nudge.set" => Command::Nudge {
            action: crate::cli::NudgeAction::Set {
                kind: s("kind").ok_or_else(|| "nudge.set requires `kind`".to_string())?,
                template: o("template"),
                template_file: o("template_file"),
                session: sess(args),
                team: o("team"),
            },
        },
        "nudge.get" => Command::Nudge {
            action: crate::cli::NudgeAction::Get {
                kind: s("kind").ok_or_else(|| "nudge.get requires `kind`".to_string())?,
                session: sess(args),
                team: o("team"),
            },
        },
        "nudge.reset" => Command::Nudge {
            action: crate::cli::NudgeAction::Reset {
                kind: s("kind").ok_or_else(|| "nudge.reset requires `kind`".to_string())?,
                session: sess(args),
                team: o("team"),
            },
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
    let mut out = commands::execute(&cli, conn).map_err(|e| e.to_string())?;
    // `team.status` additionally reports the team's auto-created git repo:
    // whether it exists and what permission the calling member has on it.
    if method == "team.status" {
        annotate_git_repos(&mut out, conn, &member_id)?;
    }
    Ok(out)
}

/// Attach `git_repo: { exists, name, id, permission }` to each team of a
/// `team.status` result (for the certificate-derived calling member).
fn annotate_git_repos(
    out: &mut Value,
    conn: &rusqlite::Connection,
    member_id: &str,
) -> Result<(), String> {
    let Some(teams) = out.get_mut("teams").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    for t in teams {
        let name = t
            .get("team")
            .and_then(|x| x.get("name"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| t.get("name").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default();
        let team_id = t
            .get("team")
            .and_then(|x| x.get("id"))
            .and_then(Value::as_str)
            .or_else(|| t.get("id").and_then(Value::as_str))
            .unwrap_or_default();
        if name.is_empty() || team_id.is_empty() {
            continue;
        }
        let repo_name = crate::git_service::repo_name_from_team(&name);
        let status = match crate::git_service::get_repo(conn, team_id, &repo_name)
            .map_err(|e| format!("git_repo check: {e}"))?
        {
            Some(repo) => {
                let permission = if crate::git_service::check_permission(
                    conn,
                    &repo.id,
                    member_id,
                    crate::git_service::PERM_ADMIN,
                )
                .map_err(|e| format!("git_repo check: {e}"))?
                {
                    "admin"
                } else if crate::git_service::check_permission(
                    conn,
                    &repo.id,
                    member_id,
                    crate::git_service::PERM_WRITE,
                )
                .map_err(|e| format!("git_repo check: {e}"))?
                {
                    "write"
                } else if crate::git_service::check_permission(
                    conn,
                    &repo.id,
                    member_id,
                    crate::git_service::PERM_READ,
                )
                .map_err(|e| format!("git_repo check: {e}"))?
                {
                    "read"
                } else {
                    "none"
                };
                json!({
                    "exists": true,
                    "name": repo.name,
                    "id": repo.id,
                    "permission": permission,
                })
            }
            None => json!({ "exists": false }),
        };
        if let Some(obj) = t.as_object_mut() {
            obj.insert("git_repo".to_string(), status);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Git HTTP handlers
// ---------------------------------------------------------------------------

/// RPC dispatch for `git.*` methods (network mode).
///
/// Repos are per-team; the caller must be a member of the repo's team. The
/// `team` arg resolves like tunnels: omitted when the caller belongs to one
/// team, required otherwise.
fn git_dispatch(
    method: &str,
    args: &Value,
    conn: &mut rusqlite::Connection,
    member_id: &str,
) -> Result<Value, String> {
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);

    let team_id = match s("team") {
        Some(t) => t,
        None => {
            let teams = commands::teams_for_member(conn, member_id).map_err(|e| e.to_string())?;
            if teams.len() == 1 {
                teams[0].clone()
            } else {
                return Err(format!(
                    "git requires `team` (member belongs to {} teams)",
                    teams.len()
                ));
            }
        }
    };
    if !commands::member_in_team(conn, member_id, &team_id).map_err(|e| e.to_string())? {
        return Err(format!("member `{member_id}` is not a member of team {team_id}"));
    }

    match method {
        "git.repos" => {
            let repos = crate::git_service::list_accessible_repos(conn, &team_id, member_id)
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({ "ok": true, "repos": repos }))
        }
        "git.create" => {
            let name = s("name").ok_or_else(|| "git.create requires `name`".to_string())?;
            let description = s("description");
            // Owner only.
            let is_owner = commands::is_team_owner(conn, member_id, &team_id).map_err(|e| e.to_string())?;
            if !is_owner {
                return Err("only the team owner can create repositories".to_string());
            }
            // Reject duplicates up-front (the DB unique index also guards).
            if crate::git_service::get_repo(conn, &team_id, &name)
                .map_err(|e| e.to_string())?
                .is_some()
            {
                return Err(format!("repository `{name}` already exists"));
            }
            let repo = crate::git_service::create_repo(conn, &team_id, &name, description.as_deref(), member_id)
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({ "ok": true, "repo": repo }))
        }
        "git.delete" => {
            let name = s("name").ok_or_else(|| "git.delete requires `name`".to_string())?;
            let repo = crate::git_service::get_repo(conn, &team_id, &name)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("repository `{name}` not found"))?;
            let ok = crate::git_service::check_permission(conn, &repo.id, member_id, crate::git_service::PERM_ADMIN)
                .map_err(|e| e.to_string())?;
            if !ok {
                return Err("admin permission required to delete a repository".to_string());
            }
            crate::git_service::delete_repo(conn, &team_id, &name).map_err(|e| e.to_string())?;
            Ok(serde_json::json!({ "ok": true }))
        }
        "git.bundle" => {
            let name = s("name").ok_or_else(|| "git.bundle requires `name`".to_string())?;
            let repo = crate::git_service::get_repo(conn, &team_id, &name)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("repository `{name}` not found"))?;
            let ok = crate::git_service::check_permission(conn, &repo.id, member_id, crate::git_service::PERM_READ)
                .map_err(|e| e.to_string())?;
            if !ok {
                return Err("read permission required".to_string());
            }
            let (bundle, branch) =
                crate::git_service::create_bundle(&team_id, &name).map_err(|e| e.to_string())?;
            Ok(serde_json::json!({ "ok": true, "bundle": bundle, "branch": branch, "empty": bundle.is_empty() }))
        }
        "git.receive" => {
            let name = s("name").ok_or_else(|| "git.receive requires `name`".to_string())?;
            let bundle_b64 = s("bundle").ok_or_else(|| "git.receive requires `bundle`".to_string())?;
            let repo = crate::git_service::get_repo(conn, &team_id, &name)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("repository `{name}` not found"))?;
            let ok = crate::git_service::check_permission(conn, &repo.id, member_id, crate::git_service::PERM_WRITE)
                .map_err(|e| e.to_string())?;
            if !ok {
                return Err("write permission required to push".to_string());
            }
            let bytes = base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                &bundle_b64,
            )
            .map_err(|e| format!("bad bundle payload: {e}"))?;
            let tmp = std::env::temp_dir().join(format!("teamx-push-{}.bundle", uuid::Uuid::new_v4()));
            std::fs::write(&tmp, &bytes).map_err(|e| format!("write bundle: {e}"))?;
            let res = crate::git_service::receive_bundle(&team_id, &name, &tmp);
            let _ = std::fs::remove_file(&tmp);
            res.map_err(|e| e.to_string())?;
            Ok(serde_json::json!({ "ok": true }))
        }
        "git.grant" => {
            let name = s("name").ok_or_else(|| "git.grant requires `name`".to_string())?;
            let target = s("member_id").ok_or_else(|| "git.grant requires `member_id`".to_string())?;
            let perm = s("permission").unwrap_or_else(|| crate::git_service::PERM_READ.to_string());
            let repo = crate::git_service::get_repo(conn, &team_id, &name)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("repository `{name}` not found"))?;
            let ok = crate::git_service::check_permission(conn, &repo.id, member_id, crate::git_service::PERM_ADMIN)
                .map_err(|e| e.to_string())?;
            if !ok {
                return Err("admin permission required to grant access".to_string());
            }
            if !commands::member_in_team(conn, &target, &team_id).map_err(|e| e.to_string())? {
                return Err(format!("member `{target}` is not in team {team_id}"));
            }
            crate::git_service::grant_permission(conn, &repo.id, &target, &perm, member_id)
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({ "ok": true }))
        }
        "git.permissions" => {
            let name = s("name").ok_or_else(|| "git.permissions requires `name`".to_string())?;
            let repo = crate::git_service::get_repo(conn, &team_id, &name)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("repository `{name}` not found"))?;
            let ok = crate::git_service::check_permission(conn, &repo.id, member_id, crate::git_service::PERM_ADMIN)
                .map_err(|e| e.to_string())?;
            if !ok {
                return Err("admin permission required".to_string());
            }
            let perms = crate::git_service::list_permissions(conn, &repo.id).map_err(|e| e.to_string())?;
            Ok(serde_json::json!({ "ok": true, "permissions": perms }))
        }
        other => Err(format!("unknown git method `{other}`")),
    }
}

/// Extract member ID from peer certificate
fn extract_member_id(peer: &PeerIdentity) -> String {
    // CN format: member:<id>:<role>
    let parts: Vec<&str> = peer.0.split(':').collect();
    if parts.len() >= 2 && parts[0] == "member" {
        parts[1].to_string()
    } else {
        peer.0.clone()
    }
}

/// Extract team ID from query parameters or member's teams
fn extract_team_id(args: &serde_json::Value) -> Option<String> {
    args.get("team").and_then(|t| t.as_str()).map(|s| s.to_string())
}

/// GET /git/repos - List repositories
async fn git_list_repos(
    State(state): State<AppState>,
    Extension(peer): Extension<PeerIdentity>,
    axum::extract::Query(args): axum::extract::Query<serde_json::Value>,
) -> Result<Json<Value>, StatusCode> {
    let member_id = extract_member_id(&peer);
    let team_id = extract_team_id(&args).ok_or(StatusCode::BAD_REQUEST)?;
    
    let db = state.db.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    
    // Check if member belongs to the team
    let is_member: bool = db
        .query_row(
            "SELECT COUNT(*) > 0 FROM members WHERE team_id = ?1 AND session_key = ?2",
            params![team_id, member_id],
            |row| row.get(0),
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    
    if !is_member {
        return Err(StatusCode::FORBIDDEN);
    }
    
    let repos = crate::git_service::list_accessible_repos(&db, &team_id, &member_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    
    Ok(Json(json!({
        "ok": true,
        "repos": repos,
    })))
}

/// POST /git/repos - Create a repository
async fn git_create_repo(
    State(state): State<AppState>,
    Extension(peer): Extension<PeerIdentity>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    let member_id = extract_member_id(&peer);
    let team_id = body.get("team_id").and_then(|t| t.as_str()).ok_or(StatusCode::BAD_REQUEST)?;
    let name = body.get("name").and_then(|n| n.as_str()).ok_or(StatusCode::BAD_REQUEST)?;
    let description = body.get("description").and_then(|d| d.as_str());
    
    let db = state.db.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    
    // Check if member is a team lead (owner or co-lead).
    let is_owner: bool = commands::is_team_owner(&db, &member_id, team_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    
    if !is_owner {
        return Err(StatusCode::FORBIDDEN);
    }
    
    let repo = crate::git_service::create_repo(&db, team_id, name, description, &member_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    
    Ok(Json(json!({
        "ok": true,
        "repo": repo,
    })))
}

/// GET /git/repos/:repo - Get repository info
async fn git_get_repo(
    State(state): State<AppState>,
    Extension(peer): Extension<PeerIdentity>,
    axum::extract::Path(repo_name): axum::extract::Path<String>,
    axum::extract::Query(args): axum::extract::Query<serde_json::Value>,
) -> Result<Json<Value>, StatusCode> {
    let member_id = extract_member_id(&peer);
    let team_id = extract_team_id(&args).ok_or(StatusCode::BAD_REQUEST)?;
    
    let db = state.db.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    
    // Check permission
    let repo = crate::git_service::get_repo(&db, &team_id, &repo_name)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    
    let has_perm = crate::git_service::check_permission(&db, &repo.id, &member_id, "read")
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    
    if !has_perm {
        return Err(StatusCode::FORBIDDEN);
    }
    
    Ok(Json(json!({
        "ok": true,
        "repo": repo,
    })))
}

/// DELETE /git/repos/:repo - Delete a repository
async fn git_delete_repo(
    State(state): State<AppState>,
    Extension(peer): Extension<PeerIdentity>,
    axum::extract::Path(repo_name): axum::extract::Path<String>,
    axum::extract::Query(args): axum::extract::Query<serde_json::Value>,
) -> Result<Json<Value>, StatusCode> {
    let member_id = extract_member_id(&peer);
    let team_id = extract_team_id(&args).ok_or(StatusCode::BAD_REQUEST)?;
    
    let db = state.db.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    
    // Check permission
    let repo = crate::git_service::get_repo(&db, &team_id, &repo_name)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    
    let has_perm = crate::git_service::check_permission(&db, &repo.id, &member_id, "admin")
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    
    if !has_perm {
        return Err(StatusCode::FORBIDDEN);
    }
    
    crate::git_service::delete_repo(&db, &team_id, &repo_name)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    
    Ok(Json(json!({
        "ok": true,
    })))
}

/// GET /git/repos/:repo/permissions - List permissions
async fn git_list_permissions(
    State(state): State<AppState>,
    Extension(peer): Extension<PeerIdentity>,
    axum::extract::Path(repo_name): axum::extract::Path<String>,
    axum::extract::Query(args): axum::extract::Query<serde_json::Value>,
) -> Result<Json<Value>, StatusCode> {
    let member_id = extract_member_id(&peer);
    let team_id = extract_team_id(&args).ok_or(StatusCode::BAD_REQUEST)?;
    
    let db = state.db.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    
    // Check permission
    let repo = crate::git_service::get_repo(&db, &team_id, &repo_name)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    
    let has_perm = crate::git_service::check_permission(&db, &repo.id, &member_id, "admin")
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    
    if !has_perm {
        return Err(StatusCode::FORBIDDEN);
    }
    
    let perms = crate::git_service::list_permissions(&db, &repo.id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    
    Ok(Json(json!({
        "ok": true,
        "permissions": perms,
    })))
}

/// POST /git/repos/:repo/permissions - Grant permission
async fn git_grant_permission(
    State(state): State<AppState>,
    Extension(peer): Extension<PeerIdentity>,
    axum::extract::Path(repo_name): axum::extract::Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    let member_id = extract_member_id(&peer);
    let team_id = body.get("team_id").and_then(|t| t.as_str()).ok_or(StatusCode::BAD_REQUEST)?;
    let target_member_id = body.get("member_id").and_then(|m| m.as_str()).ok_or(StatusCode::BAD_REQUEST)?;
    let permission = body.get("permission").and_then(|p| p.as_str()).unwrap_or("read");
    
    let db = state.db.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    
    // Check permission
    let repo = crate::git_service::get_repo(&db, team_id, &repo_name)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    
    let has_perm = crate::git_service::check_permission(&db, &repo.id, &member_id, "admin")
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    
    if !has_perm {
        return Err(StatusCode::FORBIDDEN);
    }
    
    crate::git_service::grant_permission(&db, &repo.id, target_member_id, permission, &member_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    
    Ok(Json(json!({
        "ok": true,
    })))
}

/// POST /git/repos/:repo/clone - Clone a repository
async fn git_clone_repo(
    State(state): State<AppState>,
    Extension(peer): Extension<PeerIdentity>,
    axum::extract::Path(repo_name): axum::extract::Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    let member_id = extract_member_id(&peer);
    let team_id = body.get("team_id").and_then(|t| t.as_str()).ok_or(StatusCode::BAD_REQUEST)?;
    
    let db = state.db.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    
    // Check permission
    let repo = crate::git_service::get_repo(&db, team_id, &repo_name)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    
    let has_perm = crate::git_service::check_permission(&db, &repo.id, &member_id, "read")
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    
    if !has_perm {
        return Err(StatusCode::FORBIDDEN);
    }
    
    // TODO: Implement actual git clone
    // For now, return placeholder
    Ok(Json(json!({
        "ok": true,
        "repo": repo_name,
        "message": "Git clone operation (placeholder)",
    })))
}

/// POST /git/repos/:repo/pull - Pull from repository
async fn git_pull_repo(
    State(state): State<AppState>,
    Extension(peer): Extension<PeerIdentity>,
    axum::extract::Path(repo_name): axum::extract::Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    let member_id = extract_member_id(&peer);
    let team_id = body.get("team_id").and_then(|t| t.as_str()).ok_or(StatusCode::BAD_REQUEST)?;
    
    let db = state.db.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    
    // Check permission
    let repo = crate::git_service::get_repo(&db, team_id, &repo_name)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    
    let has_perm = crate::git_service::check_permission(&db, &repo.id, &member_id, "read")
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    
    if !has_perm {
        return Err(StatusCode::FORBIDDEN);
    }
    
    // TODO: Implement actual git pull
    // For now, return placeholder
    Ok(Json(json!({
        "ok": true,
        "repo": repo_name,
        "message": "Git pull operation (placeholder)",
    })))
}

/// POST /git/repos/:repo/push - Push to repository
async fn git_push_repo(
    State(state): State<AppState>,
    Extension(peer): Extension<PeerIdentity>,
    axum::extract::Path(repo_name): axum::extract::Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    let member_id = extract_member_id(&peer);
    let team_id = body.get("team_id").and_then(|t| t.as_str()).ok_or(StatusCode::BAD_REQUEST)?;
    
    let db = state.db.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    
    // Check permission
    let repo = crate::git_service::get_repo(&db, team_id, &repo_name)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    
    let has_perm = crate::git_service::check_permission(&db, &repo.id, &member_id, "write")
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    
    if !has_perm {
        return Err(StatusCode::FORBIDDEN);
    }
    
    // TODO: Implement actual git push
    // For now, return placeholder
    Ok(Json(json!({
        "ok": true,
        "repo": repo_name,
        "message": "Git push operation (placeholder)",
    })))
}

// ---------------------------------------------------------------------------
// Git Smart HTTP (standard git protocol over mTLS)
// ---------------------------------------------------------------------------

/// Resolve member + team membership + repo permission for a Smart HTTP request.
/// Returns `(member_id, repo_id)` on success.
fn git_http_authorize(
    conn: &rusqlite::Connection,
    member_id: &str,
    team_id: &str,
    repo_name: &str,
    required: &str,
) -> Result<String, Box<Response>> {
    use crate::git_service as g;
    if !commands::member_in_team(conn, member_id, team_id).unwrap_or(false) {
        return Err(Box::new(git_http_error(StatusCode::FORBIDDEN, "not a member of team")));
    }
    let repo = g::get_repo(conn, team_id, repo_name)
        .map_err(|_| Box::new(git_http_error(StatusCode::INTERNAL_SERVER_ERROR, "db error")))?
        .ok_or_else(|| Box::new(git_http_error(StatusCode::NOT_FOUND, "repository not found")))?;
    let ok = g::check_permission(conn, &repo.id, member_id, required).unwrap_or(false);
    if !ok {
        return Err(Box::new(git_http_error(
            StatusCode::FORBIDDEN,
            &format!("need `{required}` permission"),
        )));
    }
    Ok(repo.id)
}

/// Build an error Response (smart HTTP clients show this in the git message).
fn git_http_error(status: StatusCode, msg: &str) -> Response {
    let body = format!("teamx: {msg}\n");
    Response::builder()
        .status(status)
        .header("Content-Type", "text/plain")
        .body(axum::body::Body::from(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// `GET /git/<team>/<repo>/info/refs?service=<svc>`
async fn git_http_info_refs(
    State(state): State<AppState>,
    Extension(peer): Extension<PeerIdentity>,
    Path((team_id, repo_name)): Path<(String, String)>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let member_id = extract_member_id(&peer);
    let service = match params.get("service") {
        Some(s) => s.clone(),
        None => {
            // Dumb protocol: not supported.
            return git_http_error(StatusCode::NOT_IMPLEMENTED, "dumb HTTP protocol not supported");
        }
    };
    // Permission level: upload-pack (fetch) needs read; receive-pack (push) needs write.
    let required = if service == "git-receive-pack" { "write" } else { "read" };
    let db = match state.db.lock() {
        Ok(db) => db,
        Err(_) => return git_http_error(StatusCode::INTERNAL_SERVER_ERROR, "db lock"),
    };
    if let Err(resp) = git_http_authorize(&db, &member_id, &team_id, &repo_name, required) {
        return *resp;
    }
    drop(db); // release lock before long-running git process
    match crate::git_service::info_refs(&team_id, &repo_name, &service) {
        Ok(result) => {
            let mut resp = Response::new(axum::body::Body::from(result.body));
            *resp.status_mut() = StatusCode::OK;
            resp.headers_mut().insert(
                "Content-Type",
                result.content_type.parse().unwrap_or_else(|_| "text/plain".parse().unwrap()),
            );
            resp.headers_mut().insert(
                "Cache-Control",
                "no-cache, max-age=0".parse().unwrap(),
            );
            resp.headers_mut().insert("Expires", "Fri, 01 Jan 1980 00:00:00 GMT".parse().unwrap());
            resp.headers_mut().insert("Pragma", "no-cache".parse().unwrap());
            resp
        }
        Err(e) => git_http_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
}

/// Read a full request body into bytes (Smart HTTP POST payloads are pkt-line
/// protocol; size can be a few hundred KB to MBs for pushes).
async fn read_body(body: Body) -> Result<Bytes, Response> {
    // 512 MiB cap for push payloads.
    axum::body::to_bytes(body, 512 * 1024 * 1024)
        .await
        .map_err(|e| git_http_error(StatusCode::BAD_REQUEST, &format!("body read error: {e}")))
}

/// `POST /git/<team>/<repo>/git-upload-pack` — fetch/clone/pull.
async fn git_http_upload_pack(
    State(state): State<AppState>,
    Extension(peer): Extension<PeerIdentity>,
    Path((team_id, repo_name)): Path<(String, String)>,
    body: Body,
) -> Response {
    let member_id = extract_member_id(&peer);
    let payload = match read_body(body).await {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    {
        let db = match state.db.lock() {
            Ok(db) => db,
            Err(_) => return git_http_error(StatusCode::INTERNAL_SERVER_ERROR, "db lock"),
        };
        if let Err(resp) = git_http_authorize(&db, &member_id, &team_id, &repo_name, "read") {
            return *resp;
        }
    }
    match crate::git_service::upload_pack(&team_id, &repo_name, &payload) {
        Ok(result) => {
            let mut resp = Response::new(axum::body::Body::from(result.body));
            *resp.status_mut() = StatusCode::OK;
            resp.headers_mut().insert(
                "Content-Type",
                result.content_type.parse().unwrap_or_else(|_| "text/plain".parse().unwrap()),
            );
            resp.headers_mut().insert("Cache-Control", "no-cache".parse().unwrap());
            resp
        }
        Err(e) => git_http_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
}

/// `POST /git/<team>/<repo>/git-receive-pack` — push.
async fn git_http_receive_pack(
    State(state): State<AppState>,
    Extension(peer): Extension<PeerIdentity>,
    Path((team_id, repo_name)): Path<(String, String)>,
    body: Body,
) -> Response {
    let member_id = extract_member_id(&peer);
    let payload = match read_body(body).await {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    {
        let db = match state.db.lock() {
            Ok(db) => db,
            Err(_) => return git_http_error(StatusCode::INTERNAL_SERVER_ERROR, "db lock"),
        };
        if let Err(resp) = git_http_authorize(&db, &member_id, &team_id, &repo_name, "write") {
            return *resp;
        }
    }
    match crate::git_service::receive_pack(&team_id, &repo_name, &payload) {
        Ok(result) => {
            let mut resp = Response::new(axum::body::Body::from(result.body));
            *resp.status_mut() = StatusCode::OK;
            resp.headers_mut().insert(
                "Content-Type",
                result.content_type.parse().unwrap_or_else(|_| "text/plain".parse().unwrap()),
            );
            resp.headers_mut().insert("Cache-Control", "no-cache".parse().unwrap());
            resp
        }
        Err(e) => git_http_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
}

#[cfg(test)]
mod rpc_body_limit_tests {
    use super::*;
    use axum::http::Request;
    use tower::ServiceExt; // oneshot

    #[test]
    fn service_method_whitelist_is_closed() {        // Allowed: the IM read-only query + the four P2 write actions + binding.
        for m in ["im.query", "im.approve", "im.deny", "im.verify", "im.reject", "im.card.request", "im.card.execute", "im.bind", "im.chat.log"] {
            assert!(service_method_allowed(m), "{m} should be allowed");
        }
        // Fail-closed: everything else is refused, including dangerous methods.
        for m in [
            "team.destroy",
            "goal.close",
            "task.create",
            "git.receive",
            "im.binding",
            "im.set",
            "im.unbind",
            "member.set_state",
        ] {
            assert!(!service_method_allowed(m), "{m} must be refused");
        }
    }

    #[test]
    fn parse_threshold_defaults_and_overrides() {
        assert_eq!(parse_rpc_max_body(None), RPC_MAX_BODY_DEFAULT);
        assert_eq!(parse_rpc_max_body(Some("")), RPC_MAX_BODY_DEFAULT);
        assert_eq!(parse_rpc_max_body(Some("abc")), RPC_MAX_BODY_DEFAULT);
        assert_eq!(parse_rpc_max_body(Some("-5")), RPC_MAX_BODY_DEFAULT);
        assert_eq!(parse_rpc_max_body(Some("0")), RPC_MAX_BODY_DEFAULT);
        assert_eq!(parse_rpc_max_body(Some(" 1048576 ")), 1024 * 1024);
        assert_eq!(parse_rpc_max_body(Some("268435456")), 256 * 1024 * 1024);
    }

    async fn echo_json(Json(v): Json<Value>) -> Json<Value> {
        Json(v)
    }

    /// The production wiring: /rpc with the raised DefaultBodyLimit. A body
    /// above axum's built-in 2 MiB default must reach the handler (no 413).
    #[tokio::test]
    async fn rpc_accepts_body_above_axum_default() {
        let app = Router::new().route(
            "/rpc",
            post(echo_json).layer(DefaultBodyLimit::max(parse_rpc_max_body(None))),
        );
        // ~3 MiB JSON payload: {"pad": "aaaa..."} — over the 2 MiB default.
        let pad = "a".repeat(3 * 1024 * 1024);
        let body = format!("{{\"pad\":\"{pad}\"}}");
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/rpc")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// Control: without the raised limit the same payload is rejected 413,
    /// proving the test above actually exercises the limit path.
    #[tokio::test]
    async fn rpc_without_layer_still_413s_above_2mib() {
        let app = Router::new().route("/rpc", post(echo_json));
        let pad = "a".repeat(3 * 1024 * 1024);
        let body = format!("{{\"pad\":\"{pad}\"}}");
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/rpc")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[test]
    fn service_identity_whitelist_delegation_and_unbound() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        conn.execute(
            "INSERT INTO teams (id, name, owner_member_id, state, invite_token, created_at, updated_at)
             VALUES ('t1', 'T', 'm1', 'active', 'tok', 'now', 'now')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO members (id, team_id, session_key, display_name, role, state, joined_at)
             VALUES ('m1', 't1', 's', 'M', 'contributor', 'active', 'now')",
            [],
        )
        .unwrap();
        let tunnels = crate::tunnel::TunnelRegistry::new();

        // Delegations are cryptographically verified (gateway::accept), so the
        // test env must carry the provider secret the signature is keyed on.
        std::env::set_var("TEAMX_IM_FEISHU_APP_SECRET", "test-gw-secret");
        let signed = |im_user: &str, nonce: &str| {
            let mut ctx = crate::im::types::ImDelegatedContext {
                platform: "feishu".into(),
                im_user_id: im_user.into(),
                im_team: None,
                signature: None,
                timestamp: chrono::Utc::now().timestamp(),
                nonce: nonce.into(),
            };
            ctx.signature = Some(crate::im::gateway::sign_delegation("test-gw-secret", &ctx));
            serde_json::json!({
                "platform": ctx.platform,
                "im_user_id": ctx.im_user_id,
                "team": "t1",
                "signature": ctx.signature,
                "timestamp": ctx.timestamp,
                "nonce": ctx.nonce,
            })
        };
        let good = signed("u1", "nonce-good");

        // Unknown service identity → refused.
        assert!(
            dispatch_service("evil", "i", "im.query", &good, &mut conn, &tunnels, None, None).is_err()
        );
        // Non-whitelisted methods → refused (fail-closed).
        assert!(
            dispatch_service("im-gateway", "g1", "team.destroy", &good, &mut conn, &tunnels, None, None)
                .is_err()
        );
        assert!(
            dispatch_service("im-gateway", "g1", "git.receive", &good, &mut conn, &tunnels, None, None)
                .is_err()
        );
        // A caller-supplied member_id is never trusted.
        let mut with_mid = signed("u1", "nonce-mid");
        with_mid["member_id"] = serde_json::json!("m1");
        let e = dispatch_service("im-gateway", "g1", "im.query", &with_mid, &mut conn, &tunnels, None, None)
            .unwrap_err();
        assert!(e.contains("member_id"), "{e}");
        // A forged signature → refused + audited (delegation must not be
        // forgeable: pr-57 review finding ②).
        let mut forged = signed("u1", "nonce-forged");
        forged["signature"] = serde_json::json!("deadbeef");
        let e = dispatch_service("im-gateway", "g1", "im.query", &forged, &mut conn, &tunnels, None, None)
            .unwrap_err();
        assert!(e.contains("invalid_signature"), "{e}");
        // An unsigned delegation (nonce present, signature absent) → refused.
        let unsigned = serde_json::json!({"platform":"feishu","im_user_id":"u1","team":"t1","nonce":"nonce-unsigned","timestamp":chrono::Utc::now().timestamp()});
        let e = dispatch_service("im-gateway", "g1", "im.query", &unsigned, &mut conn, &tunnels, None, None)
            .unwrap_err();
        assert!(e.contains("missing_signature"), "{e}");
        // Unbound (but correctly signed) delegation → refused + audited.
        let e = dispatch_service("im-gateway", "g1", "im.query", &signed("u9", "nonce-unbound"), &mut conn, &tunnels, None, None)
            .unwrap_err();
        assert!(e.contains("unbound"), "{e}");
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM events WHERE type = 'im.rejected'", [], |r| r.get(0))
            .unwrap();
        assert!(n >= 3, "rejections must be audited as im.rejected (got {n})");
    }

    /// 方案 A: `im.bind` is the one pre-binding action. A signed gateway
    /// delegation carrying a valid one-time code must bind an IM user who is
    /// not yet a member (member resolution is skipped for this method only).
    #[test]
    fn service_bind_is_allowed_before_a_binding_exists() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        conn.execute(
            "INSERT INTO teams (id, name, owner_member_id, goal_id, state, invite_token, created_at, updated_at)
             VALUES ('t1','T','m1',NULL,'active','tok','now','now')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO members (id, team_id, session_key, display_name, role, state, is_lead, joined_at)
             VALUES ('m1','t1','s1','M','contributor','active',0,'now')",
            [],
        )
        .unwrap();
        let tunnels = crate::tunnel::TunnelRegistry::new();
        std::env::set_var("TEAMX_IM_FEISHU_APP_SECRET", "test-gw-secret");

        let code = crate::im::binding::create_code(&mut conn, "m1", "t1", None).unwrap()["code"]
            .as_str()
            .unwrap()
            .to_string();

        let mut ctx = crate::im::types::ImDelegatedContext {
            platform: "feishu".into(),
            im_user_id: "ou-newbie".into(),
            im_team: None,
            signature: None,
            timestamp: chrono::Utc::now().timestamp(),
            nonce: "nonce-bind".into(),
        };
        ctx.signature = Some(crate::im::gateway::sign_delegation("test-gw-secret", &ctx));
        let args = serde_json::json!({
            "platform": ctx.platform,
            "im_user_id": ctx.im_user_id,
            "team": "t1",
            "signature": ctx.signature,
            "timestamp": ctx.timestamp,
            "nonce": ctx.nonce,
            "code": code,
        });

        let out =
            dispatch_service("im-gateway", "g1", "im.bind", &args, &mut conn, &tunnels, None, None)
                .expect("a valid code must bind an unbound IM user");
        assert_eq!(out["ok"], serde_json::json!(true));
        let bound = crate::im::binding::resolve_member(&mut conn, "feishu", "ou-newbie", "t1").unwrap();
        assert!(bound.is_some(), "binding must exist after im.bind");
        // The code is single-use: a replay of the same delegation is refused.
        let mut replay = args.clone();
        replay["nonce"] = serde_json::json!("nonce-bind-2");
        let mut ctx2 = ctx.clone();
        ctx2.nonce = "nonce-bind-2".into();
        ctx2.signature = Some(crate::im::gateway::sign_delegation("test-gw-secret", &ctx2));
        replay["signature"] = serde_json::json!(ctx2.signature);
        replay["timestamp"] = serde_json::json!(ctx2.timestamp);
        let e = dispatch_service("im-gateway", "g1", "im.bind", &replay, &mut conn, &tunnels, None, None)
            .unwrap_err();
        assert!(!e.is_empty(), "a used code must not bind twice: {e}");
    }
}
