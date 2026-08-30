//! teamx ui — enterprise activity analytics dashboard (HTTPS + token).
//!
//! Only the team lead (owner) can start it. The server binds 127.0.0.1 by
//! default, presents the instance's server certificate (self-signed, reused
//! from `teamx serve`'s PKI), and requires a random per-launch token either in
//! the URL query string (`?token=...`) or in a cookie set after first visit.
//!
//! Data is read directly from the local SQLite DB (the same file the serve
//! process writes on this machine). Query API mirrors the serve RPCs:
//!   GET /api/overview?team=...
//!   GET /api/by_member?team=...
//!   GET /api/by_node?team=...
//!   GET /api/tools?team=...
//!   GET /api/files?team=...
//!   GET /api/rows?team=...
//!   GET /api/human?team=...
//! All require the session cookie; `?token=` sets it.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::{
        ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::{header, StatusCode},
    response::{Html, IntoResponse, Redirect},
    routing::{get, post},
    Json, Router,
};
use axum_server::tls_rustls::RustlsConfig;
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::json;

use crate::cli::UiCmd;
use crate::pki;

/// xterm.js vendored for the web terminal (no external CDN dependency).
static XTERM_JS: &str = include_str!("assets/xterm.min.js");
static XTERM_CSS: &str = include_str!("assets/xterm.css");

