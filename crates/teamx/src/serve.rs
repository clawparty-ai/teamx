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

use axum::{
    extract::State,
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

use crate::cli::{Cli, Command, GoalCmd, LoopxCmd, MemberCmd, RoleCmd, ServeCmd, TeamCmd};
use crate::commands;
use crate::pki;

type Db = Mutex<rusqlite::Connection>;

#[derive(Clone)]
struct AppState {
    db: std::sync::Arc<Db>,
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
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/rpc", post(rpc))
        .with_state(state);

    let addr: SocketAddr = format!("{}:{}", cmd.addr, cmd.port)
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
            .serve(app.into_make_service())
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

async fn health() -> impl IntoResponse {
    Json(json!({ "ok": true, "service": "teamx", "version": env!("CARGO_PKG_VERSION") }))
}

async fn rpc(
    State(state): State<AppState>,
    Extension(identity): Extension<PeerIdentity>,
    Json(req): Json<RpcRequest>,
) -> impl IntoResponse {
    let cn = identity.0;
    let result = tokio::task::spawn_blocking(move || {
        let mut conn = state.db.lock().unwrap();
        dispatch(&req.method, &req.args, &mut conn, &cn)
    })
    .await;

    match result {
        Ok(Ok(data)) => (StatusCode::OK, Json(json!({ "ok": true, "data": data }))),
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": e }))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": format!("internal: {e}") })),
        ),
    }
}

/// Translate an RPC request into the same `Command` enum the CLI dispatches,
/// then run it through `commands::execute`. The actor identity comes from the
/// verified client certificate CN (`actor_cn`), not the self-reported `session`.
fn dispatch(method: &str, args: &Value, conn: &mut rusqlite::Connection, actor_cn: &str) -> Result<Value, String> {
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
    let o = |k: &str| s(k);
    let b = |k: &str| args.get(k).and_then(Value::as_bool).unwrap_or(false);

    // Identity from the certificate: `member:<id>:<role>`.
    let member_id = pki::parse_member_cn(actor_cn)
        .map(|(id, _role)| id)
        .ok_or_else(|| format!("no member identity in client certificate CN `{actor_cn}`"))?;

    // `team.import` registers a member whose seat is pre-allocated in the
    // invitation (the member row may not exist yet). It binds that seat to the
    // certificate-derived member id, so handle it before the generic dispatch.
    if method == "team.import" {
        let letter = s("letter").ok_or_else(|| "team.import requires `letter`".to_string())?;
        let name = o("name");
        return commands::import_with_cert(conn, &letter, name.as_deref(), &member_id)
            .map_err(|e| e.to_string());
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
