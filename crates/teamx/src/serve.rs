//! teamx serve — network-mode server (mTLS, HTTP JSON RPC).
//!
//! Serves the same command surface as the V1 CLI over HTTP RPC so a plugin on
//! another machine can talk to one shared ledger. All command logic lives in
//! `commands::execute`; this module only translates an RPC request into a
//! `Command` value and serializes the result back.
//!
//! Security: the server REQUIRES mutual TLS. Clients must present a certificate
//! signed by the instance CA (`~/.teamx/ca/ca.crt`); the client certificate CN
//! carries the member identity (`member:<id>:<role>`).

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use axum_server::tls_rustls::RustlsConfig;
use serde_json::{json, Value};

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
    let home = crate::db::teamx_home();
    let pk = pki::ensure_pki(&home)?;
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
        axum_server::bind_rustls(addr, config)
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

async fn rpc(State(state): State<AppState>, Json(req): Json<RpcRequest>) -> impl IntoResponse {
    let result = tokio::task::spawn_blocking(move || {
        let mut conn = state.db.lock().unwrap();
        dispatch(&req.method, &req.args, &mut conn)
    })
    .await;

    match result {
        Ok(Ok(data)) => (StatusCode::OK, Json(json!({ "ok": true, "data": data }))),
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": e }))),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "ok": false, "error": format!("internal: {e}") }))),
    }
}

/// Translate an RPC request into the same `Command` enum the CLI dispatches,
/// then run it through `commands::execute`.
fn dispatch(method: &str, args: &Value, conn: &mut rusqlite::Connection) -> Result<Value, String> {
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
    let o = |k: &str| s(k);
    let b = |k: &str| args.get(k).and_then(Value::as_bool).unwrap_or(false);
    let session = |args: &Value| args.get("session").and_then(Value::as_str).unwrap_or("").to_string();

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
            session: session(args),
            goal_title: o("goal_title"),
            goal_body: o("goal_body"),
        }),
        "team.join" => Command::Team(TeamCmd::Join {
            token: s("token").ok_or_else(|| "team.join requires `token`".to_string())?,
            name: s("name").ok_or_else(|| "team.join requires `name`".to_string())?,
            session: session(args),
            loopx_project: None,
        }),
        "team.approve" => Command::Team(TeamCmd::Approve {
            member_id: s("member_id").ok_or_else(|| "team.approve requires `member_id`".to_string())?,
            session: session(args),
            team: o("team"),
        }),
        "team.deny" => Command::Team(TeamCmd::Deny {
            member_id: s("member_id").ok_or_else(|| "team.deny requires `member_id`".to_string())?,
            session: session(args),
            team: o("team"),
        }),
        "team.list" => Command::Team(TeamCmd::List { session: session(args) }),
        "team.status" => Command::Team(TeamCmd::Status { team: o("team"), session: o("session") }),
        "team.leave" => Command::Team(TeamCmd::Leave { session: session(args), team: o("team") }),
        "team.archive" => Command::Team(TeamCmd::Archive { session: session(args), team: o("team") }),

        "goal.set" => Command::Goal(GoalCmd::Set {
            title: s("title").ok_or_else(|| "goal.set requires `title`".to_string())?,
            body: o("body"),
            session: session(args),
            team: o("team"),
        }),
        "goal.share" => Command::Goal(GoalCmd::Share { session: session(args), team: o("team") }),
        "goal.close" => Command::Goal(GoalCmd::Close { session: session(args), team: o("team") }),

        "member.set_state" => Command::Member(MemberCmd::SetState {
            state: s("state").ok_or_else(|| "member.set_state requires `state`".to_string())?,
            member: o("member"),
            session: session(args),
            team: o("team"),
        }),

        "role.list" => Command::Role(RoleCmd::List { team: o("team") }),
        "role.set" => Command::Role(RoleCmd::Set {
            role: s("role").ok_or_else(|| "role.set requires `role`".to_string())?,
            session: session(args),
            member: o("member"),
            team: o("team"),
        }),
        "role.propose" => Command::Role(RoleCmd::Propose {
            role: s("role").ok_or_else(|| "role.propose requires `role`".to_string())?,
            label: s("label").ok_or_else(|| "role.propose requires `label`".to_string())?,
            description: o("description"),
            session: session(args),
            team: o("team"),
        }),
        "role.approve" => Command::Role(RoleCmd::Approve {
            role: s("role").ok_or_else(|| "role.approve requires `role`".to_string())?,
            session: session(args),
            team: o("team"),
        }),
        "role.deny" => Command::Role(RoleCmd::Deny {
            role: s("role").ok_or_else(|| "role.deny requires `role`".to_string())?,
            session: session(args),
            team: o("team"),
        }),
        "role.update" => Command::Role(RoleCmd::Update {
            role: s("role").ok_or_else(|| "role.update requires `role`".to_string())?,
            label: o("label"),
            description: o("description"),
            session: session(args),
            team: o("team"),
        }),

        "publish" => Command::Publish {
            r#type: s("type").ok_or_else(|| "publish requires `type`".to_string())?,
            data: o("data"),
            session: session(args),
            team: o("team"),
        },
        "ask" => Command::Ask {
            member_id: s("member_id").ok_or_else(|| "ask requires `member_id`".to_string())?,
            question: s("question").ok_or_else(|| "ask requires `question`".to_string())?,
            session: session(args),
            team: o("team"),
        },
        "respond" => Command::Respond {
            ask_id: s("ask_id").ok_or_else(|| "respond requires `ask_id`".to_string())?,
            answer: s("answer").ok_or_else(|| "respond requires `answer`".to_string())?,
            session: session(args),
        },
        "events" => Command::Events {
            after: args.get("after").and_then(Value::as_i64),
            team: o("team"),
        },
        "log" => Command::Log {
            team: o("team"),
            session: o("session"),
            limit: args.get("limit").and_then(Value::as_i64),
            after: args.get("after").and_then(Value::as_i64),
        },
        "sync" => Command::Sync { session: session(args), no_advance: b("no_advance") },
        "loopx.report" => Command::Loopx(LoopxCmd::Report {
            project: s("project").ok_or_else(|| "loopx.report requires `project`".to_string())?.into(),
            session: session(args),
            team: o("team"),
        }),

        other => return Err(format!("unknown rpc method `{other}`")),
    };

    let cli = make(cmd);
    commands::execute(&cli, conn).map_err(|e| e.to_string())
}