/// Launch the dashboard. Blocks until the server stops (Ctrl+C / kill).
pub fn ui(cmd: &UiCmd) -> Result<(), String> {
    let db_path = cmd.db.clone().unwrap_or_else(crate::db::default_db_path);
    let conn = crate::db::open(&db_path).map_err(|e| format!("cannot open database {db_path:?}: {e}"))?;
    crate::db::migrate(&conn).map_err(|e| format!("schema init failed: {e}"))?;

    // Owner check: the session must own at least one non-destroyed team.
    // Collect the owned team ids so live-member proxies can verify that a
    // requested member actually belongs to one of the owner's teams.
    let conn = Arc::new(std::sync::Mutex::new(conn));
    let owner_teams: Vec<String> = {
        let c = conn.lock().unwrap();
        let mut stmt = c
            .prepare(
                "SELECT DISTINCT m.team_id FROM members m JOIN teams t ON t.id = m.team_id
                 WHERE m.session_key = ?1 AND m.state NOT IN ('left','denied') AND t.state NOT IN ('destroyed') AND t.owner_member_id = m.id",
            )
            .map_err(|e| format!("owner check failed: {e}"))?;
        let rows = stmt
            .query_map([&cmd.session], |r| r.get::<_, String>(0))
            .map_err(|e| format!("owner check failed: {e}"))?;
        let mut v = Vec::new();
        for r in rows {
            v.push(r.map_err(|e| format!("owner check failed: {e}"))?);
        }
        v
    };
    if owner_teams.is_empty() {
        return Err(format!(
            "`teamx ui` is owner-only: session `{}` does not own any team",
            cmd.session
        ));
    }

    // Random per-launch token (32 bytes hex).
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );

    // HTTPS: reuse the instance server cert/key (self-signed, localhost SANs).
    let home = crate::db::teamx_home();
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

    // Resolve the teamx server URL (for member tunnel proxying / web terminal).
    let server_url = match &cmd.server_url {
        Some(u) if !u.is_empty() => Some(u.clone()),
        _ => std::env::var("TEAMX_SERVER_URL").ok().filter(|u| !u.is_empty()),
    };
    if server_url.is_some() {
        eprintln!("teamx ui: live member view enabled (server_url={})", server_url.as_deref().unwrap_or(""));
    } else {
        eprintln!("teamx ui: live member view disabled (no server_url; pass --server-url or set TEAMX_SERVER_URL)");
    }

    let state = Arc::new(AppState {
        conn,
        token: token.clone(),
        server_url,
        owner_teams,
        pool: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/login", get(login_page))
        .route("/members-live", get(index))
        .route("/xterm.js", get(xterm_js))
        .route("/xterm.css", get(xterm_css))
        .route("/api/teams", get(api_teams))
        .route("/api/goal", get(api_goal))
        .route("/api/members", get(api_members))
        .route("/api/kanban", get(api_kanban))
        .route("/api/timeline", get(api_timeline))
        .route("/api/overview", get(api_overview))
        .route("/api/by_member", get(api_by_member))
        .route("/api/by_node", get(api_by_node))
        .route("/api/tools", get(api_tools))
        .route("/api/files", get(api_files))
        .route("/api/rows", get(api_rows))
        .route("/api/human", get(api_human))
        // Live member view (ENT-01): proxy to member's opencode via tunnel.
        .route("/api/members/{member_id}/sessions", get(api_member_sessions))
        .route("/api/members/{member_id}/session/{sid}/message", get(api_member_messages))
        .route("/api/members/{member_id}/session/{sid}/abort", post(api_member_abort))
        .route("/api/members/{member_id}/session/{sid}/prompt", post(api_member_prompt))
        .route("/api/members/{member_id}/shell", get(api_member_shells))
        .route("/api/members/{member_id}/pty", post(api_member_pty))
        .route("/ws/members/{member_id}/pty/{pty_id}", get(ws_member_pty))
        .fallback(not_found)
        .with_state(state);

    // Bind.
    let bind_str = if cmd.addr.contains(':') {
        format!("[{}]:{}", cmd.addr, cmd.port)
    } else {
        format!("{}:{}", cmd.addr, cmd.port)
    };
    let addr: SocketAddr = bind_str
        .parse()
        .map_err(|e| format!("invalid bind address {bind_str}: {e}"))?;

    eprintln!("teamx ui: activity dashboard");
    eprintln!("  open: https://{}/?token={}", addr, token);
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

struct AppState {
    conn: Arc<std::sync::Mutex<Connection>>,
    token: String,
    /// teamx server URL for member tunnel proxying (empty = live features off).
    server_url: Option<String>,
    /// Team ids owned by the current session (for live-member authorization).
    owner_teams: Vec<String>,
    /// Reusable member tunnel connections (member_id → stream).
    pool: Arc<tokio::sync::Mutex<std::collections::HashMap<String, crate::tunnel_client::TunnelStream>>>,
}

type S = Arc<AppState>;

/// The cookie name holding the auth token.
const COOKIE: &str = "teamx_ui_token";

/// Render the dashboard HTML (single page, vanilla JS, no external deps).
fn page_html() -> String {
    r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="theme-color" content="#f8f8f8">
<title>Teamx Enterprise</title>
<style>
  :root {
    --primary: #4A90E5;
    --primary-hover: #357ABD;
    --primary-light: #EBF3FC;
    --on-primary: #FFFFFF;
    --secondary: #67C23A;
    --accent: #E6A23C;
    --danger: #F56C6C;
    --on-accent: #FFFFFF;
    --bg: #f8f8f8;
    --fg: #333333;
    --card: #FFFFFF;
    --card-fg: #333333;
    --muted: #F2F6FC;
    --muted-fg: #909399;
    --border: #EBEEF5;
    --ring: #4A90E5;
    --radius: 4px;
    --shadow: 0 2px 4px 0 rgba(214, 228, 241, 0.7);
    --shadow-hover: 0 4px 12px rgba(214, 228, 241, 0.9);
    --font-sans: "Microsoft YaHei", "PingFang SC", -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    --font-mono: "SF Mono", "Fira Code", ui-monospace, SFMono-Regular, Menlo, monospace;
    --transition: 200ms ease;
  }
  .dark {
    --primary: #5B9EF0;
    --primary-hover: #7BB3F5;
    --primary-light: #1A2A3A;
    --on-primary: #FFFFFF;
    --secondary: #67C23A;
    --accent: #E6A23C;
    --danger: #F56C6C;
    --on-accent: #FFFFFF;
    --bg: #1A1A2E;
    --fg: #E0E0E0;
    --card: #22223A;
    --card-fg: #E0E0E0;
    --muted: #2A2A40;
    --muted-fg: #909399;
    --border: #333355;
    --ring: #5B9EF0;
    --shadow: 0 2px 4px 0 rgba(0, 0, 0, 0.3);
    --shadow-hover: 0 4px 12px rgba(0, 0, 0, 0.4);
  }
  * { box-sizing: border-box; }
  body { font-family: var(--font-sans); font-size: 14px; line-height: 1.5; margin: 0; background: var(--bg); color: var(--fg); -webkit-font-smoothing: antialiased; }
  header { position: sticky; top: 0; z-index: 100; display: flex; align-items: center; height: 60px; padding: 0 20px; background: var(--card); border-bottom: 1px solid var(--border); box-shadow: 0 1px 4px rgba(0,0,0,0.08); }
  header .logo { display: flex; align-items: center; gap: 10px; margin-right: 40px; flex-shrink: 0; }
  header .logo .logo-icon { width: 32px; height: 32px; background: linear-gradient(135deg, #4A90E5, #357ABD); border-radius: 6px; display: flex; align-items: center; justify-content: center; color: white; font-weight: 700; font-size: 16px; }
  header .logo-text { font-size: 16px; font-weight: 600; color: var(--primary); letter-spacing: 0.02em; }
  nav { display: flex; gap: 0; flex: 1; justify-content: center; }
  nav button { background: none; border: none; font: inherit; font-size: 14px; font-weight: 500; color: var(--muted-fg); padding: 18px 20px; border-bottom: 2px solid transparent; cursor: pointer; transition: color var(--transition), border-color var(--transition); }
  nav button:hover { color: var(--primary); }
  nav button.active { color: var(--primary); border-bottom-color: var(--primary); font-weight: 600; }
  nav button:focus-visible, button:focus-visible, select:focus-visible, input:focus-visible { outline: 2px solid var(--ring); outline-offset: 2px; }
  .header-right { margin-left: auto; display: flex; align-items: center; gap: 16px; flex-shrink: 0; }
  .header-right .user-info { display: flex; align-items: center; gap: 8px; font-size: 13px; color: #666; }
  .header-right .user-avatar { width: 30px; height: 30px; border-radius: 50%; background: var(--primary-light); color: var(--primary); display: flex; align-items: center; justify-content: center; font-weight: 600; font-size: 13px; }
  .theme-toggle { width: 34px; height: 34px; border: 1px solid var(--border); border-radius: var(--radius); background: var(--card); color: var(--fg); cursor: pointer; display: flex; align-items: center; justify-content: center; font-size: 16px; transition: all var(--transition); }
  .theme-toggle:hover { border-color: var(--primary); color: var(--primary); }
  .toolbar { display: flex; justify-content: space-between; align-items: center; gap: 14px; flex-wrap: wrap; padding: 12px 20px; background: var(--card); border-bottom: 1px solid var(--border); }
  .toolbar-left { display: flex; gap: 14px; flex-wrap: wrap; align-items: center; }
  .toolbar-right { margin-left: auto; display: flex; gap: 10px; align-items: center; }
  .timepicker { position: relative; }
  .time-btn { display: inline-flex; align-items: center; gap: 6px; padding: 6px 12px; border: 1px solid var(--border); border-radius: var(--radius); background: var(--card); color: var(--fg); font-size: 13px; cursor: pointer; transition: all var(--transition); }
  .time-btn:hover { border-color: var(--primary); color: var(--primary); }
  .tp-icon { color: var(--primary); font-size: 14px; }
  .tp-caret { color: var(--muted-fg); font-size: 10px; }
  .tp-popover { position: absolute; right: 0; top: calc(100% + 6px); width: 300px; background: var(--card); border: 1px solid var(--border); border-radius: var(--radius); box-shadow: var(--shadow-hover); z-index: 200; display: none; overflow: hidden; }
  .tp-popover.open { display: block; }
  .tp-tabs { display: flex; border-bottom: 1px solid var(--border); }
  .tp-tab { flex: 1; padding: 8px 0; border: none; background: none; color: var(--muted-fg); font-size: 13px; font-weight: 500; cursor: pointer; border-bottom: 2px solid transparent; }
  .tp-tab.active { color: var(--primary); border-bottom-color: var(--primary); }
  .tp-pane { display: none; padding: 10px; }
  .tp-pane.active { display: block; }
  .tp-quick { display: block; width: 100%; text-align: left; padding: 7px 10px; border: none; background: none; color: var(--fg); font-size: 13px; border-radius: var(--radius); cursor: pointer; transition: background var(--transition); }
  .tp-quick:hover { background: var(--muted); }
  .tp-quick.sel { color: var(--primary); font-weight: 600; background: var(--primary-light); }
  .tp-pane label { display: flex; flex-direction: column; font-size: 12px; font-weight: 500; color: var(--muted-fg); gap: 4px; margin-bottom: 10px; }
  .filters label { display: flex; flex-direction: column; font-size: 12px; font-weight: 500; color: var(--muted-fg); gap: 4px; }
  select, input { font: inherit; font-size: 13px; padding: 6px 10px; border: 1px solid var(--border); border-radius: var(--radius); background: var(--card); color: var(--fg); transition: border-color var(--transition); }
  select:hover, input:hover { border-color: var(--primary); }
  select:focus, input:focus { outline: none; border-color: var(--primary); box-shadow: 0 0 0 2px rgba(74,144,229,0.15); }
  button { font: inherit; }
  .btn { padding: 7px 14px; border: 1px solid var(--border); border-radius: var(--radius); background: var(--card); color: var(--fg); font-weight: 500; cursor: pointer; transition: all var(--transition); }
  .btn:hover { border-color: var(--primary); color: var(--primary); }
  .btn-primary { background: var(--primary); border-color: var(--primary); color: var(--on-primary); }
  .btn-primary:hover { background: var(--primary-hover); color: var(--on-primary); border-color: var(--primary-hover); }
  .container { max-width: 1280px; margin: 0 auto; padding: 20px; }
  .container.full-width { max-width: none; margin: 0; padding: 20px 24px; }
  .view { display: none; }
  .view.active { display: block; animation: fadein 200ms ease; }
  @keyframes fadein { from { opacity: 0; transform: translateY(4px); } to { opacity: 1; transform: none; } }
  .cards { display: grid; grid-template-columns: repeat(4, 1fr); gap: 16px; margin-bottom: 20px; }
  @media (max-width: 900px) { .cards { grid-template-columns: repeat(2, 1fr); } }
  .card { background: var(--card); border-radius: var(--radius); padding: 20px; box-shadow: var(--shadow); transition: box-shadow var(--transition); display: flex; align-items: center; gap: 16px; }
  .card:hover { box-shadow: var(--shadow-hover); }
  .card-icon { width: 48px; height: 48px; border-radius: 10px; display: flex; align-items: center; justify-content: center; font-size: 22px; flex-shrink: 0; }
  .card-icon.blue { background: #EBF5FF; color: #4A90E5; }
  .card-icon.green { background: #E8F8E8; color: #67C23A; }
  .card-icon.orange { background: #FDF0E0; color: #E6A23C; }
  .card-icon.purple { background: #F0E8FF; color: #9B59B6; }
  .card-icon.red { background: #FDE8E8; color: #F56C6C; }
  .card-body { flex: 1; min-width: 0; }
  .card .label { font-size: 13px; color: var(--muted-fg); margin-bottom: 4px; }
  .card .value { font-size: 24px; font-weight: 600; color: var(--fg); line-height: 1.2; }
  .card .value .unit { font-size: 12px; font-weight: 400; color: var(--muted-fg); margin-left: 2px; }
  .grid2 { display: grid; grid-template-columns: 1fr 1fr; gap: 16px; margin-top: 16px; }
  .charts-row { display: grid; grid-template-columns: 1fr 1fr; gap: 16px; margin-top: 16px; }
  @media (max-width: 900px) { .grid2 { grid-template-columns: 1fr; } .charts-row { grid-template-columns: 1fr; } }
  .chart-box { display: flex; align-items: center; justify-content: center; min-height: 140px; padding: 2px 0; }
  .chart-box svg { max-width: 150px; max-height: 150px; }
  .chart-legend { display: flex; flex-wrap: wrap; gap: 10px; justify-content: center; font-size: 12px; color: var(--muted-fg); margin-top: 2px; }
  .chart-legend .sw { display: inline-block; width: 10px; height: 10px; border-radius: 2px; margin-right: 5px; vertical-align: -1px; }
  .hbar-row { display: flex; align-items: center; gap: 10px; margin-bottom: 9px; }
  .hbar-label { width: 92px; flex-shrink: 0; font-size: 12px; color: var(--fg); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; text-align: right; }
  .hbar-track { flex: 1; height: 16px; background: var(--muted); border-radius: 3px; overflow: hidden; }
  .hbar-fill { height: 100%; border-radius: 3px; min-width: 2px; transition: width 400ms ease; }
  .hbar-val { font-size: 11.5px; color: var(--muted-fg); width: 74px; flex-shrink: 0; font-family: var(--font-mono); }
  .hbar-title { font-size: 12px; margin-bottom: 8px; color: #666; font-weight: 500; }
  section.panel { background: var(--card); border-radius: var(--radius); padding: 16px; margin-bottom: 16px; box-shadow: var(--shadow); }
  h2 { font-size: 14px; font-weight: 600; margin: 0 0 12px; color: var(--fg); }
  h2 .count { font-family: var(--font-mono); font-weight: 500; color: var(--muted-fg); margin-left: 6px; }
  table { width: 100%; border-collapse: collapse; font-size: 13px; }
  th, td { text-align: left; padding: 10px 12px; border-bottom: 1px solid var(--border); vertical-align: middle; }
  tbody tr { transition: background var(--transition); }
  tbody tr:hover { background: var(--muted); }
  th { color: var(--muted-fg); font-weight: 600; font-size: 12px; background: var(--muted); }
  .badge { display: inline-block; padding: 2px 8px; border-radius: 3px; font-size: 12px; font-weight: 500; line-height: 1.5; }
  .badge.proposed, .badge.blocked { background: #FDF6EC; color: #E6A23C; }
  .badge.shared, .badge.in_progress, .badge.active { background: #EBF5FF; color: #4A90E5; }
  .badge.achieved, .badge.completed, .badge.closed { background: #E8F8E8; color: #67C23A; }
  .badge.refining { background: #F0E8FF; color: #9B59B6; }
  .badge.owner { background: #EBF5FF; color: #4A90E5; }
  .badge.contributor { background: #E8F8E8; color: #67C23A; }
  .badge.reviewer { background: #F0E8FF; color: #9B59B6; }
  .badge.observer { background: #FDF0E0; color: #E6A23C; }
  .badge.idle, .badge.pending { background: #F2F6FC; color: #909399; }
  .badge.waiting { background: #FDF6EC; color: #E6A23C; }
  .badge.left { background: #FDE8E8; color: #F56C6C; }
  .detail { font-family: var(--font-mono); font-size: 11px; color: var(--muted-fg); white-space: pre-wrap; word-break: break-all; max-width: 480px; }
  .kanban { display: grid; grid-template-columns: repeat(4, 1fr); gap: 16px; }
  @media (max-width: 1100px) { .kanban { grid-template-columns: repeat(2, 1fr); } }
  @media (max-width: 600px) { .kanban { grid-template-columns: 1fr; } }
  .kb-col { background: var(--muted); border: 1px solid var(--border); border-radius: var(--radius); padding: 12px; min-height: 200px; }
  .kb-col-head { padding: 0 0 10px; border-bottom: 2px solid var(--border); margin-bottom: 10px; }
  .kb-col-title { font-size: 13px; font-weight: 600; color: var(--fg); display: flex; align-items: center; gap: 6px; }
  .kb-col-meta { font-size: 11px; color: var(--muted-fg); margin-top: 2px; }
  .kb-empty { padding: 16px; text-align: center; color: var(--muted-fg); font-size: 13px; }
  .kb-card { background: var(--card); border: 1px solid var(--border); border-radius: var(--radius); padding: 10px 12px; margin-bottom: 8px; font-size: 12px; box-shadow: 0 1px 2px rgba(0,0,0,0.04); transition: box-shadow var(--transition); }
  .kb-card:hover { box-shadow: var(--shadow-hover); }
  .kb-head { display: flex; justify-content: space-between; align-items: center; gap: 6px; margin-bottom: 5px; }
  .kb-who { font-weight: 600; font-size: 12px; color: var(--fg); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .kb-body { font-size: 12px; color: var(--fg); line-height: 1.45; word-break: break-word; margin-bottom: 5px; }
  .kb-body .tool-name { font-weight: 600; color: var(--primary); }
  .kb-body .cost { color: var(--accent); font-weight: 600; font-family: var(--font-mono); }
  .kb-meta { font-size: 11px; color: var(--muted-fg); font-family: var(--font-mono); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .kb-meta .cost, .kb-col-meta .cost { color: var(--accent); font-weight: 600; font-family: var(--font-mono); }
  .kb-card { position: relative; }
  .kb-tip { display: none; position: absolute; bottom: calc(100% + 10px); left: 50%; transform: translateX(-50%); z-index: 400; min-width: 320px; max-width: 460px; background: #333; color: #fff; border-radius: var(--radius); padding: 12px 14px; box-shadow: 0 4px 16px rgba(0,0,0,0.2); font-size: 13px; line-height: 1.6; }
  .kb-tip .tip-title { font-weight: 600; margin-bottom: 6px; display: flex; justify-content: space-between; align-items: center; gap: 8px; font-size: 13px; }
  .kb-desc { font-size: 13px; font-weight: 500; color: #fff; background: rgba(255,255,255,0.1); border-radius: var(--radius); padding: 8px 10px; margin-bottom: 6px; }
  .kb-tip table { border-collapse: collapse; font-size: 12px; }
  .kb-tip td { padding: 2px 8px 2px 0; vertical-align: top; }
  .kb-tip td.k { color: #aaa; white-space: nowrap; font-weight: 500; }
  .kb-tip td.v { word-break: break-all; font-family: var(--font-mono); font-size: 11px; }
  .kb-card:hover .kb-tip, .kb-card:focus .kb-tip { display: block; }
  .gantt { background: var(--card); border-radius: var(--radius); padding: 14px; }
  .gantt-svg { display: block; width: 100%; height: auto; }
  .gantt-legend { display: flex; gap: 14px; flex-wrap: wrap; font-size: 11.5px; color: var(--muted-fg); margin-bottom: 10px; }
  .gantt-legend .sw { width: 12px; height: 8px; border-radius: 2px; display: inline-block; margin-right: 5px; vertical-align: 0; }
  .gantt-row { display: flex; align-items: center; height: 40px; border-bottom: 1px solid var(--border); }
  .gantt-label { width: 150px; flex-shrink: 0; font-size: 12px; font-weight: 500; padding: 0 10px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--fg); }
  .gantt-track { position: relative; flex: 1; height: 100%; min-width: 420px; }
  .gantt-gridline { position: absolute; top: 0; bottom: 0; width: 1px; background: var(--border); opacity: 0.5; pointer-events: none; }
  .gantt-bar { position: absolute; height: 20px; top: 10px; border-radius: 3px; opacity: 0.92; min-width: 2px; cursor: pointer; transition: opacity var(--transition), transform var(--transition); box-shadow: 0 1px 2px rgba(0,0,0,0.1); }
  .gantt-bar:hover { opacity: 1; transform: translateY(-1px); z-index: 5; }
  .gantt-bar.work { background: linear-gradient(90deg, var(--primary), #6BB3F0); }
  .gantt-bar.tool { background: #9B59B6; }
  .gantt-bar.step { background: #E6A23C; }
  .gantt-bar.human { background: #67C23A; }
  .gantt-point { position: absolute; top: 14px; width: 12px; height: 12px; border-radius: 50%; margin-left: -6px; cursor: pointer; transition: transform var(--transition); border: 2px solid var(--card); box-shadow: 0 0 0 1px var(--border); z-index: 2; }
  .gantt-point:hover { transform: scale(1.7); z-index: 6; }
  .gantt-point.tool { background: #9B59B6; }
  .gantt-point.step { background: #E6A23C; }
  .gantt-point.human { background: #67C23A; }
  .gantt-tip { display: none; position: absolute; bottom: calc(100% + 8px); left: 50%; transform: translateX(-50%); z-index: 400; background: #333; color: #fff; border-radius: var(--radius); padding: 8px 10px; box-shadow: 0 2px 8px rgba(0,0,0,0.2); font-size: 11px; line-height: 1.55; white-space: nowrap; max-width: 340px; }
  .gantt-tip .mono { font-family: var(--font-mono); }
  .gantt-bar:hover .gantt-tip, .gantt-point:hover .gantt-tip, .gantt-bar:focus .gantt-tip, .gantt-point:focus .gantt-tip { display: block; }
  .gantt-axis { position: relative; height: 22px; margin-left: 150px; font-size: 10.5px; color: var(--muted-fg); font-family: var(--font-mono); margin-bottom: 4px; }
  .gantt-axis span { position: absolute; transform: translateX(-50%); white-space: nowrap; }
  .view-toggle { display: flex; border: 1px solid var(--border); border-radius: var(--radius); overflow: hidden; }
  .view-toggle button { background: var(--card); border: none; padding: 6px 14px; font-size: 13px; color: var(--muted-fg); cursor: pointer; transition: all var(--transition); }
  .view-toggle button + button { border-left: 1px solid var(--border); }
  .view-toggle button.active { background: var(--primary); color: var(--on-primary); font-weight: 600; }
  /* ---------- fishbone timeline (vertical) ---------- */
  .fishbone { background: var(--card); border-radius: var(--radius); padding: 18px 14px 8px; }
  .fb-wrap { position: relative; }
  .fb-spine { position: absolute; left: 50%; top: 0; bottom: 0; width: 2px; background: var(--border); transform: translateX(-1px); }
  .fb-start { display: flex; justify-content: center; position: relative; z-index: 2; }
  .fb-start-box { display: inline-flex; align-items: center; gap: 9px; padding: 7px 18px; border: 1.5px solid var(--primary); border-radius: 6px; background: var(--card); color: var(--primary); font-size: 12.5px; font-weight: 600; box-shadow: var(--shadow); letter-spacing: 0.03em; }
  .fb-start-box:hover { box-shadow: var(--shadow-hover); }
  .fb-start-dot { width: 10px; height: 10px; background: var(--primary); border-radius: 2px; flex-shrink: 0; }
  .fb-day { text-align: center; padding: 14px 0 8px; font-size: 11.5px; font-weight: 600; color: var(--muted-fg); font-family: var(--font-mono); letter-spacing: 0.05em; }
  .fb-day:first-child { padding-top: 0; }
  .fb-day::before { content: "— "; color: var(--border); }
  .fb-day::after { content: " —"; color: var(--border); }
  .fb-row { position: relative; display: flex; align-items: center; min-height: 34px; padding: 3px 0; }
  .fb-side { flex: 1; min-width: 0; position: relative; display: flex; }
  .fb-center { flex: 0 0 30px; position: relative; display: flex; align-items: center; justify-content: center; }
  .fb-row.left .fb-side:first-child { justify-content: flex-end; }
  .fb-card { position: relative; background: var(--card); border: 1px solid var(--border); border-radius: 6px; padding: 8px 10px; box-shadow: var(--shadow); max-width: 430px; transition: box-shadow var(--transition), border-color var(--transition); }
  .fb-card:hover { box-shadow: var(--shadow-hover); border-color: var(--primary); }
  .fb-row.left .fb-card { margin-right: 10px; }
  .fb-row.right .fb-card { margin-left: 10px; }
  .fb-row.left .fb-card::after { content: ""; position: absolute; left: 100%; top: 50%; width: 25px; height: 2px; background: var(--border); transform: translateY(-50%); }
  .fb-row.right .fb-card::after { content: ""; position: absolute; right: 100%; top: 50%; width: 25px; height: 2px; background: var(--border); transform: translateY(-50%); }
  .fb-dot { width: 14px; height: 14px; border-radius: 50%; background: var(--muted-fg); border: 2px solid var(--card); box-shadow: 0 0 0 2px var(--border); z-index: 2; }
  .fb-top { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; margin-bottom: 2px; }
  .fb-time { font-family: var(--font-mono); font-size: 11px; color: var(--muted-fg); white-space: nowrap; }
  .fb-who { font-size: 12px; font-weight: 600; color: var(--fg); }
  .fb-kind { display: inline-block; padding: 0 6px; border-radius: 3px; font-size: 10.5px; font-weight: 600; color: #fff; line-height: 1.6; }
  .fb-text { font-size: 13px; color: var(--fg); line-height: 1.45; word-break: break-word; }
  .fb-meta { display: flex; gap: 10px; flex-wrap: wrap; font-size: 11px; color: var(--muted-fg); font-family: var(--font-mono); margin-top: 3px; }
  .fb-meta .human { color: #1a7f37; font-weight: 600; }
  .fb-cost { color: var(--accent); font-weight: 600; }
  /* ---------- horizontal fishbone (goal activity timeline) ---------- */
  .hfb-wrap { position: relative; height: 560px; }
  .hfb-spine { position: absolute; left: 4%; right: 4%; top: 50%; height: 2px; background: var(--border); }
  .hfb-node { position: absolute; top: 50%; transform: translate(-50%, -50%); z-index: 2; }
  .hfb-dot { position: absolute; left: 50%; top: 50%; transform: translate(-50%, -50%); width: 13px; height: 13px; border-radius: 50%; border: 2px solid var(--card); box-shadow: 0 0 0 2px var(--border); }
  .hfb-card { position: absolute; left: 50%; transform: translateX(-50%); width: 172px; background: var(--card); border: 1px solid var(--border); border-radius: 6px; padding: 7px 9px; box-shadow: var(--shadow); transition: box-shadow var(--transition), border-color var(--transition); }
  .hfb-card:hover { box-shadow: var(--shadow-hover); border-color: var(--primary); }
  .hfb-node.top-near .hfb-card { bottom: 20px; }
  .hfb-node.top-far .hfb-card { bottom: 180px; }
  .hfb-node.bottom-near .hfb-card { top: 20px; }
  .hfb-node.bottom-far .hfb-card { top: 180px; }
  .hfb-node.top-near .hfb-card::after, .hfb-node.top-far .hfb-card::after { content: ""; position: absolute; left: 50%; top: 100%; transform: translateX(-50%); width: 2px; background: var(--border); }
  .hfb-node.bottom-near .hfb-card::after, .hfb-node.bottom-far .hfb-card::after { content: ""; position: absolute; left: 50%; bottom: 100%; transform: translateX(-50%); width: 2px; background: var(--border); }
  .hfb-node.top-near .hfb-card::after, .hfb-node.bottom-near .hfb-card::after { height: 18px; }
  .hfb-node.top-far .hfb-card::after, .hfb-node.bottom-far .hfb-card::after { height: 178px; }
  .hfb-ruler { display: flex; justify-content: space-between; margin: 6px 4% 0; font-family: var(--font-mono); font-size: 10.5px; color: var(--muted-fg); }
  .hfb-cost { color: var(--accent); font-weight: 600; }
  .lifecycle { position: relative; background: var(--card); border-radius: var(--radius); padding: 20px; margin-bottom: 16px; box-shadow: var(--shadow); }
  .lc-track { display: flex; align-items: flex-start; position: relative; }
  .lc-step { flex: 1; text-align: center; position: relative; min-width: 90px; z-index: 1; }
  .lc-step::before { content: ""; position: absolute; top: 7px; left: -50%; width: 100%; height: 3px; background: var(--border); z-index: 0; }
  .lc-step:first-child::before { display: none; }
  .lc-step.done::before, .lc-step.current::before { background: linear-gradient(90deg, var(--primary), #6BB3F0); }
  .lc-step .dot { width: 15px; height: 15px; border-radius: 50%; margin: 0 auto 8px; background: var(--card); border: 3px solid var(--border); position: relative; z-index: 2; box-sizing: border-box; transition: all 200ms ease; }
  .lc-step.done .dot { background: var(--primary); border-color: var(--primary); }
  .lc-step.current .dot { background: var(--accent); border-color: var(--accent); box-shadow: 0 0 0 4px rgba(230,162,60,0.2); }
  .lc-step .name { font-size: 12px; font-weight: 500; color: var(--muted-fg); }
  .lc-step.done .name { color: var(--fg); }
  .lc-step.current .name { color: var(--accent); font-weight: 600; }
  .lc-step .date { font-size: 11px; color: var(--muted-fg); margin-top: 3px; font-family: var(--font-mono); }
  .goal-hero { background: var(--card); border-radius: var(--radius); padding: 20px; margin-bottom: 16px; box-shadow: var(--shadow); border-left: 4px solid var(--primary); }
  .goal-hero h2 { font-size: 18px; margin: 0 0 8px; font-weight: 600; }
  .goal-body { color: var(--muted-fg); margin: 0 0 12px; max-width: 760px; font-size: 14px; line-height: 1.6; }
  .muted { color: var(--muted-fg); font-size: 12px; }
  .mono { font-family: var(--font-mono); }
  .goal-row { display: flex; justify-content: space-between; align-items: center; gap: 12px; padding: 10px 12px; border-bottom: 1px solid var(--border); border-radius: var(--radius); transition: background var(--transition); }
  .goal-row:last-child { border-bottom: none; }
  .goal-row:hover { background: var(--muted); }
  .goal-row.current { background: var(--primary-light); }
  .goal-row-title { font-weight: 500; font-size: 13px; display: flex; align-items: center; gap: 6px; flex-wrap: wrap; }
  .goal-row-meta { color: var(--muted-fg); font-size: 12px; white-space: nowrap; }
  @media (prefers-reduced-motion: reduce) {
    * { animation: none !important; transition: none !important; }
  }
  .status-dot { display: inline-block; width: 8px; height: 8px; border-radius: 50%; margin-right: 6px; }
  .status-dot.online { background: var(--secondary); }
  .status-dot.busy { background: var(--accent); }
  .status-dot.offline { background: #C0C4CC; }
  .tag { display: inline-block; padding: 1px 8px; border-radius: 10px; font-size: 11px; font-weight: 500; }
  .tag.blue { background: var(--primary-light); color: var(--primary); }
  .tag.green { background: #E8F8E8; color: var(--secondary); }
  .tag.orange { background: #FDF0E0; color: var(--accent); }
  .tag.red { background: #FDE8E8; color: var(--danger); }
  .tag.gray { background: var(--muted); color: var(--muted-fg); }
  .mono { font-family: var(--font-mono); font-size: 12px; }
  .layout { display: grid; grid-template-columns: 300px 1fr; gap: 16px; align-items: start; }
  @media (max-width: 900px) { .layout { grid-template-columns: 1fr; } }
  /* ---------- member list ---------- */
  .member-item { display: flex; align-items: center; gap: 10px; padding: 12px; border: 1px solid var(--border); border-radius: var(--radius); margin-bottom: 8px; cursor: pointer; background: var(--card); transition: all var(--transition); }
  .member-item:hover { border-color: var(--primary); box-shadow: var(--shadow-hover); }
  .member-item.selected { border-color: var(--primary); background: var(--primary-light); box-shadow: var(--shadow-hover); }
  .member-item .mi-avatar { width: 38px; height: 38px; border-radius: 50%; display: flex; align-items: center; justify-content: center; font-size: 16px; flex-shrink: 0; background: var(--muted); color: var(--muted-fg); font-weight: 600; }
  .member-item.selected .mi-avatar { background: var(--primary); color: white; }
  .member-item .mi-main { flex: 1; min-width: 0; }
  .member-item .mi-name { font-size: 14px; font-weight: 600; display: flex; align-items: center; gap: 6px; }
  .member-item .mi-sub { font-size: 12px; color: var(--muted-fg); margin-top: 2px; }
  .member-item .mi-sub .cost { color: var(--accent); font-weight: 600; font-family: var(--font-mono); }
  .member-item .mi-live { flex-shrink: 0; display: flex; align-items: center; gap: 6px; }
  .live-badge { display: inline-block; padding: 1px 8px; border-radius: 10px; font-size: 11px; background: var(--danger); color: white; animation: pulse 1.5s infinite; }
  @keyframes pulse { 0%,100% { opacity: 1; } 50% { opacity: 0.4; } }
  /* ---------- session list ---------- */
  .session-item { display: flex; align-items: center; gap: 10px; padding: 10px 12px; border: 1px solid var(--border); border-radius: var(--radius); margin-bottom: 8px; cursor: pointer; background: var(--card); transition: all var(--transition); }
  .session-item:hover { border-color: var(--primary); box-shadow: var(--shadow-hover); }
  .session-item.active { border-color: var(--primary); background: var(--primary-light); }
  .session-item .si-icon { width: 34px; height: 34px; border-radius: 8px; display: flex; align-items: center; justify-content: center; font-size: 16px; flex-shrink: 0; background: var(--muted); color: var(--muted-fg); }
  .session-item.active .si-icon { background: var(--primary); color: white; }
  .session-item .si-main { flex: 1; min-width: 0; }
  .session-item .si-title { font-size: 13px; font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .session-item .si-sub { font-size: 12px; color: var(--muted-fg); display: flex; gap: 8px; margin-top: 2px; }
  .session-item .si-time { font-size: 11px; color: var(--muted-fg); font-family: var(--font-mono); flex-shrink: 0; }
  /* ---------- detail pane ---------- */
  .detail-pane { background: var(--card); border-radius: var(--radius); box-shadow: var(--shadow); overflow: hidden; }
  .detail-header { display: flex; align-items: center; gap: 12px; padding: 14px 16px; border-bottom: 1px solid var(--border); flex-wrap: wrap; }
  .detail-header .dh-title { font-size: 14px; font-weight: 600; flex: 1; min-width: 0; }
  .detail-tabs { display: flex; gap: 0; border-bottom: 1px solid var(--border); }
  .detail-tabs button { background: none; border: none; padding: 10px 16px; font-size: 13px; color: var(--muted-fg); border-bottom: 2px solid transparent; cursor: pointer; font-weight: 500; }
  .detail-tabs button.active { color: var(--primary); border-bottom-color: var(--primary); }
  .detail-body { padding: 16px; min-height: 400px; max-height: 600px; overflow-y: auto; }
  /* ---------- live event stream ---------- */
  .live-line { display: flex; gap: 10px; padding: 6px 0; border-bottom: 1px dashed var(--border); font-size: 13px; }
  .live-line .ll-time { font-family: var(--font-mono); font-size: 11px; color: var(--muted-fg); flex-shrink: 0; width: 60px; }
  .live-line .ll-icon { flex-shrink: 0; width: 20px; text-align: center; }
  .live-line .ll-text { flex: 1; min-width: 0; word-break: break-all; }
  .live-line .ll-text .tool-name { color: var(--primary); font-weight: 600; }
  .live-line .ll-text .file { color: var(--accent); font-family: var(--font-mono); font-size: 12px; }
  /* ---------- terminal placeholder ---------- */
  .term-window { background: #1e1e1e; border-radius: 6px; overflow: hidden; box-shadow: var(--shadow-hover); }
  .term-titlebar { display: flex; align-items: center; gap: 6px; padding: 8px 12px; background: #2d2d2d; color: #ccc; font-size: 12px; }
  .term-dot { width: 10px; height: 10px; border-radius: 50%; }
  .term-body { padding: 12px; font-family: var(--font-mono); font-size: 12.5px; color: #d4d4d4; min-height: 300px; white-space: pre-wrap; }
  .term-body .cmd { color: #9CDCFE; }
  .term-cursor { display: inline-block; width: 7px; height: 14px; background: #d4d4d4; animation: blink 1s step-end infinite; vertical-align: text-bottom; }
  @keyframes blink { 50% { opacity: 0; } }
  .term-max-btn { margin-left: 8px; background: none; border: 1px solid #555; color: #ccc; border-radius: 3px; cursor: pointer; font-size: 11px; padding: 1px 6px; }
  .term-max-btn:hover { border-color: #888; color: #fff; }
  .term-backdrop { display: none; position: fixed; inset: 0; background: rgba(0,0,0,0.55); z-index: 900; }
  .term-backdrop.open { display: block; }
  .term-window.maximized { position: fixed; inset: 24px; z-index: 901; border-radius: 8px; display: flex; flex-direction: column; }
  .term-window.maximized .term-body { flex: 1; overflow: auto; }

  .mlive-layout { display: grid; grid-template-columns: 300px 1fr; gap: 16px; align-items: start; }
  @media (max-width: 900px) { .mlive-layout { grid-template-columns: 1fr; } }
  .hint { font-size: 12px; color: var(--muted-fg); padding: 0 4px 8px; }

</style>
</head>
<body>
<header>
  <div class="logo">
    <div class="logo-icon">Tx</div>
    <span class="logo-text">Teamx Enterprise</span>
  </div>
  <nav>
    <button data-view="kanban" class="active">KanBan</button>
    <button data-view="goal">Goal</button>
    <button data-view="cost">Cost</button>
    <button data-view="timeline">Timeline</button>
    <button data-view="members">Members</button>
  </nav>
  <div class="header-right">
    <button class="theme-toggle" id="themeToggle" title="Toggle light/dark mode">☀️</button>
    <div class="user-info">
      <div class="user-avatar">O</div>
      <span>Owner</span>
    </div>
  </div>
</header>
<div class="toolbar">
  <div class="toolbar-left">
    <label>Team <select id="team"></select></label>
    <label>Goal <select id="goal"><option value="">All</option></select></label>
  </div>
  <div class="toolbar-right">
    <div class="view-toggle" id="tlViewToggle" style="display:none">
      <button type="button" data-tlmode="gantt">Gantt</button>
      <button type="button" data-tlmode="fishbone" class="active">Fishbone</button>
    </div>
    <div class="timepicker">
      <button class="btn time-btn" id="timeBtn" title="Select time range">
        <span class="tp-icon">◷</span>
        <span id="timeLabel">Last 30 days</span>
        <span class="tp-caret">▾</span>
      </button>
      <div class="tp-popover" id="timePopover">
        <div class="tp-tabs">
          <button class="tp-tab active" data-tp="quick">Quick</button>
          <button class="tp-tab" data-tp="abs">Absolute</button>
        </div>
        <div class="tp-pane active" data-tp-pane="quick">
          <button class="tp-quick" data-from="now-15m">Last 15 minutes</button>
          <button class="tp-quick" data-from="now-1h">Last 1 hour</button>
          <button class="tp-quick" data-from="now-24h">Last 24 hours</button>
          <button class="tp-quick" data-from="now-7d">Last 7 days</button>
          <button class="tp-quick" data-from="now-14d">Last 14 days</button>
          <button class="tp-quick" data-from="now-30d" class="sel">Last 30 days</button>
          <button class="tp-quick" data-from="now-90d">Last 90 days</button>
          <button class="tp-quick" data-from="start">All time</button>
        </div>
        <div class="tp-pane" data-tp-pane="abs">
          <label>From <input type="datetime-local" id="absFrom"></label>
          <label>To <input type="datetime-local" id="absTo"></label>
          <button class="btn" id="absApply">Apply</button>
        </div>
      </div>
    </div>
  </div>
</div>

<div class="container">
  <!-- ============ KanBan view (default) ============ -->
  <div class="view active" id="view-kanban">
    <div class="kanban" id="kanbanCols"></div>
  </div>

  <!-- ============ Goal view ============ -->
  <div class="view" id="view-goal">
    <div id="goalHero"></div>
    <div id="goalLifecycle"></div>
    <section class="panel"><h2>Activity timeline (this goal)</h2><div class="gantt" id="goalGantt"></div></section>
    <div id="goalHistory" style="background:var(--card);border:1px solid var(--border);border-radius:var(--radius);padding:16px 18px;margin-bottom:20px;box-shadow:var(--shadow)"></div>
    <div class="cards" id="goalCards"></div>
    <div class="grid2">
      <section class="panel"><h2>By member</h2><table id="gByMember"><thead><tr><th>Member</th><th>Work time</th><th>Tokens (in/out)</th><th>Cost</th><th>Actions</th></tr></thead><tbody></tbody></table></section>
      <section class="panel"><h2>By node</h2><table id="gByNode"><thead><tr><th>Node</th><th>Work time</th><th>Tokens</th><th>Cost</th><th>Actions</th></tr></thead><tbody></tbody></table></section>
    </div>
    <section class="panel"><h2>Goal timeline (recent events)</h2><table id="goalEvents"><thead><tr><th>When</th><th>Event</th><th>Member</th><th>Details</th></tr></thead><tbody></tbody></table></section>
  </div>

  <!-- ============ Cost view ============ -->
  <div class="view" id="view-cost">
    <div class="cards" id="costCards"></div>
    <section class="panel" id="costBreakdownPanel" style="display:none"><h2>Cost breakdown</h2><div id="costBreakdown"></div></section>
    <div class="charts-row">
      <section class="panel"><h2>Cost by member</h2><div id="chartCostDonut" class="chart-box"></div><div id="chartCostDonutLegend" class="chart-legend"></div></section>
      <section class="panel"><h2>Cost by node</h2><div id="chartCostBars" class="chart-box"></div></section>
    </div>
    <div class="charts-row">
      <section class="panel"><h2>Tokens by member</h2><div id="chartTokensBars" class="chart-box"></div></section>
      <section class="panel"><h2>Human vs AI actions</h2><div id="chartHumanDonut" class="chart-box"></div><div id="chartHumanDonutLegend" class="chart-legend"></div></section>
    </div>
    <div class="grid2">
      <section class="panel"><h2>Cost by member</h2><table id="costByMember"><thead><tr><th>Member</th><th>In tokens</th><th>Out tokens</th><th>Cost</th><th>Actions</th></tr></thead><tbody></tbody></table></section>
      <section class="panel"><h2>Cost by node</h2><table id="costByNode"><thead><tr><th>Node</th><th>In tokens</th><th>Out tokens</th><th>Cost</th><th>Actions</th></tr></thead><tbody></tbody></table></section>
    </div>
    <div class="grid2">
      <section class="panel"><h2>Tool usage</h2><table id="costTools"><thead><tr><th>Tool</th><th>Count</th></tr></thead><tbody></tbody></table></section>
      <section class="panel"><h2>Files edited</h2><table id="costFiles"><thead><tr><th>File</th><th>Count</th></tr></thead><tbody></tbody></table></section>
    </div>
  </div>

  <!-- ============ Timeline view (Gantt / fishbone toggle) ============ -->
  <div class="view" id="view-timeline">
    <div class="gantt" id="gantt" style="display:none"></div>
    <div class="fishbone" id="fishbone"></div>
  </div>

  <!-- ============ Members view (live member sessions, ENT-01) ============ -->
  <div class="view" id="view-members">
    <div style="display:flex;align-items:center;justify-content:space-between;flex-wrap:wrap;gap:8px;margin-bottom:12px">
      <div class="hint" style="padding:0">在线成员 / 活跃会话 / 正在执行工具为最近时间窗口的活动聚合；打开的终端为实时状态</div>
      <div class="view-toggle" id="mliveWindowToggle">
        <button type="button" data-win="300000">5 分钟</button>
        <button type="button" data-win="600000" class="active">10 分钟</button>
        <button type="button" data-win="1800000">30 分钟</button>
        <button type="button" data-win="3600000">1 小时</button>
      </div>
    </div>
    <div class="cards" id="mliveCards"></div>
    <div class="mlive-layout">
      <div>
        <section class="panel"><h2>Members <span class="count" id="mliveMemberCount">0</span></h2>
          <div class="hint">点击成员查看其实时会话</div>
          <div id="mliveMemberList"></div>
        </section>
      </div>
      <div class="detail-pane">
        <div id="mliveEmpty">
          <div class="empty"><div class="big">👈</div>
            <div>选择左侧成员，查看其 opencode 会话与实时活动</div>
            <div class="muted mt16" style="font-size:12px">实时查看成员正在做什么：会话列表、工具调用、消息流、终端</div>
          </div>
        </div>
        <div id="mliveDetail" style="display:none">
          <div class="detail-header">
            <div class="member-chip" id="mliveMemberName" style="display:inline-flex;align-items:center;gap:6px;padding:3px 10px;border-radius:12px;background:var(--muted)"></div>
            <div class="dh-title" id="mliveSessionTitle">—</div>
            <span class="tag blue" id="mliveRunningTag" style="display:none">running</span>
            <span class="tag orange" id="mliveCostTag" style="display:none"></span>
            <button class="btn btn-sm" id="mliveInterruptBtn" style="margin-left:auto;display:none">⏸ 中断</button>
            <button class="btn btn-sm" id="mliveSendBtn" style="display:none">📋 发送消息</button>
            <button class="btn btn-primary btn-sm" id="mliveTermBtn">⌨ 打开终端</button>
          </div>
          <div class="detail-tabs">
            <button class="active" data-mtab="live">实时活动</button>
            <button data-mtab="sessions">会话</button>
            <button data-mtab="messages">消息</button>
            <button data-mtab="terminal">终端</button>
          </div>
          <div class="detail-body">
            <div id="mtab-live"></div>
            <div id="mtab-sessions" style="display:none"></div>
            <div id="mtab-messages" style="display:none"></div>
            <div id="mtab-terminal" style="display:none">
              <div id="mliveTerm" class="muted">点击右上角"打开终端"或选择下方终端标签</div>
            </div>
          </div>
        </div>
      </div>
    </div>
  </div>
</div>
<script>
// ---- theme toggle ----
const html = document.documentElement
const THEME_KEY = "teamx-theme"
function applyTheme(dark) {
  html.classList.toggle("dark", dark)
  const btn = document.getElementById("themeToggle")
  if (btn) btn.textContent = dark ? "🌙" : "☀️"
  document.querySelector('meta[name="theme-color"]')?.setAttribute("content", dark ? "#1A1A2E" : "#f8f8f8")
}
function initTheme() {
  const saved = localStorage.getItem(THEME_KEY)
  if (saved !== null) { applyTheme(saved === "dark"); return }
  applyTheme(window.matchMedia("(prefers-color-scheme: dark)").matches)
}
initTheme()
document.getElementById("themeToggle")?.addEventListener("click", () => {
  const dark = !html.classList.contains("dark")
  applyTheme(dark)
  localStorage.setItem(THEME_KEY, dark ? "dark" : "light")
})
window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", (e) => {
  if (localStorage.getItem(THEME_KEY) === null) applyTheme(e.matches)
})
const $ = (s) => document.querySelector(s)
const $$ = (s) => Array.from(document.querySelectorAll(s))
const KINDS = ["tool_call","step_finish","command","file_edit","work_session","human_input","human_approval","human_command"]
const KIND_COLOR = { tool_call: "#8250df", step_finish: "#bf8700", command: "#8250df", file_edit: "#bf8700", work_session: "#0969da", human_input: "#1a7f37", human_approval: "#1a7f37", human_command: "#1a7f37" }
const GOAL_STATES = ["proposed", "shared", "refining", "in_progress", "blocked", "achieved", "closed"]
function fmtMs(ms) {
  if (ms == null) return "—"
  const h = Math.floor(ms / 3600000), m = Math.round((ms % 3600000) / 60000)
  if (h > 0) return h + "h" + (m > 0 ? m + "m" : "")
  if (m > 0) return m + "m"
  return Math.round(ms / 1000) + "s"
}
function fmtNum(v) { return v == null ? "—" : Number(v).toLocaleString() }
function fmtCost(v) { return v == null ? "—" : "$" + Number(v).toFixed(4) }
function fmtDate(iso) { if (!iso) return "—"; const d = new Date(iso); return d.toLocaleString() }
function esc(s) { const d = document.createElement("div"); d.textContent = s ?? ""; return d.innerHTML }
function badge(text) { return `<span class="badge ${esc(text)}">${esc(text)}</span>` }
// ---- global time range state (Kibana-style) ----
const timeState = { type: "quick", from: "now-30d", label: "Last 30 days" }

function computeRange() {
  const now = new Date()
  if (timeState.type === "quick") {
    const f = timeState.from
    if (f === "start") return { from: null, to: now.toISOString() }
    const m = f.match(/^now-(\d+)([mhd])$/)
    if (m) {
      const n = parseInt(m[1]), unit = m[2]
      const ms = unit === "m" ? n * 60000 : unit === "h" ? n * 3600000 : n * 86400000
      return { from: new Date(now.getTime() - ms).toISOString(), to: now.toISOString() }
    }
    return { from: null, to: now.toISOString() }
  }
  // absolute
  return {
    from: timeState.absFrom ? new Date(timeState.absFrom).toISOString() : null,
    to: timeState.absTo ? new Date(timeState.absTo).toISOString() : null,
  }
}

function qs() {
  const p = new URLSearchParams({ team: $("#team").value })
  const r = computeRange()
  if (r.from) p.set("from", r.from)
  if (r.to) p.set("to", r.to)
  return p.toString()
}
async function jget(path) {
  const r = await fetch(path, { headers: { Accept: "application/json" } })
  if (!r.ok) throw new Error("HTTP " + r.status + " " + (await r.text()).slice(0, 120))
  return r.json()
}
function row(o) { return o != null && typeof o === "object" ? JSON.stringify(o) : "—" }
function tableRows(rows, cols) {
  if (!rows || rows.length === 0) return '<tr><td colspan="10" class="muted">no data</td></tr>'
  return rows.map((r) => `<tr>${cols.map((c) => `<td>${c(r)}</td>`).join("")}</tr>`).join("")
}

// ---- navigation ----
function showView(name) {
  $$(".view").forEach((v) => v.classList.toggle("active", v.id === "view-" + name))
  $$("nav button").forEach((b) => b.classList.toggle("active", b.dataset.view === name))
  $(".container").classList.toggle("full-width", name === "timeline")
  $("#tlViewToggle").style.display = name === "timeline" ? "" : "none"
  if (name === "timeline") renderTimeline()
  if (name === "cost") renderCost()
  if (name === "kanban") renderKanban()
  if (name === "members") renderMembers()
  if (name === "goal") renderGoal()
}
$$("nav button").forEach((b) => b.addEventListener("click", () => showView(b.dataset.view)))

// ---- Goal view ----
let selectedGoalId = null

// Fill the toolbar goal dropdown (current + history). Called after /api/goal.
function fillGoalSelect(goals, currentId) {
  const sel = document.getElementById("goal")
  if (!sel) return
  const list = (goals ?? []).slice().reverse() // newest first
  const val = selectedGoalId || currentId || ""
  sel.innerHTML = `<option value="">(current${currentId ? "" : " — none"})</option>` +
    list.map((g) => `<option value="${esc(g.id)}" ${g.id === val ? "selected" : ""}>${esc((g.title || "goal").slice(0, 30))}${g.id === currentId ? " · active" : g.closed_at ? " · done" : ""}</option>`).join("")
}

// Load goal list and fill the toolbar dropdown (independent of the active view).
async function refreshGoalSelect() {
  try {
    const team = $("#team").value
    if (!team) return
    const goal = await jget("/api/goal?team=" + encodeURIComponent(team))
    fillGoalSelect(goal.goals ?? [], goal.current?.id)
  } catch { /* keep current options */ }
}

async function renderGoal() {
  const q = qs()
  const [teams, goal, tl] = await Promise.all([
    jget("/api/teams"), jget("/api/goal?" + q), jget("/api/timeline?" + q),
  ])
  fillTeamSelect(teams)
  const goals = goal.goals ?? []
  const currentId = goal.current?.id
  // Selected goal = explicit selection, else the current goal.
  const sel = goals.find((x) => x.id === (selectedGoalId ?? currentId)) ?? goal.current ?? null
  // Lifecycle events scoped to the selected goal's time window.
  const lc = (goal.lifecycle ?? []).filter((e) => {
    const t = Date.parse(e.created_at)
    if (!Number.isFinite(t)) return true
    const from = sel ? Date.parse(sel.created_at) : -Infinity
    const to = sel?.closed_at ? Date.parse(sel.closed_at) : Infinity
    return t >= from && t < to
  })
  fillGoalSelect(goals, currentId)
  const hero = $("#goalHero")
  if (sel) {
    hero.innerHTML = `<div class="goal-hero">
      <h2>${esc(sel.title)} ${sel.state ? badge(sel.state) : ""} ${sel.id === currentId ? `<span class="badge active">current</span>` : ""}</h2>
      ${sel.body ? `<p class="goal-body">${esc(sel.body)}</p>` : ""}
      <div class="muted">created ${fmtDate(sel.created_at)} · ${sel.closed_at ? "closed " + fmtDate(sel.closed_at) : "in progress"}</div>
    </div>`
  } else {
    hero.innerHTML = `<div class="goal-hero"><h2>No goal set</h2><p class="goal-body muted">Use <code>teamx goal set</code> to define the team goal.</p></div>`
  }
  renderLifecycle(lc, sel?.state)
  renderGoalHistory(goals, currentId)
  // Goal-scoped window for stats + gantt.
  const winQ = new URLSearchParams()
  if (sel) {
    winQ.set("team", $("#team").value)
    winQ.set("from", sel.created_at)
    winQ.set("to", sel.closed_at || new Date().toISOString())
  }
  const win = sel ? winQ.toString() : q
  const [overview, byMember, byNode] = await Promise.all([
    jget("/api/overview?" + win), jget("/api/by_member?" + win), jget("/api/by_node?" + win),
  ])
  // Horizontal fishbone scoped to the selected goal's lifecycle window.
  if (sel) {
    renderHFishbone(tl.items ?? [], { el: "#goalGantt", from: sel.created_at, to: sel.closed_at || new Date().toISOString() })
  } else {
    const el = document.querySelector("#goalGantt")
    if (el) el.innerHTML = '<div class="muted">no goal</div>'
  }
  const o = overview.overview ?? {}
  $("#goalCards").innerHTML = [
    ["Total work time", fmtMs(o.duration_ms)],
    ["Total tokens", fmtNum((o.tokens_input ?? 0) + (o.tokens_output ?? 0))],
    ["Cost", fmtCost(o.cost)],
    ["Active members", fmtNum(o.active_members)],
    ["Active nodes", fmtNum(o.active_nodes)],
    ["Human actions", fmtNum(overview.human?.count)],
    ["AI actions", fmtNum(overview.ai?.count)],
    ["Rows", fmtNum(overview.overall?.count)],
  ].map(([l, v]) => `<div class="card"><div class="label">${l}</div><div class="value">${v}</div></div>`).join("")
  $("#gByMember tbody").innerHTML = tableRows(byMember, [
    (m) => esc(memberLabel(m.member_id)), (m) => fmtMs(m.duration_ms),
    (m) => fmtNum(m.tokens_input) + "/" + fmtNum(m.tokens_output), (m) => fmtCost(m.cost), (m) => fmtNum(m.count),
  ])
  $("#gByNode tbody").innerHTML = tableRows(byNode, [
    (n) => esc(n.node_name ?? n.node_id?.slice(0, 8)), (n) => fmtMs(n.duration_ms),
    (n) => fmtNum(n.tokens_input), (n) => fmtCost(n.cost), (n) => fmtNum(n.count),
  ])
  // lifecycle events table
  const events = (goal.lifecycle ?? []).slice().reverse()
  $("#goalEvents tbody").innerHTML = tableRows(events, [
    (e) => fmtDate(e.created_at), (e) => badge(e.type),
    (e) => esc(memberLabel(e.member_id)), (e) => esc(row(e.payload)),
  ])
}

// Render the team's goal history (all goals, newest first; current highlighted).
function renderGoalHistory(goals, currentId) {
  const host = document.getElementById("goalHistory")
  if (!host) return
  const list = (goals ?? []).slice().reverse()
  if (list.length === 0) {
    host.innerHTML = '<div class="muted">No goals yet. Set the team goal with <code>teamx goal set</code>.</div>'
    return
  }
  host.innerHTML = `<h2>Goal history <span class="count">${list.length}</span></h2>` + list.map((g) => `
    <div class="goal-row ${g.id === currentId ? "current" : ""}">
      <span class="goal-row-title">${esc(g.title)} ${g.id === currentId ? `<span class="badge active">active</span>` : g.closed_at ? badge("closed") : ""}</span>
      <span class="goal-row-meta mono">${fmtDate(g.created_at)} → ${g.closed_at ? fmtDate(g.closed_at) : "now"}</span>
    </div>`).join("")
}

function renderLifecycle(lifecycle, currentState) {
  const el = $("#goalLifecycle")
  const events = lifecycle ?? []
  const seen = new Set()
  const steps = []
  for (const e of events) {
    let state = null
    if (e.type === "goal.set") state = "proposed"
    else if (e.type === "goal.shared") state = "shared"
    else if (e.type === "goal.achieved") state = "achieved"
    else if (e.type === "team.completed") state = "closed"
    else if (e.type === "goal.state_changed") {
      const to = e.payload?.to
      if (to) state = String(to)
    }
    if (state && !seen.has(state)) { seen.add(state); steps.push({ state, at: e.created_at }) }
  }
  const current = currentState ?? "proposed"
  if (!seen.has(current) && GOAL_STATES.includes(current)) { steps.push({ state: current, at: null }); seen.add(current) }
  if (steps.length === 0) { el.innerHTML = '<div class="muted">no lifecycle data yet</div>'; return }
  const curIdx = steps.findIndex((s) => s.state === current)
  const html = `<div class="lifecycle"><div class="lc-track">
    ${steps.map((s, i) => {
      // done = strictly before the current step; current = the current state;
      // anything after the current (e.g. achieved/closed on an in-progress goal)
      // is shown as upcoming (muted).
      const cls = i === curIdx ? "current" : (curIdx >= 0 && i < curIdx ? "done" : "")
      const day = s.at ? fmtDate(s.at).split(", ")[0] : (i === curIdx ? "now" : "—")
      return `<div class="lc-step ${cls}"><div class="dot"></div><div class="name">${esc(s.state.replace("_", " "))}</div><div class="date">${day}</div></div>`
    }).join("")}
  </div></div>`
  el.innerHTML = html
}

// ---- KanBan view (one column per member, each showing their tasks) ----
async function renderKanban() {
  const q = qs()
  const kanban = await jget("/api/kanban?" + q)
  $("#kanbanCols").innerHTML = (kanban.columns ?? []).map((col) => {
    const s = col.summary ?? {}
    const tasks = col.tasks ?? []
    const done = tasks.filter((t) => t.has_human).length
    const totTokens = tasks.reduce((sum, t) => sum + ((t.tokens_input ?? 0) + (t.tokens_output ?? 0)), 0)
    const totCost = tasks.reduce((sum, t) => sum + (t.cost ?? 0), 0)
    const costBits = totTokens > 0 || totCost > 0
      ? ` · ⚡ ${fmtNum(totTokens)} tok · <span class="cost">${fmtCost(totCost)}</span>`
      : ""
    return `<div class="kb-col">
      <div class="kb-col-head">
        <div class="kb-col-title">${esc(col.display_name)} ${col.role ? badge(col.role) : ""}</div>
        <div class="kb-col-meta muted">
          ${fmtMs(s.duration_ms)} · ${fmtNum(tasks.length)} tasks${tasks.length > 0 ? ` · ${done} with human` : ""}${costBits}
        </div>
      </div>
      ${tasks.slice(0, 12).map((r) => renderTaskCard(r)).join("") || '<div class="kb-empty muted">no work in this range</div>'}
    </div>`
  }).join("")
}

// Render one task (work_session) card.
function renderTaskCard(r) {
  let d = null
  try { d = typeof r.detail === "string" ? JSON.parse(r.detail) : (r.detail ?? null) } catch { d = null }
  const d_ = d ?? {}
  const hrs = r.duration_ms != null ? (r.duration_ms / 3600000).toFixed(1) : "?"
  const who = memberLabel(r.member_id)
  const tag = r.has_human ? "human-in-loop" : "auto"
  const desc = d_.description || d_.task || ""
  const totTok = (r.tokens_input ?? 0) + (r.tokens_output ?? 0)
  const tipRows = []
  const addRow = (k, v) => { if (v != null && v !== "") tipRows.push([k, String(v)]) }
  addRow("member", who)
  addRow("node", r.node_name)
  addRow("started", fmtDate(r.started_at))
  if (r.ended_at) addRow("ended", fmtDate(r.ended_at))
  if (r.duration_ms != null) addRow("duration", fmtMs(r.duration_ms))
  addRow("human", r.has_human ? "yes" : "no")
  if (r.tokens_input != null) addRow("tokens in", fmtNum(r.tokens_input))
  if (r.tokens_output != null) addRow("tokens out", fmtNum(r.tokens_output))
  if (r.cost != null) addRow("cost", fmtCost(r.cost))
  // description first, then the rest of detail fields (skip description dup)
  if (desc) addRow("task", desc)
  if (d_) { for (const [k, v] of Object.entries(d_)) { if (v == null || k === "description" || k === "task") continue; const val = typeof v === "object" ? JSON.stringify(v) : String(v); addRow(k, val.length > 300 ? val.slice(0, 300) + "…" : val) } }
  const descHtml = desc ? `<div class="kb-desc">${esc(desc)}</div>` : ""
  const costMeta = totTok > 0 || (r.cost ?? 0) > 0
    ? ` · <span class="cost">⚡ ${fmtNum(totTok)} tok · ${fmtCost(r.cost)}</span>`
    : ""
  const tip = `<div class="kb-tip" role="tooltip">
    <div class="tip-title"><span>${esc(tag)}</span><span class="badge work_session">work_session</span></div>
    ${descHtml}
    <table>${tipRows.map(([k, v]) => `<tr><td class="k">${esc(k)}</td><td class="v">${esc(v)}</td></tr>`).join("")}</table>
  </div>`
  return `<div class="kb-card" tabindex="0">
    <div class="kb-head"><span class="kb-who">${esc(who)}</span><span class="badge ${r.has_human ? "active" : "idle"}">${esc(tag)}</span></div>
    <div class="kb-body">${desc ? esc(desc) : `Worked <b>${hrs}h</b>`}${!desc && d_.sessionID ? " · <span class='mono'>" + esc(String(d_.sessionID)) + "</span>" : ""}</div>
    <div class="kb-meta muted">${fmtDate(r.started_at)}${r.ended_at ? " → " + fmtDate(r.ended_at) : ""}${costMeta}</div>
    ${tip}
  </div>`
}

// ---- Cost view ----
async function renderCost() {
  const q = qs()
  const [overview, byMember, byNode, tools, files] = await Promise.all([
    jget("/api/overview?" + q), jget("/api/by_member?" + q), jget("/api/by_node?" + q),
    jget("/api/tools?" + q), jget("/api/files?" + q),
  ])
  const o = overview.overview ?? {}
  $("#costCards").innerHTML = [
    ["Total tokens", fmtNum((o.tokens_input ?? 0) + (o.tokens_output ?? 0))],
    ["Input tokens", fmtNum(o.tokens_input)],
    ["Output tokens", fmtNum(o.tokens_output)],
    ["Reasoning tokens", fmtNum(o.tokens_reasoning)],
    ["Cost", fmtCost(o.cost)],
    ["Avg cost / row", fmtCost(o.cost != null && o.overall?.count ? o.cost / o.overall.count : null)],
  ].map(([l, v]) => `<div class="card"><div class="label">${l}</div><div class="value">${v}</div></div>`).join("")
  renderCostBreakdown(o)
  $("#costByMember tbody").innerHTML = tableRows(byMember, [
    (m) => esc(memberLabel(m.member_id)), (m) => fmtNum(m.tokens_input),
    (m) => fmtNum(m.tokens_output), (m) => fmtCost(m.cost), (m) => fmtNum(m.count),
  ])
  $("#costByNode tbody").innerHTML = tableRows(byNode, [
    (n) => esc(n.node_name ?? n.node_id?.slice(0, 8)), (n) => fmtNum(n.tokens_input),
    (n) => fmtNum(n.tokens_output), (n) => fmtCost(n.cost), (n) => fmtNum(n.count),
  ])
  $("#costTools tbody").innerHTML = tableRows(tools, [(t) => esc(t.tool), (t) => fmtNum(t.count)])
  $("#costFiles tbody").innerHTML = tableRows(files, [(f) => esc(f.file), (f) => fmtNum(f.count)])

  // ---- charts ----
  const mem = (byMember ?? []).map((m) => ({ label: memberLabel(m.member_id), value: m.cost, meta: fmtCost(m.cost) }))
  drawDonut("#chartCostDonut", "#chartCostDonutLegend", mem.filter((x) => x.value != null), "Cost")
  const nodes = (byNode ?? []).map((n) => ({ label: n.node_name ?? n.node_id?.slice(0, 8) ?? "?", value: n.cost, meta: fmtCost(n.cost) }))
  drawBars("#chartCostBars", nodes.filter((x) => x.value != null), "Cost by node")
  const tokens = (byMember ?? []).map((m) => ({ label: memberLabel(m.member_id), a: m.tokens_input, b: m.tokens_output, meta: fmtNum(m.tokens_input) + " / " + fmtNum(m.tokens_output) }))
  drawGroupedBars("#chartTokensBars", tokens, "in", "out")
  drawDonut("#chartHumanDonut", "#chartHumanDonutLegend", [
    { label: "AI", value: overview.ai?.count, meta: fmtNum(overview.ai?.count) },
    { label: "Human", value: overview.human?.count, meta: fmtNum(overview.human?.count) },
  ].filter((x) => x.value != null), "Actions")
}

// ---------- SVG charts (vanilla, no deps) ----------
const CHART_COLORS = ["#1E40AF", "#3B82F6", "#8B5CF6", "#D97706", "#059669", "#DC2626", "#64748B", "#0EA5E9"]
function chartSvg(w, h) { return `<svg viewBox="0 0 ${w} ${h}" xmlns="http://www.w3.org/2000/svg" role="img" style="max-width:100%;height:auto">` }
function polar(cx, cy, r, angleDeg) { const a = (angleDeg - 90) * Math.PI / 180; return [cx + r * Math.cos(a), cy + r * Math.sin(a)] }
function arcPath(cx, cy, r, startDeg, endDeg) {
  const [x1, y1] = polar(cx, cy, r, startDeg)
  const [x2, y2] = polar(cx, cy, r, endDeg)
  const large = endDeg - startDeg > 180 ? 1 : 0
  return `M ${x1} ${y1} A ${r} ${r} 0 ${large} 1 ${x2} ${y2}`
}

/** Donut chart (part-to-whole). data: [{label, value, meta}] */
function drawDonut(selId, legendId, data, title) {
  const el = document.querySelector(selId)
  const legend = document.querySelector(legendId)
  if (!el) return
  if (!data || data.length === 0) { el.innerHTML = '<div class="muted">no data</div>'; if (legend) legend.innerHTML = ""; return }
  const total = data.reduce((s, d) => s + d.value, 0)
  if (total <= 0) { el.innerHTML = '<div class="muted">no data</div>'; if (legend) legend.innerHTML = ""; return }
  const cx = 100, cy = 100, r = 62, R = 88
  let angle = 0
  const segs = data.map((d, i) => {
    const sweep = (d.value / total) * 360
    const color = CHART_COLORS[i % CHART_COLORS.length]
    // outer arc (donut ring)
    const [ax1, ay1] = polar(cx, cy, R, angle); const [ax2, ay2] = polar(cx, cy, R, angle + sweep)
    const [bx1, by1] = polar(cx, cy, r, angle + sweep); const [bx2, by2] = polar(cx, cy, r, angle)
    const large = sweep > 180 ? 1 : 0
    const path = sweep >= 360
      ? `<circle cx="${cx}" cy="${cy}" r="${R}" fill="none" stroke="${color}" stroke-width="${R - r}"/>`
      : `<path d="M ${ax1} ${ay1} A ${R} ${R} 0 ${large} 1 ${ax2} ${ay2} L ${bx1} ${by1} A ${r} ${r} 0 ${large} 0 ${bx2} ${by2} Z" fill="${color}"/>`
    const mid = angle + sweep / 2
    const [lx, ly] = polar(cx, cy, (R + r) / 2, mid)
    const label = `<text x="${lx}" y="${ly + 3}" text-anchor="middle" font-size="8.5" fill="#fff" font-weight="600">${Math.round(d.value / total * 100)}%</text>`
    angle += sweep
    return { path, label, d }
  })
  el.innerHTML = chartSvg(200, 200) + `<circle cx="${cx}" cy="${cy}" r="${R}" fill="none" stroke="var(--border)" stroke-width="1" opacity="0.4"/>
    ${segs.map((s) => s.path).join("")}${segs.filter((s) => s.d.value / total >= 0.05).map((s) => s.label).join("")}
    <text x="${cx}" y="${cy - 4}" text-anchor="middle" font-size="11" font-weight="600" fill="var(--fg)">${title}</text>
    <text x="${cx}" y="${cy + 12}" text-anchor="middle" font-size="13" font-weight="700" fill="var(--fg)">${fmtNum(total)}</text></svg>`
  if (legend) {
    legend.innerHTML = data.map((d, i) => `<span><span class="sw" style="background:${CHART_COLORS[i % CHART_COLORS.length]}"></span>${esc(d.label)} ${d.meta ?? ""}</span>`).join("")
  }
}

/** Horizontal bar chart. data: [{label, value, meta}] */
function drawBars(selId, data, title) {
  const el = document.querySelector(selId)
  if (!el) return
  if (!data || data.length === 0) { el.innerHTML = '<div class="muted">no data</div>'; return }
  const max = Math.max(...data.map((d) => d.value))
  const rows = data.sort((a, b) => b.value - a.value).map((d, i) => {
    const w = max > 0 ? (d.value / max) * 100 : 0
    const color = CHART_COLORS[i % CHART_COLORS.length]
    return `<div class="hbar-row" title="${esc(d.label)}: ${d.meta ?? ""}">
      <div class="hbar-label">${esc(d.label)}</div>
      <div class="hbar-track"><div class="hbar-fill" style="width:${w}%;background:${color}"></div></div>
      <div class="hbar-val mono">${d.meta ?? fmtNum(d.value)}</div>
    </div>`
  }).join("")
  el.innerHTML = `<div style="width:100%"><div class="hbar-title muted">${esc(title)}</div>${rows}</div>`
}

/** Grouped vertical bars (two series). data: [{label, a, b, meta}] */
function drawGroupedBars(selId, data, nameA, nameB) {
  const el = document.querySelector(selId)
  if (!el) return
  if (!data || data.length === 0) { el.innerHTML = '<div class="muted">no data</div>'; return }
  const max = Math.max(...data.flatMap((d) => [d.a ?? 0, d.b ?? 0]))
  const W = 300, H = 150, pad = 26, bottom = 18
  const n = data.length
  const groupW = (W - pad * 2) / n
  const barW = Math.min(groupW * 0.32, 26)
  const bars = data.map((d, i) => {
    const x0 = pad + i * groupW
    const hA = max > 0 ? (d.a ?? 0) / max * (H - pad - bottom) : 0
    const hB = max > 0 ? (d.b ?? 0) / max * (H - pad - bottom) : 0
    const yA = H - bottom - hA
    const yB = H - bottom - hB
    return `<rect x="${x0 + groupW / 2 - barW - 1.5}" y="${yA}" width="${barW}" height="${hA}" rx="2" fill="#1E40AF"/>
      <rect x="${x0 + groupW / 2 + 1.5}" y="${yB}" width="${barW}" height="${hB}" rx="2" fill="#3B82F6"/>
      <text x="${x0 + groupW / 2}" y="${H - 5}" text-anchor="middle" font-size="7.5" fill="var(--muted-fg)">${esc(d.label)}</text>`
  }).join("")
  el.innerHTML = chartSvg(W, H) + `
    <text x="6" y="${H - 45}" font-size="7.5" fill="#1E40AF">${esc(nameA)}</text>
    <text x="6" y="${H - 33}" font-size="7.5" fill="#3B82F6">${esc(nameB)}</text>
    ${bars}</svg>`
}

/** Cost breakdown: stacked bar + legend (input/output/cache read/cache write). */
function renderCostBreakdown(o) {
  const panel = document.getElementById("costBreakdownPanel")
  const host = document.getElementById("costBreakdown")
  if (!panel || !host) return
  const parts = [
    ["input", o.cost_input ?? 0],
    ["output", o.cost_output ?? 0],
    ["cache read", o.cost_cache_read ?? 0],
    ["cache write", o.cost_cache_write ?? 0],
  ]
  const colors = { input: "#1E40AF", output: "#3B82F6", "cache read": "#8B5CF6", "cache write": "#D97706" }
  const total = o.cost ?? 0
  const hasData = parts.some(([, v]) => v > 0)
  if (!hasData) { panel.style.display = "none"; return }
  panel.style.display = ""
  const pct = (v) => (total > 0 ? (v / total * 100).toFixed(1) : "0")
  const segs = parts.filter(([, v]) => v > 0)
  host.innerHTML = `<div class="hbar-track" style="height:20px;display:flex;overflow:hidden;border-radius:3px">${segs.map(([k, v]) =>
    `<div style="height:100%;width:${pct(v)}%;background:${colors[k]}" title="${k}: ${fmtCost(v)}"></div>`).join("")}</div>
  <div class="chart-legend" style="margin-top:8px">${segs.map(([k, v]) =>
    `<span><span class="sw" style="background:${colors[k]}"></span>${k}: ${fmtCost(v)} (${pct(v)}%)</span>`).join("")}</div>`
}

// ---- Timeline (Gantt) view ----
// ---- Timeline view (Gantt + fishbone toggle) ----
let tlMode = "fishbone"
let lastTimelineItems = []

async function renderTimeline() {
  const q = qs()
  const [teams, tl] = await Promise.all([jget("/api/teams"), jget("/api/timeline?" + q)])
  fillTeamSelect(teams)
  lastTimelineItems = tl.items ?? []
  if (tlMode === "gantt") drawGantt(lastTimelineItems)
  else renderFishbone(lastTimelineItems)
}

function setTimelineMode(mode) {
  tlMode = mode
  $$("#tlViewToggle button").forEach((b) => b.classList.toggle("active", b.dataset.tlmode === mode))
  $("#gantt").style.display = mode === "gantt" ? "" : "none"
  $("#fishbone").style.display = mode === "fishbone" ? "" : "none"
  if (tlMode === "gantt") drawGantt(lastTimelineItems)
  else renderFishbone(lastTimelineItems)
}
$$("#tlViewToggle button").forEach((b) => b.addEventListener("click", () => setTimelineMode(b.dataset.tlmode)))

// ---- Fishbone timeline (vertical, all events by time) ----
const FB_KIND_LABEL = {
  work_session: "work", tool_call: "tool", command: "tool", step_finish: "step",
  file_edit: "file", human_input: "human", human_approval: "human", human_command: "human",
}
const FB_KIND_COLOR = {
  work_session: "#0969da", tool_call: "#8250df", command: "#8250df", step_finish: "#bf8700",
  file_edit: "#cf222e", human_input: "#1a7f37", human_approval: "#1a7f37", human_command: "#1a7f37",
}
function fishKindLabel(kind) { return FB_KIND_LABEL[kind] || kind || "event" }
function fishKindColor(kind) { return FB_KIND_COLOR[kind] || "#909399" }

// Per-activity cost: how many tokens / how much $ an activity consumed.
function fmtItemCost(it) {
  if (!it) return null
  const tin = it.tokens_input ?? it.tokens_in ?? it.tokens ?? null
  const tout = it.tokens_output ?? it.tokens_out ?? null
  const c = it.cost ?? null
  if (tin == null && tout == null && c == null) return null
  const bits = []
  const t = (tin ?? 0) + (tout ?? 0)
  if (t) bits.push("⚡ " + fmtNum(t) + " tok")
  if (c != null) bits.push("$" + Number(c).toFixed(2))
  return bits.join(" · ")
}

function fishDetailText(it) {
  let d = null
  try { d = typeof it.detail === "string" ? JSON.parse(it.detail) : it.detail } catch { d = null }
  if (!d) return it.detail ? String(it.detail) : ""
  if (it.kind === "work_session") return d.task ?? d.description ?? d.title ?? ""
  if (it.kind === "tool_call") return d.tool ? "调用工具 " + d.tool + (d.file ? " · " + d.file : "") : ""
  if (it.kind === "file_edit") return d.file ? "编辑 " + d.file : ""
  if (it.kind === "step_finish") return d.step ? "完成步骤 " + d.step : ""
  if (it.kind && it.kind.startsWith("human")) return d.text ?? d.name ?? d.response ?? d.title ?? ""
  return d.text ?? d.tool ?? d.file ?? d.task ?? JSON.stringify(d)
}

function fishboneRow(it, side) {
  const t = new Date(it.started_at)
  const time = t.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })
  const who = memberLabel(it.member_id)
  const kind = it.kind || "event"
  const label = fishKindLabel(kind)
  const color = fishKindColor(kind)
  const text = fishDetailText(it)
  const dur = it.duration_ms ? `<span>${fmtMs(it.duration_ms)}</span>` : ""
  const node = it.node_name ? `<span>${esc(it.node_name)}</span>` : ""
  const human = it.has_human ? `<span class="human">● human-in-loop</span>` : ""
  const cost = fmtItemCost(it)
  const meta = [dur, node, human, cost ? `<span class="fb-cost">${esc(cost)}</span>` : ""].filter(Boolean).join("") || ""
  const card = `
    <div class="fb-card">
      <div class="fb-top">
        <span class="fb-time">${esc(time)}</span>
        <span class="fb-who">${esc(who)}</span>
        <span class="fb-kind" style="background:${color}">${esc(label)}</span>
      </div>
      <div class="fb-text">${esc(text || "(no detail)")}</div>
      ${meta ? `<div class="fb-meta">${meta}</div>` : ""}
    </div>`
  return `<div class="fb-row ${side}">
    <div class="fb-side">${side === "left" ? card : ""}</div>
    <div class="fb-center"><span class="fb-dot" style="background:${color}"></span></div>
    <div class="fb-side">${side === "right" ? card : ""}</div>
  </div>`
}

function renderFishbone(items, title) {
  const el = $("#fishbone")
  const valid = (items || []).filter((it) => Number.isFinite(Date.parse(it.started_at)))
  if (valid.length === 0) { el.innerHTML = '<div class="muted" style="padding:20px">no activity in this range</div>'; return }
  valid.sort((a, b) => Date.parse(a.started_at) - Date.parse(b.started_at))
  const startBox = `<div class="fb-start"><div class="fb-start-box"><span class="fb-start-dot"></span>${esc(title || "Start")}</div></div>`
  const rows = []
  let prevDay = null
  valid.forEach((it, i) => {
    const d = new Date(it.started_at)
    const day = d.toDateString()
    if (prevDay !== day) {
      rows.push(`<div class="fb-day">${esc(d.toLocaleDateString(undefined, { month: "short", day: "numeric", weekday: "short" }))}</div>`)
      prevDay = day
    }
    rows.push(fishboneRow(it, i % 2 === 0 ? "left" : "right"))
  })
  el.innerHTML = `<div class="fb-wrap"><div class="fb-spine"></div>${startBox}${rows.join("")}</div>`
}

// ---- Horizontal fishbone (goal-scoped activity timeline) ----
function renderHFishbone(items, opts) {
  opts = opts || {}
  const el = document.querySelector(opts.el ?? "#goalGantt")
  if (!el) return
  const winFrom = opts.from != null ? Date.parse(opts.from) : null
  const winTo = opts.to != null ? Date.parse(opts.to) : null
  const valid = (items || []).filter((it) => {
    const t = Date.parse(it.started_at)
    if (!Number.isFinite(t)) return false
    if (winFrom != null && t < winFrom) return false
    if (winTo != null && t > winTo) return false
    return true
  })
  if (valid.length === 0) { el.innerHTML = '<div class="muted">no activity in this range</div>'; return }
  valid.sort((a, b) => Date.parse(a.started_at) - Date.parse(b.started_at))
  const edge = 4
  const levels = ["top-near", "bottom-near", "top-far", "bottom-far"]
  const nodes = valid.map((it, i) => {
    const pct = valid.length === 1 ? 50 : edge + (i / (valid.length - 1)) * (100 - edge * 2)
    const cls = levels[i % 4]
    const t = new Date(it.started_at)
    const time = t.toLocaleDateString(undefined, { month: "short", day: "numeric" }) + " " +
      t.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })
    const who = memberLabel(it.member_id)
    const kind = it.kind || "event"
    const label = fishKindLabel(kind)
    const color = fishKindColor(kind)
    const text = fishDetailText(it) || "(no detail)"
    const cost = fmtItemCost(it)
    const dur = it.duration_ms ? `<span>${fmtMs(it.duration_ms)}</span>` : ""
    const human = it.has_human ? `<span class="human">● human</span>` : ""
    const meta = [dur, human].filter(Boolean).join("") || ""
    const card = `
      <div class="hfb-card">
        <div class="fb-top">
          <span class="fb-time">${esc(time)}</span>
          <span class="fb-who">${esc(who)}</span>
          <span class="fb-kind" style="background:${color}">${esc(label)}</span>
        </div>
        <div class="fb-text">${esc(text)}</div>
        <div class="fb-meta">${meta}${cost ? `<span class="hfb-cost">${esc(cost)}</span>` : ""}</div>
      </div>`
    return `<div class="hfb-node ${cls}" style="left:${pct.toFixed(2)}%">
      <span class="hfb-dot" style="background:${color}"></span>${card}
    </div>`
  })
  const f = new Date(valid[0].started_at)
  const l = new Date(valid[valid.length - 1].started_at)
  const fmtAxis = (d) => d.toLocaleDateString(undefined, { month: "short", day: "numeric" }) + " " +
    d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })
  const ruler = `<div class="hfb-ruler"><span>${esc(fmtAxis(f))}</span><span>${esc(fmtAxis(l))}</span></div>`
  el.innerHTML = `<div class="hfb-wrap"><div class="hfb-spine"></div>${nodes.join("")}</div>${ruler}`
}

function drawGantt(items, opts) {
  opts = opts || {}
  const el = document.querySelector(opts.el ?? "#gantt")
  if (!el) return
  if (!items || items.length === 0) { el.innerHTML = '<div class="muted">no activity in this range</div>'; return }
  // Optional fixed window (goal lifecycle): clamp items and axis to it.
  const winFrom = opts.from != null ? new Date(opts.from).getTime() : null
  const winTo = opts.to != null ? new Date(opts.to).getTime() : null
  let min = Infinity, max = -Infinity
  for (const it of items) {
    const s = Date.parse(it.started_at); if (!Number.isFinite(s)) continue
    if (winFrom != null && s < winFrom) continue
    let e = s
    if (it.ended_at) { const ee = Date.parse(it.ended_at); if (Number.isFinite(ee)) e = ee }
    else if (it.duration_ms) e = s + it.duration_ms
    if (winTo != null && s > winTo) continue
    if (winFrom != null && Number.isFinite(s)) min = Math.min(min, Math.max(s, winFrom))
    else min = Math.min(min, s)
    if (Number.isFinite(e)) max = Math.max(max, e)
  }
  if (winFrom != null && Number.isFinite(min)) min = Math.min(min, winFrom)
  if (winTo != null && Number.isFinite(max)) max = Math.max(max, winTo)
  if (!Number.isFinite(min) || !Number.isFinite(max)) { el.innerHTML = '<div class="muted">no activity in this range</div>'; return }
  const span = max - min
  const byMember = new Map()
  for (const it of items) {
    const s = Date.parse(it.started_at)
    if (!Number.isFinite(s)) continue
    if (winFrom != null && s < winFrom) continue
    if (winTo != null && s > winTo) continue
    if (!byMember.has(it.member_id)) byMember.set(it.member_id, [])
    byMember.get(it.member_id).push(it)
  }
  if (byMember.size === 0) { el.innerHTML = '<div class="muted">no activity in this range</div>'; return }

  // ---- layout ----
  const labelW = 150        // left member column
  const rowH = 46           // lane height
  const topPad = 34         // axis area
  // Responsive: use full viewport width minus padding, fallback to 760px
  const containerW = window.innerWidth - 40 || 760
  const W = Math.max(760, containerW)   // total width (px)
  const H = topPad + byMember.size * rowH + 12
  const x = (t) => labelW + (t - min) / span * (W - labelW)
  const laneTop = (i) => topPad + i * rowH

  // ---- time ticks (adaptive) ----
  const spanDays = span / 86400000
  const unit = spanDays > 120 ? "month" : spanDays > 45 ? "week" : spanDays > 4 ? "day" : "hour"
  const ticks = []
  const t0 = new Date(min)
  if (unit === "hour") { const h0 = new Date(min); h0.setMinutes(0, 0, 0); for (let t = h0.getTime(); t <= max; t += 3600000) ticks.push(t) }
  else if (unit === "day") { const d0 = new Date(min); d0.setHours(0, 0, 0, 0); for (let t = d0.getTime(); t <= max; t += 86400000) ticks.push(t) }
  else if (unit === "week") { const d0 = new Date(min); d0.setHours(0, 0, 0, 0); const dow = (d0.getDay() + 6) % 7; d0.setDate(d0.getDate() - dow); for (let t = d0.getTime(); t <= max; t += 7 * 86400000) ticks.push(t) }
  else { const m0 = new Date(min); m0.setDate(1); m0.setHours(0, 0, 0, 0); for (let t = m0.getTime(); t <= max; ) { ticks.push(t); const d = new Date(t); d.setMonth(d.getMonth() + 1); t = d.getTime() } }
  if (ticks.length > 24) { const step = Math.ceil(ticks.length / 24); for (let i = 0; i < ticks.length; i++) if (i % step !== 0) ticks[i] = null }

  const fmtTick = (t) => unit === "hour" ? new Date(t).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })
    : unit === "day" ? new Date(t).toLocaleDateString(undefined, { month: "short", day: "numeric" })
    : unit === "week" ? new Date(t).toLocaleDateString(undefined, { month: "short", day: "numeric" })
    : new Date(t).toLocaleDateString(undefined, { month: "short", year: "2-digit" })
  const today = new Date(); today.setHours(0, 0, 0, 0)
  const todayMs = today.getTime()

  // ---- SVG ----
  const svg = []
  svg.push(`<svg viewBox="0 0 ${W} ${H}" width="100%" height="${H}" xmlns="http://www.w3.org/2000/svg" role="img" class="gantt-svg" style="display:block">`)
  // background + lane separators
  svg.push(`<rect x="0" y="0" width="${labelW}" height="${H}" fill="var(--muted)" opacity="0.5"/>`)
  svg.push(`<rect x="0" y="0" width="${W}" height="${topPad}" fill="var(--card)"/>`)
  // gridlines
  for (const t of ticks) { if (t == null) continue; const gx = x(t); svg.push(`<line x1="${gx}" y1="${topPad}" x2="${gx}" y2="${H}" stroke="var(--border)" stroke-width="1"/>`) }
  // today line
  if (todayMs >= min && todayMs <= max) {
    const tx = x(todayMs)
    svg.push(`<line x1="${tx}" y1="${topPad}" x2="${tx}" y2="${H}" stroke="var(--accent)" stroke-width="1.5" stroke-dasharray="4 3"/>`)
    svg.push(`<text x="${tx + 4}" y="${topPad - 8}" font-size="10" fill="var(--accent)" font-weight="600">today</text>`)
  }
  // tick labels
  for (const t of ticks) { if (t == null) continue; const tx = x(t); svg.push(`<text x="${tx}" y="${topPad - 10}" font-size="10.5" fill="var(--muted-fg)" font-family="var(--font-mono)">${esc(fmtTick(t))}</text>`) }

  // ---- lanes ----
  const defs = `<defs>
    <linearGradient id="gWork" x1="0" y1="0" x2="1" y2="0">
      <stop offset="0%" stop-color="#4A90E5"/><stop offset="100%" stop-color="#6BB3F0"/>
    </linearGradient>
  </defs>`
  svg.push(defs)

  const members = Array.from(byMember.entries())
  members.forEach(([mid, items2], i) => {
    const yTop = laneTop(i)
    const midY = yTop + rowH / 2
    const name = memberLabel(mid) || (items2[0].node_name ?? mid.slice(0, 8))
    // lane label
    svg.push(`<text x="${labelW - 12}" y="${midY + 4}" font-size="12.5" font-weight="500" fill="var(--fg)" text-anchor="end">${esc(name)}</text>`)
    // work sessions as bars
    const sessions = items2.filter((it) => it.kind === "work_session")
    sessions.forEach((it) => {
      const s = Date.parse(it.started_at)
      let e = s
      if (it.ended_at) { const ee = Date.parse(it.ended_at); if (Number.isFinite(ee)) e = ee }
      else if (it.duration_ms) e = s + it.duration_ms
      const bx = x(s), bw = Math.max(x(e) - bx, 18)
      const by = midY - 9
      let d = null; try { d = typeof it.detail === "string" ? JSON.parse(it.detail) : it.detail } catch { d = null }
      const dur = it.duration_ms ? ` · ${fmtMs(it.duration_ms)}` : ""
      const human = it.has_human ? " · human-in-loop" : ""
      const detail = d && Object.keys(d).length ? `\n${JSON.stringify(d, null, 1)}` : ""
      svg.push(`<g class="gantt-seg"><rect x="${bx}" y="${by}" width="${bw}" height="18" rx="9" fill="url(#gWork)" opacity="0.9">
        <title>${esc(memberLabel(it.member_id))} · work ${new Date(s).toLocaleString()} → ${new Date(e).toLocaleString()}${dur}${human}${detail}</title>
      </rect></g>`)
      // human marker inside bar
      if (it.has_human) {
        svg.push(`<circle cx="${bx + Math.min(bw, 10)}" cy="${midY}" r="3" fill="#fff" opacity="0.9"/>`)
      }
    })
    // human events as small bars (approval/input/command)
    items2.filter((it) => it.kind.startsWith("human")).forEach((it) => {
      const s = Date.parse(it.started_at)
      if (!Number.isFinite(s)) return
      const hx = x(s)
      // Minimum bar width = 18px so short events are visible as swim-lane segments
      const barW = 18
      let d = null; try { d = typeof it.detail === "string" ? JSON.parse(it.detail) : it.detail } catch { d = null }
      const label = d ? (d.text ?? d.name ?? d.response ?? "") : ""
      svg.push(`<rect x="${hx - barW / 2}" y="${midY - 6}" width="${barW}" height="12" rx="3" fill="#059669" opacity="0.85">
        <title>${esc(it.kind)}${label ? " · " + esc(String(label).slice(0, 60)) : ""}</title>
      </rect>`)
    })
    // per-lane counters (tool/step aggregated, no visual noise)
    const tools = items2.filter((it) => it.kind === "tool_call").length
    const steps = items2.filter((it) => it.kind === "step_finish").length
    const files = items2.filter((it) => it.kind === "file_edit").length
    if (tools || steps || files) {
      const bits = []
      if (tools) bits.push(`${tools} tools`)
      if (steps) bits.push(`${steps} steps`)
      if (files) bits.push(`${files} files`)
      svg.push(`<text x="${labelW + 6}" y="${yTop + 11}" font-size="9.5" fill="var(--muted-fg)">${esc(bits.join(" · "))}</text>`)
    }
  })
  svg.push(`</svg>`)

  const legend = `<div class="gantt-legend">
    <span><span class="sw" style="background:linear-gradient(90deg,var(--primary),var(--secondary))"></span>work session</span>
    <span><span class="sw" style="background:#059669;border-radius:3px"></span>human action</span>
    <span><span class="sw" style="background:var(--accent);border-radius:2px;width:14px"></span>today</span>
  </div>`
  el.innerHTML = `<div style="width:100%">${legend}${svg.join("")}</div>`
}

// ---- Members view ----
// Recent-activity window (ms) for the cards; adjustable via #mliveWindowToggle.
let membersWindowMs = 600000

async function renderMembers() {
  // Live member view: members come from the local activity API; sessions come
  // from each member's opencode (proxied via teamx ui /api/members/{id}/...).
  const q = qs() + "&window=" + membersWindowMs
  const data = await jget("/api/members?" + q)
  const members = data.members ?? []
  membersCache = members
  $("#mliveMemberCount").textContent = String(members.length)
  // Overview cards: 在线成员/活跃会话/正在执行工具 = 最近时间窗口活动聚合;
  // 打开的终端 = 实时 presence.
  const recent = data.recent ?? {}
  const activeMembers = recent.active_members ?? 0
  const workSessions = recent.work_sessions ?? 0
  const toolCalls = recent.tool_calls ?? 0
  const openTerminals = members.reduce((s, m) => s + (m.presence?.open_terminals ?? 0), 0)
  $("#mliveCards").innerHTML = [
    ["在线成员", `${fmtNum(activeMembers)}<span class="muted" style="font-size:13px;font-weight:400">/ ${members.length}</span>`, "●", "green"],
    ["活跃会话", fmtNum(workSessions), "💬", "blue"],
    ["正在执行工具", fmtNum(toolCalls), "⚡", "orange"],
    ["打开的终端", fmtNum(openTerminals), "⌨", "purple"],
  ].map(([l, v, icon, color]) => `<div class="card"><div class="card-icon ${color}">${icon}</div><div class="card-body"><div class="label">${l}</div><div class="value">${v}</div></div></div>`).join("")
  // Member list
  $("#mliveMemberList").innerHTML = members.map((m) => {
    const node = m.activity?.node_name ? ` · ${esc(m.activity.node_name)}` : ""
    const cost = m.activity?.cost != null ? ` · <span class="cost">${fmtCost(m.activity.cost)}</span>` : ""
    return `
    <div class="member-item" data-mlive="${esc(m.id)}">
      <div class="mi-avatar">${esc((m.display_name || "?").slice(0,1).toUpperCase())}</div>
      <div class="mi-main">
        <div class="mi-name">${esc(m.display_name)} ${m.role ? badge(m.role) : ""}</div>
        <div class="mi-sub"><span class="status-dot ${m.state === "active" ? "online" : m.state === "idle" ? "busy" : "offline"}"></span>${esc(m.state)} · ${fmtMs(m.activity?.duration_ms)}${node}${cost}</div>
      </div>
      ${m.state === "active" ? '<div class="mi-live"><span class="live-badge">LIVE</span></div>' : ""}
    </div>`
  }).join("")
  // Click a member to load their opencode sessions
  $$(".member-item").forEach((el) => {
    el.addEventListener("click", () => {
      $$(".member-item").forEach((x) => x.classList.remove("selected"))
      el.classList.add("selected")
      const mid = el.dataset.mlive
      loadMemberLive(mid, el.querySelector(".mi-name")?.textContent || mid)
    })
  })
}

// ---- live member session view (ENT-01) ----
let liveMemberId = null
let liveTimer = null
let liveSessionId = null
// Cached member list (id -> activity aggregates) for the detail cost tag.
let membersCache = []

async function loadMemberLive(memberId, displayName) {
  liveMemberId = memberId
  $("#mliveEmpty").style.display = "none"
  $("#mliveDetail").style.display = ""
  $("#mliveMemberName").innerHTML = `<span class="status-dot online"></span> ${esc(displayName)}`
  // Cost tag (per-member cumulative cost) + running tag.
  const meta = membersCache.find((m) => m.id === memberId)
  const cost = meta?.activity?.cost
  const costTag = $("#mliveCostTag")
  if (cost != null) {
    costTag.textContent = `⚡ ${fmtNum(meta.activity.tokens_input ?? 0)} tok · ${fmtCost(cost)}`
    costTag.style.display = ""
  } else {
    costTag.style.display = "none"
  }
  $("#mliveRunningTag").style.display = "none"
  $("#mliveSessionTitle").textContent = "—"
  // Fetch the member's opencode sessions through the ui proxy.
  try {
    const resp = await fetch(`/api/members/${encodeURIComponent(memberId)}/sessions`)
    const data = await resp.json()
    const sessions = (data.data ?? data.sessions ?? []).map((s, i) => ({
      id: s.id ?? "ses_" + i,
      title: s.title || ("session " + (i + 1)),
      agent: s.agent ?? s.agentID ?? "main",
      model: s.model?.id ?? s.model ?? "",
      tokens: s.tokens ? (s.tokens.input ?? 0) + (s.tokens.output ?? 0) : 0,
      time: s.time?.updated ? new Date(s.time.updated) : new Date(),
    }))
    renderSessions(sessions)
    startLivePolling(memberId, sessions)
  } catch (e) {
    $("#mliveSessionTitle").textContent = "无法连接该成员的 opencode（未注册隧道？）"
    $("#mtab-live").innerHTML = `<div class="muted">${esc(String(e))}</div>`
  }
}

function renderSessions(sessions) {
  const host = $("#mtab-sessions")
  if (!sessions.length) { host.innerHTML = '<div class="muted">no sessions</div>'; return }
  host.innerHTML = sessions.map((s) => `
    <div class="session-item" data-sid="${esc(s.id)}">
      <div class="si-icon">💬</div>
      <div class="si-main"><div class="si-title">${esc(s.title)}</div>
        <div class="si-sub">@${esc(s.agent)} ${s.model ? "· " + esc(s.model) : ""} · ${fmtNum(s.tokens)} tokens</div></div>
      <div class="si-time">${s.time.toLocaleTimeString()}</div>
    </div>`).join("")
  const first = sessions[0]
  if (first) {
    $("#mliveSessionTitle").textContent = first.title
    $("#mtab-live").innerHTML = `<div class="live-line"><span class="ll-time">—</span><span class="ll-icon">🤖</span><span class="ll-text"><b>${esc(first.agent)}</b> · ${esc(first.title)} · ${fmtNum(first.tokens)} tokens</span></div><div class="muted mt16">会话消息/实时活动通过成员 opencode 事件流持续刷新…</div>`
  }
}

// Poll the member's messages for the first session every few seconds.
// On repeated failures (member offline / not tunneled) we back off so a dead
// member doesn't hammer the server with new tunnel attempts every 5s.
let liveFailures = 0

// Render the full message list (User/Assistant/Tool) into the messages tab.
function renderMessages(msgs) {
  const host = $("#mtab-messages")
  if (!host) return
  if (!Array.isArray(msgs) || msgs.length === 0) { host.innerHTML = '<div class="muted">no messages</div>'; return }
  host.innerHTML = msgs.map((m) => {
    const role = m.role ?? m.type ?? "event"
    const icon = role === "user" ? "💬" : role === "assistant" ? "🤖" : role === "tool" ? "🔧" : "•"
    const text = (m.parts ?? []).map((p) => typeof p === "string" ? p : (p.text ?? (p.tool ? "[tool " + p.tool + "]" : ""))).join(" ").slice(0, 500) || JSON.stringify(m).slice(0, 500)
    const time = m.time?.created ? new Date(m.time.created).toLocaleTimeString() : ""
    return `<div class="live-line"><span class="ll-time">${esc(time)}</span><span class="ll-icon">${icon}</span><span class="ll-text">${esc(role)}: ${esc(text)}</span></div>`
  }).join("")
}

function startLivePolling(memberId, sessions) {
  if (liveTimer) clearInterval(liveTimer)
  liveFailures = 0
  if (!sessions.length) return
  const sid = sessions[0].id
  liveSessionId = sid
  // Show the interrupt / send-message controls now that a session is active.
  $("#mliveInterruptBtn").style.display = ""
  $("#mliveSendBtn").style.display = ""
  $("#mliveRunningTag").style.display = ""
  const poll = async () => {
    if (document.hidden) return
    try {
      const resp = await fetch(`/api/members/${encodeURIComponent(memberId)}/session/${encodeURIComponent(sid)}/message`)
      if (!resp.ok) throw new Error("HTTP " + resp.status)
      const data = await resp.json()
      liveFailures = 0
      const msgs = data.data ?? data.messages ?? data
      if (Array.isArray(msgs) && msgs.length) {
        const last = msgs[msgs.length - 1]
        const role = last.role ?? last.type ?? "event"
        const text = (last.parts ?? []).map((p) => typeof p === "string" ? p : (p.text ?? "")).join(" ").slice(0, 180) || JSON.stringify(last).slice(0, 180)
        const host = $("#mtab-live")
        const line = document.createElement("div")
        line.className = "live-line"
        line.innerHTML = `<span class="ll-time">${new Date().toLocaleTimeString()}</span><span class="ll-icon">${role === "user" ? "💬" : "🤖"}</span><span class="ll-text">${esc(text)}</span>`
        host.prepend(line)
        while (host.children.length > 50) host.lastChild?.remove()
        // Full message list for the messages tab.
        renderMessages(msgs)
      }
    } catch {
      liveFailures++
      // After a few consecutive failures, stop polling (the member view is
      // effectively offline). Any new member click resets this via startLivePolling.
      if (liveFailures >= 3 && liveTimer) {
        clearInterval(liveTimer)
        liveTimer = null
      }
    }
  }
  poll()
  liveTimer = setInterval(poll, 5000)
}


// ---- web terminal (ENT-01 P2): xterm.js over the ui WS proxy ----
function ensureXterm() {
  return new Promise((resolve, reject) => {
    if (window.Terminal) { resolve(); return }
    const css = document.createElement("link")
    css.rel = "stylesheet"
    css.href = "/xterm.css"
    document.head.appendChild(css)
    const js = document.createElement("script")
    js.src = "/xterm.js"
    js.onload = () => resolve()
    js.onerror = () => reject(new Error("xterm.js failed to load"))
    document.head.appendChild(js)
  })
}

let term = null
let termWs = null
let termActiveMember = null

 async function openMemberTerminal(memberId) {
   // Avoid reopening when the terminal for this member is already connected.
   if (termWs && termWs.readyState === WebSocket.OPEN && term && termActiveMember === memberId) {
     term.focus()
     return
   }
   try {
     await ensureXterm()
   } catch (e) {
     $("#mliveTerm").innerHTML = `<div class="muted">无法加载终端库（需要网络）：${esc(String(e))}</div>`
     return
   }
   // Create a PTY on the member via ui proxy.
   let ptyId = null
   try {
     const resp = await fetch(`/api/members/${encodeURIComponent(memberId)}/pty`, {
       method: "POST",
       headers: { "Content-Type": "application/json" },
       body: JSON.stringify({ command: "zsh", cwd: undefined }),
     })
     const data = await resp.json()
     ptyId = data.id ?? data.ptyID ?? data
   } catch (e) {
     $("#mliveTerm").innerHTML = `<div class="muted">无法创建终端：${esc(String(e))}</div>`
     return
   }
   if (!ptyId || typeof ptyId === "object") {
     $("#mliveTerm").innerHTML = `<div class="muted">成员未返回 pty id：${esc(JSON.stringify(ptyId))}</div>`
     return
   }
    const host = $("#mliveTerm")
    host.innerHTML = `<div class="term-backdrop" id="termBackdrop"></div><div class="term-window" id="termWindow"><div class="term-titlebar">
      <span class="term-dot" style="background:#ff5f56"></span><span class="term-dot" style="background:#ffbd2e"></span><span class="term-dot" style="background:#27c93f"></span>
      <span style="margin-left:8px" id="termTitle">member shell</span><span style="margin-left:auto;font-size:11px">pty</span><button class="term-max-btn" id="termMax">⛶</button></div>
      <div class="term-body" id="termMount"></div></div>
      <div class="mt16 muted" style="font-size:12px">该终端运行在成员的机器，通过 mTLS 隧道实时连接。输入会发送到对方 shell（owner 操作，请谨慎）。</div>`
    // Terminal maximize toggle (fullscreen overlay + backdrop).
    const win = $("#termWindow")
    const backdrop = $("#termBackdrop")
    const maxBtn = $("#termMax")
    const toggleMax = () => {
      const on = win.classList.toggle("maximized")
      backdrop.classList.toggle("open", on)
      maxBtn.textContent = on ? "🗕" : "⛶"
      if (term) setTimeout(() => term.fit(), 20)
    }
    maxBtn?.addEventListener("click", toggleMax)
    backdrop?.addEventListener("click", toggleMax)
   const mount = $("#termMount")
   term = new Terminal({ cursorBlink: true, fontSize: 13, theme: { background: "#1e1e1e" } })
   term.open(mount)
   termActiveMember = memberId
   term.write("\r\n\x1b[90mconnecting to member terminal...\x1b[0m\r\n")
   // WS proxy path
   const wsProto = location.protocol === "https:" ? "wss" : "ws"
   const wsUrl = `${wsProto}://${location.host}/ws/members/${encodeURIComponent(memberId)}/pty/${encodeURIComponent(ptyId)}`
   termWs = new WebSocket(wsUrl)
   // Terminal data arrives as binary (raw PTY bytes) — decode as UTF-8.
   termWs.binaryType = "arraybuffer"
   termWs.onopen = () => { term.write("\r\n\x1b[92mconnected\x1b[0m\r\n"); term.focus() }
   termWs.onmessage = (e) => {
     try {
       if (typeof e.data === "string") { term.write(e.data) }
       else if (e.data instanceof ArrayBuffer) { term.write(new TextDecoder("utf-8").decode(e.data)) }
     } catch {}
   }
   termWs.onclose = () => { try { term.write("\r\n\x1b[91mdisconnected\x1b[0m\r\n") } catch {} }
   term.onData((d) => { if (termWs && termWs.readyState === WebSocket.OPEN) termWs.send(d) })
 }

// Terminal tab activation: if a member is selected, offer to open the terminal.
function switchMemberTab(name) {
  ;["live", "sessions", "messages", "terminal"].forEach((x) => {
    const el = document.getElementById("mtab-" + x)
    if (el) el.style.display = x === name ? "" : "none"
  })
  $$(".detail-tabs button").forEach((b) => b.classList.toggle("active", b.dataset.mtab === name))
}


// ---- shared ----
// member_id -> display_name (loaded once from /api/members)
const memberNames = new Map()
async function loadMemberNames() {
  try {
    const data = await jget("/api/members?team=" + encodeURIComponent($("#team").value))
    for (const m of data.members ?? []) memberNames.set(m.id, m.display_name || m.id.slice(0, 8))
  } catch { /* keep empty map */ }
}
function memberLabel(id) {
  if (!id) return "—"
  return memberNames.get(id) ?? id.slice(0, 8)
}
function fillTeamSelect(teams) {
  const sel = $("#team")
  if (sel.options.length === 0) {
    const all = document.createElement("option"); all.value = ""; all.textContent = "All"; sel.appendChild(all)
    for (const t of teams?.teams ?? []) { const o = document.createElement("option"); o.value = t.id; o.textContent = t.name; sel.appendChild(o) }
    sel.addEventListener("change", () => { selectedGoalId = null; refreshGoalSelect(); refresh() })
  }
}
function refresh() {
  const active = $$("nav button").find((b) => b.classList.contains("active"))?.dataset.view ?? "kanban"
  if (active === "timeline") renderTimeline()
  else if (active === "cost") renderCost()
  else if (active === "goal") renderGoal()
  else if (active === "members") renderMembers()
  else renderKanban()
}
$("#kind")?.addEventListener("change", refresh)
$("#goal").addEventListener("change", () => {
  selectedGoalId = $("#goal").value || null
  refresh()
})

// ---- Kibana-style time picker interaction ----
function updateTimeLabel() {
  const el = document.getElementById("timeLabel")
  if (el) el.textContent = timeState.label
}
function closeTimepicker() {
  document.getElementById("timePopover")?.classList.remove("open")
}
document.getElementById("timeBtn")?.addEventListener("click", (e) => {
  e.stopPropagation()
  const pop = document.getElementById("timePopover")
  if (pop) pop.classList.toggle("open")
})
document.getElementById("timePopover")?.addEventListener("click", (e) => e.stopPropagation())
document.addEventListener("click", closeTimepicker)
// quick buttons
document.querySelectorAll(".tp-quick").forEach((b) => {
  b.addEventListener("click", () => {
    timeState.type = "quick"
    timeState.from = b.dataset.from
    timeState.label = b.textContent.trim()
    document.querySelectorAll(".tp-quick").forEach((x) => x.classList.remove("sel"))
    b.classList.add("sel")
    updateTimeLabel()
    closeTimepicker()
    refresh()
  })
})
// tabs
document.querySelectorAll(".tp-tab").forEach((t) => {
  t.addEventListener("click", () => {
    document.querySelectorAll(".tp-tab").forEach((x) => x.classList.toggle("active", x === t))
    document.querySelectorAll(".tp-pane").forEach((p) => p.classList.toggle("active", p.dataset.tpPane === t.dataset.tp))
  })
})
// absolute apply
document.getElementById("absApply")?.addEventListener("click", () => {
  timeState.type = "absolute"
  timeState.absFrom = document.getElementById("absFrom").value || null
  timeState.absTo = document.getElementById("absTo").value || null
  const fromL = timeState.absFrom ? new Date(timeState.absFrom).toLocaleString() : "start"
  const toL = timeState.absTo ? new Date(timeState.absTo).toLocaleString() : "now"
  timeState.label = fromL + " → " + toL
  updateTimeLabel()
  closeTimepicker()
  refresh()
})
// Live member tabs + terminal button wiring (ENT-01).
$$(".detail-tabs button").forEach((b) => {
  b.addEventListener("click", () => {
    switchMemberTab(b.dataset.mtab)
    if (b.dataset.mtab === "terminal" && liveMemberId) openMemberTerminal(liveMemberId)
  })
})
document.getElementById("mliveTermBtn")?.addEventListener("click", () => {
  switchMemberTab("terminal")
  if (liveMemberId) openMemberTerminal(liveMemberId)
})
// Members-view recent-activity window toggle.
$$("#mliveWindowToggle button").forEach((b) => {
  b.addEventListener("click", () => {
    membersWindowMs = Number(b.dataset.win) || 600000
    $$("#mliveWindowToggle button").forEach((x) => x.classList.toggle("active", x === b))
    renderMembers()
  })
})
// Interrupt the member's current session (proxy opencode abort).
document.getElementById("mliveInterruptBtn")?.addEventListener("click", async () => {
  if (!liveMemberId || !liveSessionId) return
  try {
    await fetch(`/api/members/${encodeURIComponent(liveMemberId)}/session/${encodeURIComponent(liveSessionId)}/abort`, { method: "POST" })
  } catch (e) { /* ignore */ }
})
// Send a message to the member's current session (proxy opencode prompt).
document.getElementById("mliveSendBtn")?.addEventListener("click", () => {
  if (!liveMemberId || !liveSessionId) return
  const text = window.prompt("发送消息给该成员的会话：")
  if (text == null || text.trim() === "") return
  fetch(`/api/members/${encodeURIComponent(liveMemberId)}/session/${encodeURIComponent(liveSessionId)}/prompt`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ parts: [{ type: "text", text: text.trim() }] }),
  }).catch(() => {})
})

async function boot() {
  try {
    const teams = await jget("/api/teams")
    fillTeamSelect(teams)
    await loadMemberNames()
    await refreshGoalSelect()
    // Standalone live-monitor route (/members-live) opens directly on Members.
    if (location.pathname.endsWith("/members-live")) showView("members")
    else renderKanban()
  } catch (e) {
    document.body.insertAdjacentHTML("beforeend", `<div style="position:fixed;bottom:10px;right:10px;background:#ffebe9;border:1px solid #cf222e;color:#cf222e;padding:10px 14px;border-radius:6px">${esc(String(e))}</div>`)
  }
}
boot()
setInterval(() => refresh(), 30000)
</script>
</body>
</html>"##
    .to_string()
}

/// Render a styled 401 unauthorized page (OA-style).
fn unauthorized_html() -> String {
    r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>401 - Unauthorized | Teamx Enterprise</title>
<style>
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body { font-family: "Microsoft YaHei", "PingFang SC", -apple-system, BlinkMacSystemFont, sans-serif; background: #f8f8f8; color: #333; min-height: 100vh; display: flex; flex-direction: column; }
  header { height: 60px; background: #fff; border-bottom: 1px solid #EBEEF5; display: flex; align-items: center; padding: 0 20px; box-shadow: 0 1px 4px rgba(0,0,0,0.08); }
  .logo { display: flex; align-items: center; gap: 10px; }
  .logo-icon { width: 32px; height: 32px; background: linear-gradient(135deg, #4A90E5, #357ABD); border-radius: 6px; display: flex; align-items: center; justify-content: center; color: white; font-weight: 700; font-size: 16px; }
  .logo-text { font-size: 16px; font-weight: 600; color: #4A90E5; }
  .main { flex: 1; display: flex; align-items: center; justify-content: center; padding: 40px 20px; }
  .card { background: #fff; border-radius: 4px; box-shadow: 0 2px 4px 0 rgba(214,228,241,0.7); padding: 48px 56px; text-align: center; max-width: 480px; width: 100%; }
  .icon { width: 80px; height: 80px; margin: 0 auto 24px; background: #FDF6EC; border-radius: 50%; display: flex; align-items: center; justify-content: center; }
  .icon svg { width: 40px; height: 40px; color: #E6A23C; }
  h1 { font-size: 20px; font-weight: 600; color: #333; margin-bottom: 8px; }
  .code { font-size: 48px; font-weight: 700; color: #4A90E5; margin-bottom: 8px; font-family: "SF Mono", "Fira Code", monospace; }
  .msg { font-size: 14px; color: #909399; line-height: 1.6; margin-bottom: 24px; }
  .hint { font-size: 13px; color: #909399; }
  .hint a { color: #4A90E5; text-decoration: none; }
</style>
</head>
<body>
<header>
  <div class="logo">
    <div class="logo-icon">Tx</div>
    <span class="logo-text">Teamx Enterprise</span>
  </div>
</header>
<div class="main">
  <div class="card">
    <div class="icon">
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        <rect x="3" y="11" width="18" height="11" rx="2" ry="2"></rect>
        <path d="M7 11V7a5 5 0 0 1 10 0v4"></path>
      </svg>
    </div>
    <div class="code">401</div>
    <h1>Unauthorized</h1>
    <p class="msg">Please login with a valid token.</p>
    <p class="hint">Redirecting to login in <span id="cd">5</span>s... <a href="/login">Login now</a></p>
  </div>
</div>
<script>let s=5;const e=document.getElementById("cd");const t=setInterval(()=>{s--;if(e)e.textContent=s;if(s<=0){clearInterval(t);location.href="/login"}},1000)</script>
</body>
</html>"##
    .to_string()
}

fn not_found_html() -> String {
    r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>404 - Not Found | Teamx Enterprise</title>
<style>
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body { font-family: "Microsoft YaHei", "PingFang SC", -apple-system, BlinkMacSystemFont, sans-serif; background: #f8f8f8; color: #333; min-height: 100vh; display: flex; flex-direction: column; }
  header { height: 60px; background: #fff; border-bottom: 1px solid #EBEEF5; display: flex; align-items: center; padding: 0 20px; box-shadow: 0 1px 4px rgba(0,0,0,0.08); }
  .logo { display: flex; align-items: center; gap: 10px; }
  .logo-icon { width: 32px; height: 32px; background: linear-gradient(135deg, #4A90E5, #357ABD); border-radius: 6px; display: flex; align-items: center; justify-content: center; color: white; font-weight: 700; font-size: 16px; }
  .logo-text { font-size: 16px; font-weight: 600; color: #4A90E5; }
  .main { flex: 1; display: flex; align-items: center; justify-content: center; padding: 40px 20px; }
  .card { background: #fff; border-radius: 4px; box-shadow: 0 2px 4px 0 rgba(214,228,241,0.7); padding: 48px 56px; text-align: center; max-width: 480px; width: 100%; }
  .icon { width: 80px; height: 80px; margin: 0 auto 24px; background: #F2F6FC; border-radius: 50%; display: flex; align-items: center; justify-content: center; }
  .icon svg { width: 40px; height: 40px; color: #909399; }
  h1 { font-size: 20px; font-weight: 600; color: #333; margin-bottom: 8px; }
  .code { font-size: 48px; font-weight: 700; color: #4A90E5; margin-bottom: 8px; font-family: "SF Mono", "Fira Code", monospace; }
  .msg { font-size: 14px; color: #909399; line-height: 1.6; margin-bottom: 24px; }
  .hint { font-size: 13px; color: #909399; }
  .hint a { color: #4A90E5; text-decoration: none; }
</style>
</head>
<body>
<header>
  <div class="logo">
    <div class="logo-icon">Tx</div>
    <span class="logo-text">Teamx Enterprise</span>
  </div>
</header>
<div class="main">
  <div class="card">
    <div class="icon">
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        <circle cx="12" cy="12" r="10"></circle>
        <line x1="12" y1="8" x2="12" y2="12"></line>
        <line x1="12" y1="16" x2="12.01" y2="16"></line>
      </svg>
    </div>
    <div class="code">404</div>
    <h1>Page Not Found</h1>
    <p class="msg">The page you requested does not exist.</p>
    <p class="hint">Redirecting to login in <span id="cd">5</span>s... <a href="/login">Login now</a></p>
  </div>
</div>
<script>let s=5;const e=document.getElementById("cd");const t=setInterval(()=>{s--;if(e)e.textContent=s;if(s<=0){clearInterval(t);location.href="/login"}},1000)</script>
</body>
</html>"##
    .to_string()
}

fn login_html() -> String {
    r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Login | Teamx Enterprise</title>
<style>
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body { font-family: "Microsoft YaHei", "PingFang SC", -apple-system, BlinkMacSystemFont, sans-serif; background: #f8f8f8; color: #333; min-height: 100vh; display: flex; flex-direction: column; }
  header { height: 60px; background: #fff; border-bottom: 1px solid #EBEEF5; display: flex; align-items: center; padding: 0 20px; box-shadow: 0 1px 4px rgba(0,0,0,0.08); }
  .logo { display: flex; align-items: center; gap: 10px; }
  .logo-icon { width: 32px; height: 32px; background: linear-gradient(135deg, #4A90E5, #357ABD); border-radius: 6px; display: flex; align-items: center; justify-content: center; color: white; font-weight: 700; font-size: 16px; }
  .logo-text { font-size: 16px; font-weight: 600; color: #4A90E5; }
  .main { flex: 1; display: flex; align-items: center; justify-content: center; padding: 40px 20px; }
  .card { background: #fff; border-radius: 4px; box-shadow: 0 2px 4px 0 rgba(214,228,241,0.7); padding: 48px 56px; text-align: center; max-width: 420px; width: 100%; }
  .icon { width: 72px; height: 72px; margin: 0 auto 20px; background: #EBF5FF; border-radius: 50%; display: flex; align-items: center; justify-content: center; }
  .icon svg { width: 36px; height: 36px; color: #4A90E5; }
  h1 { font-size: 20px; font-weight: 600; color: #333; margin-bottom: 6px; }
  .subtitle { font-size: 14px; color: #909399; margin-bottom: 28px; }
  .form { display: flex; flex-direction: column; gap: 14px; }
  .input { width: 100%; padding: 10px 14px; border: 1px solid #EBEEF5; border-radius: 4px; font-size: 14px; font-family: "SF Mono", "Fira Code", monospace; color: #333; background: #fff; outline: none; transition: border-color 200ms; }
  .input:focus { border-color: #4A90E5; box-shadow: 0 0 0 2px rgba(74,144,229,0.15); }
  .input::placeholder { color: #C0C4CC; font-family: "Microsoft YaHei", "PingFang SC", sans-serif; }
  .btn { width: 100%; padding: 10px 0; border: none; border-radius: 4px; background: #4A90E5; color: #fff; font-size: 15px; font-weight: 500; cursor: pointer; transition: background 200ms; }
  .btn:hover { background: #357ABD; }
  .btn:active { background: #2A6AA0; }
  .error { font-size: 13px; color: #F56C6C; margin-top: 4px; display: none; }
</style>
</head>
<body>
<header>
  <div class="logo">
    <div class="logo-icon">Tx</div>
    <span class="logo-text">Teamx Enterprise</span>
  </div>
</header>
<div class="main">
  <div class="card">
    <div class="icon">
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        <rect x="3" y="11" width="18" height="11" rx="2" ry="2"></rect>
        <path d="M7 11V7a5 5 0 0 1 10 0v4"></path>
      </svg>
    </div>
    <h1>Teamx Enterprise</h1>
    <p class="subtitle">Enter your access token to login</p>
    <div class="form">
      <input class="input" id="tokenInput" type="text" placeholder="Paste token here..." autofocus autocomplete="off" spellcheck="false">
      <button class="btn" id="loginBtn">Login</button>
      <div class="error" id="errMsg">Invalid token, please try again.</div>
    </div>
  </div>
</div>
<script>
const btn=document.getElementById("loginBtn"),inp=document.getElementById("tokenInput"),err=document.getElementById("errMsg");
function doLogin(){const t=inp.value.trim();if(!t){inp.focus();return}err.style.display="none";btn.textContent="Logging in...";btn.disabled=true;window.location.href="/?token="+encodeURIComponent(t)}
btn.addEventListener("click",doLogin);inp.addEventListener("keydown",e=>{if(e.key==="Enter")doLogin()});
</script>
</body>
</html>"##
    .to_string()
}

/// Auth helper: the request is authorized when the `teamx_ui_token` cookie
/// matches the launch token.
fn authorized(state: &S, cookies: Option<&str>) -> bool {
    match cookies {
        Some(c) => c.split(';').any(|kv| {
            let mut it = kv.trim().splitn(2, '=');
            it.next() == Some(COOKIE) && it.next() == Some(&state.token)
        }),
        None => false,
    }
}

async fn index(
    State(state): State<S>,
    Query(params): Query<HashMap0>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    // ?token=xxx sets the cookie and redirects to /.
    if let Some(t) = params.token {
        if t == state.token {
            let mut resp = Redirect::to("/").into_response();
            let val = format!("{COOKIE}={}; HttpOnly; SameSite=Strict; Path=/", state.token);
            resp.headers_mut().insert(header::SET_COOKIE, val.parse().unwrap());
            return resp;
        }
    }
    // Cookie already set → serve the page.
    if authorized(&state, headers.get(header::COOKIE).and_then(|v| v.to_str().ok())) {
        return Html(page_html()).into_response();
    }
    // No token → 401 page.
    Html(unauthorized_html()).into_response()
}

async fn login_page() -> impl IntoResponse {
    Html(login_html())
}

/// Serve the vendored xterm.js (web terminal rendering, no CDN needed).
async fn xterm_js() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/javascript")],
        XTERM_JS,
    )
}

/// Serve the vendored xterm.css.
async fn xterm_css() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/css")],
        XTERM_CSS,
    )
}

async fn not_found() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, Html(not_found_html()))
}

/// A tiny query-string map (avoids pulling in serde_urlencoded for a handful
/// of optional params).
#[derive(Deserialize, Default)]
struct HashMap0 {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    team: Option<String>,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    member: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
    /// Recent-activity window in milliseconds (Members view cards).
    #[serde(default)]
    window: Option<i64>,
}

/// Guard the API routes: requires the cookie, else 401.
#[allow(clippy::result_large_err)]
fn guard(state: &S, headers: &axum::http::HeaderMap) -> Result<(), axum::response::Response> {
    if authorized(state, headers.get(header::COOKIE).and_then(|v| v.to_str().ok())) {
        Ok(())
    } else {
        Err(Html(unauthorized_html()).into_response())
    }
}

fn extract_filters(params: &HashMap0) -> (Option<&str>, Option<&str>, Option<&str>, Option<&str>) {
    (
        params.from.as_deref(),
        params.to.as_deref(),
        params.kind.as_deref(),
        params.member.as_deref(),
    )
}

fn lock(conn: &S) -> std::sync::MutexGuard<'_, Connection> {
    conn.conn.lock().unwrap()
}

// ---------------------------------------------------------------------------
// Live member view (ENT-01): proxy to a member's opencode serve over the
// teamx tunnel. The member machine exposes `opencode serve` and registers a
// tunnel named `opencode-<member_id>`; ui connects as a tunnel consumer and
// forwards HTTP requests / WS bytes.
// ---------------------------------------------------------------------------

/// Tunnel name a member registers for their opencode serve.
fn member_tunnel(member_id: &str) -> String {
    format!("opencode-{member_id}")
}

/// Verify that `member_id` belongs to one of the owner's teams. This is a
/// second line of defense: the teamx server's tunnel forward already rejects
/// cross-team consumers, but we avoid even attempting a tunnel for members the
/// owner should not see.
fn verify_member(state: &S, member_id: &str) -> Result<(), String> {
    if member_id.is_empty() || member_id.len() > 128 {
        return Err("invalid member id".to_string());
    }
    let c = lock(state);
    for tid in &state.owner_teams {
        let n: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM members WHERE id = ?1 AND team_id = ?2 AND state NOT IN ('left','denied')",
                rusqlite::params![member_id, tid],
                |r| r.get(0),
            )
            .map_err(|e| format!("db error: {e}"))?;
        if n > 0 {
            return Ok(());
        }
    }
    Err(format!("member `{member_id}` is not in any team you own"))
}

/// Open a fresh byte stream to the member's opencode serve (via tunnel).
async fn member_stream(state: &S, member_id: &str) -> Result<crate::tunnel_client::TunnelStream, String> {
    let server_url = state.server_url.as_deref().ok_or_else(|| "live member view disabled (no server_url)".to_string())?;
    let bridge = crate::tunnel_client::open_tunnel_bridge(server_url, &member_tunnel(member_id), "")
        .await
        .map_err(|e| format!("open tunnel to {member_id}: {e}"))?;
    Ok(crate::tunnel_client::TunnelStream::new(bridge))
}

/// Take a pooled connection for `member_id`, opening a fresh one if absent.
async fn take_conn(state: &S, member_id: &str) -> Result<crate::tunnel_client::TunnelStream, String> {
    let cached = {
        let mut pool = state.pool.lock().await;
        pool.remove(member_id)
    };
    match cached {
        Some(s) if !s.is_eof() => Ok(s),
        _ => member_stream(state, member_id).await,
    }
}

/// Return a healthy connection to the pool.
async fn put_conn(state: &S, member_id: &str, stream: crate::tunnel_client::TunnelStream) {
    if stream.is_eof() {
        return;
    }
    let mut pool = state.pool.lock().await;
    // Only keep one idle connection per member (replace if a stale one lingers).
    pool.insert(member_id.to_string(), stream);
}

/// A parsed HTTP response from a member's opencode serve.
struct MemberResponse {
    status: u16,
    body: Vec<u8>,
    /// Whether the connection is still usable for a follow-up request.
    alive: bool,
}

/// Read one HTTP/1.1 response from `stream` (status line + headers + body),
/// handling Content-Length and `Transfer-Encoding: chunked`. Returns whether
/// the connection can be reused (`alive`).
async fn read_http_response<S>(stream: &mut S) -> Result<MemberResponse, String>
where
    S: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
    let mut reader = BufReader::new(&mut *stream);

    // Status line.
    let mut status_line = String::new();
    if reader.read_line(&mut status_line).await.map_err(|e| format!("read status: {e}"))? == 0 {
        return Err("connection closed before response".to_string());
    }
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(500);

    // Headers.
    let mut clen: Option<usize> = None;
    let mut chunked = false;
    let mut close = false;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await.map_err(|e| format!("read header: {e}"))? == 0 {
            return Err("connection closed during headers".to_string());
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim();
            let v = v.trim();
            if k.eq_ignore_ascii_case("content-length") {
                clen = v.parse().ok();
            } else if k.eq_ignore_ascii_case("transfer-encoding") {
                chunked = v.to_ascii_lowercase().contains("chunked");
            } else if k.eq_ignore_ascii_case("connection") {
                close = v.eq_ignore_ascii_case("close");
            }
        }
    }

    let mut body = Vec::new();
    let mut alive = !close;
    if chunked {
        loop {
            let mut sz_line = String::new();
            if reader.read_line(&mut sz_line).await.map_err(|e| format!("read chunk size: {e}"))? == 0 {
                return Err("connection closed mid-chunk".to_string());
            }
            let sz_line = sz_line.trim_end_matches(['\r', '\n']);
            let sz = usize::from_str_radix(sz_line.split(';').next().unwrap_or("0").trim(), 16).unwrap_or(0);
            if sz == 0 {
                // Consume the trailing CRLF after the last chunk.
                let mut trailer = String::new();
                let _ = reader.read_line(&mut trailer).await;
                break;
            }
            let mut chunk = vec![0u8; sz];
            reader.read_exact(&mut chunk).await.map_err(|e| format!("read chunk: {e}"))?;
            body.extend_from_slice(&chunk);
            let mut crlf = [0u8; 2];
            reader.read_exact(&mut crlf).await.map_err(|e| format!("read chunk crlf: {e}"))?;
        }
    } else if let Some(n) = clen {
        if n > 0 {
            let mut buf = vec![0u8; n];
            reader.read_exact(&mut buf).await.map_err(|e| format!("read body: {e}"))?;
            body = buf;
        }
    } else {
        // No length: read until EOF (server must close the connection).
        reader.read_to_end(&mut body).await.map_err(|e| format!("read body: {e}"))?;
        alive = false;
    }

    // If BufReader buffered extra bytes beyond this response, the connection
    // cannot be cleanly reused — drop it rather than corrupting the next reply.
    if !reader.buffer().is_empty() {
        alive = false;
    }

    Ok(MemberResponse { status, body, alive })
}

/// Send an HTTP request over a pooled member connection and read the response.
async fn member_request(
    state: &S,
    member_id: &str,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
) -> Result<(u16, Vec<u8>), String> {
    use tokio::io::AsyncWriteExt;

    verify_member(state, member_id)?;
    let mut stream = take_conn(state, member_id).await?;

    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAccept: application/json\r\nConnection: keep-alive\r\n"
    );
    if let Some(b) = body {
        req.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n", b.len()));
    }
    req.push_str("\r\n");
    stream
        .write_all(req.as_bytes())
        .await
        .map_err(|e| format!("write request: {e}"))?;
    if let Some(b) = body {
        stream.write_all(b).await.map_err(|e| format!("write body: {e}"))?;
    }

    let resp = read_http_response(&mut stream).await?;
    if resp.alive && !stream.is_eof() {
        put_conn(state, member_id, stream).await;
    }
    Ok((resp.status, resp.body))
}

/// Map a member proxy result into a JSON HTTP response.
fn member_json_response(result: Result<(u16, Vec<u8>), String>) -> axum::response::Response {
    match result {
        Ok((200, body)) => match serde_json::from_slice::<serde_json::Value>(&body) {
            Ok(v) => Json(v).into_response(),
            Err(_) => (StatusCode::BAD_GATEWAY, "bad json from member".to_string()).into_response(),
        },
        Ok((status, body)) => {
            (StatusCode::BAD_GATEWAY, format!("member returned {status}: {}", String::from_utf8_lossy(&body).chars().take(200).collect::<String>())).into_response()
        }
        Err(e) => (StatusCode::BAD_GATEWAY, e).into_response(),
    }
}

/// GET /api/members/{member_id}/sessions — proxy opencode /api/session.
async fn api_member_sessions(
    State(state): State<S>,
    Path(member_id): Path<String>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if let Err(r) = guard(&state, &headers) {
        return r;
    }
    member_json_response(member_request(&state, &member_id, "GET", "/api/session", None).await)
}

/// GET /api/members/{member_id}/session/{sid}/message — proxy opencode messages.
async fn api_member_messages(
    State(state): State<S>,
    Path((member_id, sid)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if let Err(r) = guard(&state, &headers) {
        return r;
    }
    let path = format!("/api/session/{sid}/message");
    member_json_response(member_request(&state, &member_id, "GET", &path, None).await)
}

/// POST /api/members/{member_id}/session/{sid}/abort — proxy opencode session abort.
async fn api_member_abort(
    State(state): State<S>,
    Path((member_id, sid)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if let Err(r) = guard(&state, &headers) {
        return r;
    }
    let path = format!("/api/session/{sid}/abort");
    member_json_response(member_request(&state, &member_id, "POST", &path, None).await)
}

/// POST /api/members/{member_id}/session/{sid}/prompt — proxy opencode prompt.
async fn api_member_prompt(
    State(state): State<S>,
    Path((member_id, sid)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if let Err(r) = guard(&state, &headers) {
        return r;
    }
    let path = format!("/api/session/{sid}/message");
    member_json_response(member_request(&state, &member_id, "POST", &path, Some(&body)).await)
}

/// GET /api/members/{member_id}/shell — proxy opencode /pty/shells.
async fn api_member_shells(
    State(state): State<S>,
    Path(member_id): Path<String>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if let Err(r) = guard(&state, &headers) {
        return r;
    }
    member_json_response(member_request(&state, &member_id, "GET", "/pty/shells", None).await)
}

/// POST /api/members/{member_id}/pty — proxy opencode POST /pty (create a shell).
async fn api_member_pty(
    State(state): State<S>,
    Path(member_id): Path<String>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    if let Err(r) = guard(&state, &headers) {
        return r;
    }
    member_json_response(member_request(&state, &member_id, "POST", "/pty", Some(&body)).await)
}

/// WebSocket endpoint that bridges the browser's xterm.js to the member's
/// opencode PTY (`/api/pty/{ptyID}/connect`) over the teamx tunnel.
/// Browser bytes → ui → TunnelStream → member opencode WS → member shell.
async fn ws_member_pty(
    State(state): State<S>,
    Path((member_id, pty_id)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    ws: WebSocketUpgrade,
) -> axum::response::Response {
    // Authorize with the ui token (cookie or ?token=).
    if !authorized(&state, headers.get(header::COOKIE).and_then(|v| v.to_str().ok())) {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    // Verify the member belongs to the owner's teams before opening a tunnel.
    if let Err(e) = verify_member(&state, &member_id) {
        return (StatusCode::FORBIDDEN, e).into_response();
    }
    let server_url = match state.server_url.as_deref() {
        Some(u) => u.to_string(),
        None => return (StatusCode::BAD_GATEWAY, "live member view disabled (no server_url)").into_response(),
    };
    ws.on_upgrade(move |socket| {
        bridge_pty(socket, server_url, member_id, pty_id)
    })
}

/// Bridge the browser WS and the member's opencode PTY WS, relaying bytes.
async fn bridge_pty(
    browser: WebSocket,
    server_url: String,
    member_id: String,
    pty_id: String,
) {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as TMessage;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    // 1. Open a byte stream to the member's opencode (via tunnel).
    let bridge = match crate::tunnel_client::open_tunnel_bridge(&server_url, &member_tunnel(&member_id), "").await {
        Ok(b) => b,
        Err(_) => return,
    };
    let stream = crate::tunnel_client::TunnelStream::new(bridge);

    // 2. Upgrade it to a WS toward opencode's PTY connect endpoint.
    let url = format!("ws://127.0.0.1/api/pty/{pty_id}/connect");
    let mut req = match url.into_client_request() {
        Ok(r) => r,
        Err(_) => return,
    };
    req.headers_mut().insert("Host", "127.0.0.1".parse().unwrap());
    let (member_ws, _) = match tokio_tungstenite::client_async(req, stream).await {
        Ok(v) => v,
        Err(_) => return,
    };

    // 3. Relay bytes both ways, with a heartbeat so idle terminals don't get
    // silently dropped by middleboxes.
    let (mut b_sink, mut b_stream) = browser.split();
    let (mut m_sink, mut m_stream) = member_ws.split();

    let to_member = async {
        while let Some(Ok(msg)) = b_stream.next().await {
            match msg {
                WsMessage::Text(t) => {
                    if m_sink.send(TMessage::Text(t.to_string().into())).await.is_err() { break; }
                }
                WsMessage::Binary(b) => {
                    if m_sink.send(TMessage::Binary(b.to_vec().into())).await.is_err() { break; }
                }
                WsMessage::Ping(p) => {
                    if m_sink.send(TMessage::Ping(p.to_vec().into())).await.is_err() { break; }
                }
                WsMessage::Pong(p) => {
                    if m_sink.send(TMessage::Pong(p.to_vec().into())).await.is_err() { break; }
                }
                WsMessage::Close(_) => break,
            }
        }
        let _ = m_sink.close().await;
    };

    // Relay member → browser, interleaving a periodic ping so an idle terminal
    // is not reaped by the tunnel/server.
    let to_browser = async {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(25));
        loop {
            tokio::select! {
                msg = m_stream.next() => {
                    match msg {
                        Some(Ok(TMessage::Text(t))) => {
                            if b_sink.send(WsMessage::Text(t.to_string().into())).await.is_err() { break; }
                        }
                        Some(Ok(TMessage::Binary(b))) => {
                            if b_sink.send(WsMessage::Binary(b.to_vec().into())).await.is_err() { break; }
                        }
                        Some(Ok(TMessage::Ping(p))) => {
                            if b_sink.send(WsMessage::Ping(axum::body::Bytes::from(p))).await.is_err() { break; }
                        }
                        Some(Ok(TMessage::Pong(p))) => {
                            if b_sink.send(WsMessage::Pong(axum::body::Bytes::from(p))).await.is_err() { break; }
                        }
                        Some(Ok(TMessage::Close(_))) | Some(Ok(TMessage::Frame(_))) => {}
                        Some(Err(_)) | None => break,
                    }
                }
                _ = interval.tick() => {
                    if b_sink.send(WsMessage::Ping(axum::body::Bytes::new())).await.is_err() { break; }
                }
            }
        }
    };

    let _ = tokio::join!(to_member, to_browser);
}

async fn api_teams(State(state): State<S>, headers: axum::http::HeaderMap) -> axum::response::Response {
    if let Err(r) = guard(&state, &headers) {
        return r;
    }
    let c = lock(&state);
    let teams: Vec<serde_json::Value> = c
        .prepare(
            "SELECT t.id, t.name, t.state FROM teams t WHERE t.state NOT IN ('destroyed') ORDER BY t.created_at",
        )
        .and_then(|mut stmt| {
            let rows = stmt.query_map([], |r| Ok(json!({ "id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)? })))?;
            rows.collect()
        })
        .unwrap_or_default();
    Json(serde_json::json!({ "teams": teams })).into_response()
}

// overview uses the summary RPC shape (overall + human + ai).
async fn api_overview(State(state): State<S>, Query(params): Query<HashMap0>, headers: axum::http::HeaderMap) -> axum::response::Response {
    if let Err(r) = guard(&state, &headers) {
        return r;
    }
    let team = params.team.as_deref();
    let (from, to, kind, member) = extract_filters(&params);
    let c = lock(&state);
    match crate::activity::summary(&c, team, member, None, kind, from, to) {
        Ok(v) => Json(serde_json::json!({ "overview": v })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    }
}

async fn api_by_member(State(state): State<S>, Query(params): Query<HashMap0>, headers: axum::http::HeaderMap) -> axum::response::Response {
    if let Err(r) = guard(&state, &headers) {
        return r;
    }
    let team = params.team.as_deref();
    let (from, to, kind, member) = extract_filters(&params);
    let c = lock(&state);
    match crate::activity::by_member(&c, team, member, None, kind, from, to) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    }
}

async fn api_by_node(State(state): State<S>, Query(params): Query<HashMap0>, headers: axum::http::HeaderMap) -> axum::response::Response {
    if let Err(r) = guard(&state, &headers) {
        return r;
    }
    let team = params.team.as_deref();
    let (from, to, kind, member) = extract_filters(&params);
    let c = lock(&state);
    match crate::activity::by_node(&c, team, member, None, kind, from, to) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    }
}

async fn api_tools(State(state): State<S>, Query(params): Query<HashMap0>, headers: axum::http::HeaderMap) -> axum::response::Response {
    if let Err(r) = guard(&state, &headers) {
        return r;
    }
    let team = params.team.as_deref();
    let (from, to, kind, member) = extract_filters(&params);
    let _ = kind;
    let c = lock(&state);
    match crate::activity::tools(&c, team, member, None, from, to) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    }
}

async fn api_files(State(state): State<S>, Query(params): Query<HashMap0>, headers: axum::http::HeaderMap) -> axum::response::Response {
    if let Err(r) = guard(&state, &headers) {
        return r;
    }
    let team = params.team.as_deref();
    let (from, to, kind, member) = extract_filters(&params);
    let _ = kind;
    let c = lock(&state);
    match crate::activity::files(&c, team, member, None, from, to) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    }
}

async fn api_rows(State(state): State<S>, Query(params): Query<HashMap0>, headers: axum::http::HeaderMap) -> axum::response::Response {
    if let Err(r) = guard(&state, &headers) {
        return r;
    }
    let team = params.team.as_deref();
    let (from, to, kind, member) = extract_filters(&params);
    let limit = params.limit.unwrap_or(100);
    let c = lock(&state);
    match crate::activity::rows(&c, team, member, None, kind, from, to, limit) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    }
}

async fn api_human(State(state): State<S>, Query(params): Query<HashMap0>, headers: axum::http::HeaderMap) -> axum::response::Response {
    if let Err(r) = guard(&state, &headers) {
        return r;
    }
    let team = params.team.as_deref();
    let (from, to, _kind, member) = extract_filters(&params);
    let limit = params.limit.unwrap_or(100);
    let c = lock(&state);
    match crate::activity::human_rows(&c, team, member, None, from, to, limit) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    }
}

// ---------------------------------------------------------------------------
// Enterprise dashboard v2 API: goal-centric views.
// ---------------------------------------------------------------------------

/// Goal-centric overview: all of the team's goals (current + history) with
/// their lifecycle events. The team's current goal is `teams.goal_id`; closed
/// goals have `closed_at` set and appear below the current one.
async fn api_goal(State(state): State<S>, Query(params): Query<HashMap0>, headers: axum::http::HeaderMap) -> axum::response::Response {
    if let Err(r) = guard(&state, &headers) {
        return r;
    }
    let team = match params.team.as_deref() {
        Some(t) if !t.is_empty() => t,
        // "All" mode: no single goal lifecycle to show; return an empty result
        // and let the frontend prompt the user to pick a team.
        _ => return Json(json!({ "team_id": "", "current": null, "goals": [], "lifecycle": [] })).into_response(),
    };
    let c = lock(&state);

    // Current goal row (teams.goal_id -> goals).
    let current_goal = match c.query_row(
        "SELECT g.id, g.title, g.body, g.state, g.created_at, g.updated_at, g.closed_at
         FROM goals g JOIN teams t ON t.goal_id = g.id WHERE t.id = ?1",
        [team],
        |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "title": r.get::<_, String>(1)?,
                "body": r.get::<_, Option<String>>(2)?,
                "state": r.get::<_, String>(3)?,
                "created_at": r.get::<_, String>(4)?,
                "updated_at": r.get::<_, String>(5)?,
                "closed_at": r.get::<_, Option<String>>(6)?,
            }))
        },
    ) {
        Ok(g) => Some(g),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response();
        }
    };

    // All goals for the team (history, newest first), with current flag.
    let mut stmt = match c.prepare(
        "SELECT g.id, g.title, g.body, g.state, g.created_at, g.updated_at, g.closed_at
         FROM goals g WHERE g.team_id = ?1 ORDER BY g.created_at ASC",
    ) {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    };
    let goals = match stmt.query_map([team], |r| {
        Ok(json!({
            "id": r.get::<_, String>(0)?,
            "title": r.get::<_, String>(1)?,
            "body": r.get::<_, Option<String>>(2)?,
            "state": r.get::<_, String>(3)?,
            "created_at": r.get::<_, String>(4)?,
            "updated_at": r.get::<_, String>(5)?,
            "closed_at": r.get::<_, Option<String>>(6)?,
        }))
    }) {
        Ok(rows) => match rows.collect::<Result<Vec<_>, _>>() {
            Ok(v) => v,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
        },
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    };

    // Lifecycle events from the events ledger.
    let mut stmt = match c.prepare(
        "SELECT seq, member_id, type, payload_json, created_at
         FROM events WHERE team_id = ?1 AND type IN
           ('goal.set','goal.updated','goal.shared','goal.state_changed','goal.achieved','team.completed')
         ORDER BY seq ASC",
    ) {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    };
    let lifecycle = match stmt.query_map([team], |r| {
        let payload: Option<String> = r.get(3)?;
        let payload_v: serde_json::Value = payload
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(serde_json::Value::Null);
        Ok(json!({
            "seq": r.get::<_, i64>(0)?,
            "member_id": r.get::<_, Option<String>>(1)?,
            "type": r.get::<_, String>(2)?,
            "payload": payload_v,
            "created_at": r.get::<_, String>(4)?,
        }))
    }) {
        Ok(rows) => match rows.collect::<Result<Vec<_>, _>>() {
            Ok(v) => v,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
        },
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    };

    Json(json!({
        "team_id": team,
        "current": current_goal,
        "goals": goals,
        "lifecycle": lifecycle,
    }))
    .into_response()
}

/// Members view: every member with role/state + per-member activity aggregates
/// (duration/tokens/cost) across the team.
async fn api_members(State(state): State<S>, Query(params): Query<HashMap0>, headers: axum::http::HeaderMap) -> axum::response::Response {
    if let Err(r) = guard(&state, &headers) {
        return r;
    }
    let team = params.team.as_deref();
    let c = lock(&state);
    let (sql, team_param): (&str, Vec<&str>) = match team {
        Some(t) if !t.is_empty() => (
            "SELECT m.id, m.display_name, m.role, m.state, m.joined_at, m.last_seen_at
             FROM members m WHERE m.team_id = ?1 AND m.state NOT IN ('left','denied')
             ORDER BY m.joined_at ASC",
            vec![t],
        ),
        _ => (
            "SELECT m.id, m.display_name, m.role, m.state, m.joined_at, m.last_seen_at
             FROM members m WHERE m.state NOT IN ('left','denied')
             ORDER BY m.joined_at ASC",
            vec![],
        ),
    };
    let mut stmt = match c.prepare(sql) {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    };
    let rows = match stmt.query_map(rusqlite::params_from_iter(team_param.iter()), |r| {
        Ok(json!({
            "id": r.get::<_, String>(0)?,
            "display_name": r.get::<_, String>(1)?,
            "role": r.get::<_, Option<String>>(2)?,
            "state": r.get::<_, String>(3)?,
            "joined_at": r.get::<_, String>(4)?,
            "last_seen_at": r.get::<_, Option<String>>(5)?,
        }))
    }) {
        Ok(rows) => match rows.collect::<Result<Vec<_>, _>>() {
            Ok(v) => v,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
        },
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    };

    // Per-member aggregates from activity.
    let by_member = crate::activity::by_member(&c, team, None, None, None, None, None)
        .unwrap_or(serde_json::json!([]));
    let agg: std::collections::HashMap<String, &serde_json::Value> = by_member
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    m.get("member_id")
                        .and_then(|id| id.as_str())
                        .map(|id| (id.to_string(), m))
                })
                .collect()
        })
        .unwrap_or_default();
    let presence_map = crate::activity::presence_for_members(&c).unwrap_or_default();
    let mut out = Vec::new();
    for m in &rows {
        let mid = m["id"].as_str().unwrap_or("").to_string();
        let a = agg
            .get(&mid)
            .cloned()
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        let p = presence_map
            .get(&mid)
            .cloned()
            .unwrap_or_default();
        let mut row = m.clone();
        if let Some(o) = row.as_object_mut() {
            o.insert("activity".to_string(), a);
            o.insert("presence".to_string(), serde_json::json!(p));
        }
        out.push(row);
    }
    let window_ms = params
        .window
        .unwrap_or(crate::activity::RECENT_WINDOW_MS)
        .clamp(60_000, 24 * 3600 * 1000);
    let recent = crate::activity::recent_activity(&c, team, window_ms)
        .unwrap_or(serde_json::json!({}));
    Json(json!({ "members": out, "recent": recent })).into_response()
}

/// KanBan view: one column per member, each showing that member's work
/// sessions (tasks) newest-first + a compact activity summary. Rows are the
/// member's `work_session` activity (a bounded work segment), which is the
/// closest unit to a "task" in the activity ledger.
async fn api_kanban(State(state): State<S>, Query(params): Query<HashMap0>, headers: axum::http::HeaderMap) -> axum::response::Response {
    if let Err(r) = guard(&state, &headers) {
        return r;
    }
    let team = params.team.as_deref();
    let (from, to, _kind, member) = extract_filters(&params);
    let limit = params.limit.unwrap_or(30);
    let c = lock(&state);

    // Members of the team (or all teams when team is empty/All).
    let (sql, team_param): (&str, Vec<&str>) = match team {
        Some(t) if !t.is_empty() => (
            "SELECT m.id, m.display_name, m.role, m.state FROM members m
             WHERE m.team_id = ?1 AND m.state NOT IN ('left','denied')
             ORDER BY m.joined_at ASC",
            vec![t],
        ),
        _ => (
            "SELECT m.id, m.display_name, m.role, m.state FROM members m
             WHERE m.state NOT IN ('left','denied')
             ORDER BY m.joined_at ASC",
            vec![],
        ),
    };
    let mut stmt = match c.prepare(sql) {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    };
    let members = match stmt.query_map(rusqlite::params_from_iter(team_param.iter()), |r| {
        Ok(json!({
            "id": r.get::<_, String>(0)?,
            "display_name": r.get::<_, String>(1)?,
            "role": r.get::<_, Option<String>>(2)?,
            "state": r.get::<_, String>(3)?,
        }))
    }) {
        Ok(rows) => match rows.collect::<Result<Vec<_>, _>>() {
            Ok(v) => v,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
        },
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    };

    // Per-member aggregates (duration/tokens/cost/count) for the column summary.
    let by_member = crate::activity::by_member(&c, team, member, None, None, from, to)
        .unwrap_or(serde_json::json!([]));
    let mut agg: std::collections::HashMap<String, serde_json::Value> = std::collections::HashMap::new();
    if let Some(arr) = by_member.as_array() {
        for m in arr {
            if let Some(mid) = m.get("member_id").and_then(|x| x.as_str()) {
                agg.insert(mid.to_string(), m.clone());
            }
        }
    }

    // Each member's work sessions (tasks), newest first.
    let mut out = Vec::new();
    for m in &members {
        let mid = m["id"].as_str().unwrap_or("").to_string();
        let tasks = crate::activity::rows(&c, team, Some(&mid), None, Some("work_session"), from, to, limit)
            .unwrap_or(serde_json::json!([]));
        let a = agg.get(&mid).cloned().unwrap_or_else(|| serde_json::json!({}));
        out.push(json!({
            "member_id": mid,
            "display_name": m["display_name"],
            "role": m["role"],
            "state": m["state"],
            "tasks": tasks,
            "summary": a,
        }));
    }
    Json(json!({ "columns": out })).into_response()
}

/// Timeline (Gantt) view data: per-member work segments + event points.
async fn api_timeline(State(state): State<S>, Query(params): Query<HashMap0>, headers: axum::http::HeaderMap) -> axum::response::Response {
    if let Err(r) = guard(&state, &headers) {
        return r;
    }
    let team = params.team.as_deref();
    let (from, to, _kind, member) = extract_filters(&params);
    let c = lock(&state);
    match crate::activity::timeline(&c, team, member, from, to) {
        Ok(v) => Json(json!({ "items": v })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    }
}
