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
    extract::{Query, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Redirect},
    routing::get,
    Json, Router,
};
use axum_server::tls_rustls::RustlsConfig;
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::json;

use crate::cli::UiCmd;
use crate::pki;

/// Launch the dashboard. Blocks until the server stops (Ctrl+C / kill).
pub fn ui(cmd: &UiCmd) -> Result<(), String> {
    let db_path = cmd.db.clone().unwrap_or_else(crate::db::default_db_path);
    let conn = crate::db::open(&db_path).map_err(|e| format!("cannot open database {db_path:?}: {e}"))?;
    crate::db::migrate(&conn).map_err(|e| format!("schema init failed: {e}"))?;

    // Owner check: the session must own at least one non-destroyed team.
    let conn = Arc::new(std::sync::Mutex::new(conn));
    {
        let c = conn.lock().unwrap();
        let owners: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM members m JOIN teams t ON t.id = m.team_id
                 WHERE m.session_key = ?1 AND m.state NOT IN ('left','denied') AND t.state NOT IN ('destroyed') AND t.owner_member_id = m.id",
                [&cmd.session],
                |r| r.get(0),
            )
            .map_err(|e| format!("owner check failed: {e}"))?;
        if owners == 0 {
            return Err(format!(
                "`teamx ui` is owner-only: session `{}` does not own any team",
                cmd.session
            ));
        }
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

    let state = Arc::new(AppState {
        conn,
        token: token.clone(),
    });

    let app = Router::new()
        .route("/", get(index))
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
<title>Teamx Enterprise</title>
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link href="https://fonts.googleapis.com/css2?family=Fira+Code:wght@400;500;600&family=Fira+Sans:wght@300;400;500;600;700&display=swap" rel="stylesheet">
<style>
  :root {
    color-scheme: light dark;
    --primary: #1E40AF;
    --primary-hover: #1D4ED8;
    --on-primary: #FFFFFF;
    --secondary: #3B82F6;
    --accent: #D97706;
    --on-accent: #FFFFFF;
    --bg: #F8FAFC;
    --fg: #1E293B;
    --card: #FFFFFF;
    --card-fg: #1E293B;
    --muted: #E9EEF6;
    --muted-fg: #475569;
    --border: #E2E8F0;
    --destructive: #DC2626;
    --ring: #1E40AF;
    --radius: 10px;
    --shadow: 0 1px 3px rgba(16, 24, 40, 0.08), 0 1px 2px rgba(16, 24, 40, 0.04);
    --shadow-hover: 0 4px 12px rgba(16, 24, 40, 0.12);
    --font-sans: "Fira Sans", -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    --font-mono: "Fira Code", ui-monospace, SFMono-Regular, Menlo, monospace;
    --transition: 150ms ease;
  }
  * { box-sizing: border-box; }
  body { font-family: var(--font-sans); font-size: 14px; line-height: 1.55; margin: 0; background: var(--bg); color: var(--fg); -webkit-font-smoothing: antialiased; }
  header { position: sticky; top: 0; z-index: 100; display: flex; align-items: center; gap: 24px; padding: 0 24px; height: 56px; background: var(--card); border-bottom: 1px solid var(--border); box-shadow: var(--shadow); }
  header h1 { font-size: 15px; font-weight: 600; margin: 0; color: var(--fg); letter-spacing: -0.01em; }
  header h1 .mark { color: var(--primary); font-weight: 700; }
  nav { display: flex; gap: 2px; }
  nav button { background: none; border: none; font: inherit; font-size: 13.5px; font-weight: 500; color: var(--muted-fg); padding: 8px 14px; border-radius: 8px; cursor: pointer; transition: background var(--transition), color var(--transition); }
  nav button:hover { background: var(--muted); color: var(--fg); }
  nav button.active { background: var(--primary); color: var(--on-primary); box-shadow: 0 1px 3px rgba(30,64,175,0.35); }
  nav button:focus-visible, button:focus-visible, select:focus-visible, input:focus-visible { outline: 2px solid var(--ring); outline-offset: 2px; }
  .toolbar { display: flex; justify-content: space-between; align-items: center; gap: 14px; flex-wrap: wrap; padding: 12px 24px; background: var(--card); border-bottom: 1px solid var(--border); }
  .toolbar-left { display: flex; gap: 14px; flex-wrap: wrap; align-items: flex-end; }
  .toolbar-right { margin-left: auto; }
  /* Kibana-style time picker */
  .timepicker { position: relative; }
  .time-btn { display: inline-flex; align-items: center; gap: 8px; padding: 7px 12px; border: 1px solid var(--border); border-radius: 8px; background: var(--card); color: var(--fg); font-size: 13px; font-weight: 500; cursor: pointer; transition: all var(--transition); }
  .time-btn:hover { border-color: var(--secondary); box-shadow: var(--shadow); }
  .tp-icon { color: var(--primary); font-size: 14px; }
  .tp-caret { color: var(--muted-fg); font-size: 10px; }
  .tp-popover { position: absolute; right: 0; top: calc(100% + 6px); width: 320px; background: var(--card); border: 1px solid var(--border); border-radius: 10px; box-shadow: var(--shadow-hover); z-index: 200; display: none; overflow: hidden; }
  .tp-popover.open { display: block; }
  .tp-tabs { display: flex; border-bottom: 1px solid var(--border); }
  .tp-tab { flex: 1; padding: 9px 0; border: none; background: none; color: var(--muted-fg); font-size: 13px; font-weight: 500; cursor: pointer; border-bottom: 2px solid transparent; }
  .tp-tab.active { color: var(--primary); border-bottom-color: var(--primary); }
  .tp-pane { display: none; padding: 10px; }
  .tp-pane.active { display: block; }
  .tp-quick { display: block; width: 100%; text-align: left; padding: 8px 12px; border: none; background: none; color: var(--fg); font-size: 13px; border-radius: 6px; cursor: pointer; transition: background var(--transition); }
  .tp-quick:hover { background: var(--muted); }
  .tp-quick.sel { color: var(--primary); font-weight: 600; }
  .tp-pane label { display: flex; flex-direction: column; font-size: 11px; font-weight: 500; color: var(--muted-fg); gap: 4px; margin-bottom: 10px; }
  .filters label { display: flex; flex-direction: column; font-size: 11px; font-weight: 500; color: var(--muted-fg); gap: 4px; letter-spacing: 0.02em; }
  select, input { font: inherit; font-size: 13px; padding: 7px 10px; border: 1px solid var(--border); border-radius: 8px; background: var(--card); color: var(--fg); transition: border-color var(--transition), box-shadow var(--transition); }
  select:hover, input:hover { border-color: var(--secondary); }
  select:focus, input:focus { outline: none; border-color: var(--ring); box-shadow: 0 0 0 3px rgba(30,64,175,0.15); }
  button { font: inherit; }
  .btn { padding: 8px 16px; border: 1px solid var(--border); border-radius: 8px; background: var(--card); color: var(--fg); font-weight: 500; cursor: pointer; transition: all var(--transition); }
  .btn:hover { border-color: var(--primary); color: var(--primary); box-shadow: var(--shadow); }
  .btn-primary { background: var(--primary); border-color: var(--primary); color: var(--on-primary); }
  .btn-primary:hover { background: var(--primary-hover); color: var(--on-primary); border-color: var(--primary-hover); }
  .container { max-width: 1280px; margin: 0 auto; padding: 24px; }
  .view { display: none; }
  .view.active { display: block; animation: fadein 200ms ease; }
  @keyframes fadein { from { opacity: 0; transform: translateY(4px); } to { opacity: 1; transform: none; } }
  .cards { display: grid; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); gap: 14px; margin-bottom: 20px; }
  .card { background: var(--card); border: 1px solid var(--border); border-radius: var(--radius); padding: 14px 16px; box-shadow: var(--shadow); transition: transform var(--transition), box-shadow var(--transition); }
  .card:hover { transform: translateY(-1px); box-shadow: var(--shadow-hover); }
  .card .label { font-size: 11.5px; font-weight: 500; color: var(--muted-fg); text-transform: uppercase; letter-spacing: 0.04em; }
  .card .value { font-family: var(--font-mono); font-size: 21px; font-weight: 600; margin-top: 4px; color: var(--fg); letter-spacing: -0.01em; }
  .card .value .unit { font-size: 12px; font-weight: 400; color: var(--muted-fg); }
  .grid2 { display: grid; grid-template-columns: 1fr 1fr; gap: 16px; margin-top: 20px; }
  .charts-row { display: grid; grid-template-columns: 1fr 1fr; gap: 16px; margin-top: 20px; }
  @media (max-width: 900px) { .grid2 { grid-template-columns: 1fr; } .charts-row { grid-template-columns: 1fr; } }
  .chart-box { display: flex; align-items: center; justify-content: center; min-height: 140px; padding: 2px 0; }
  .chart-box svg { max-width: 150px; max-height: 150px; }
  .chart-legend { display: flex; flex-wrap: wrap; gap: 10px; justify-content: center; font-size: 12px; color: var(--muted-fg); margin-top: 2px; }
  .chart-legend .sw { display: inline-block; width: 10px; height: 10px; border-radius: 3px; margin-right: 5px; vertical-align: -1px; }
  .hbar-row { display: flex; align-items: center; gap: 10px; margin-bottom: 9px; }
  .hbar-label { width: 92px; flex-shrink: 0; font-size: 12px; color: var(--fg); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; text-align: right; }
  .hbar-track { flex: 1; height: 16px; background: var(--muted); border-radius: 4px; overflow: hidden; }
  .hbar-fill { height: 100%; border-radius: 4px; min-width: 2px; transition: width 400ms ease; }
  .hbar-val { font-size: 11.5px; color: var(--muted-fg); width: 74px; flex-shrink: 0; font-family: var(--font-mono); }
  .hbar-title { font-size: 11px; margin-bottom: 8px; text-transform: uppercase; letter-spacing: 0.04em; }
  section.panel { background: var(--card); border: 1px solid var(--border); border-radius: var(--radius); padding: 18px; margin-bottom: 20px; box-shadow: var(--shadow); }
  h2 { font-size: 13px; font-weight: 600; margin: 0 0 14px; color: var(--fg); letter-spacing: 0.02em; text-transform: uppercase; }
  h2 .count { font-family: var(--font-mono); font-weight: 500; color: var(--muted-fg); margin-left: 6px; }
  table { width: 100%; border-collapse: collapse; font-size: 13px; }
  th, td { text-align: left; padding: 8px 10px; border-bottom: 1px solid var(--border); vertical-align: middle; }
  tbody tr { transition: background var(--transition); }
  tbody tr:hover { background: var(--muted); }
  th { color: var(--muted-fg); font-weight: 600; font-size: 12px; text-transform: uppercase; letter-spacing: 0.03em; }
  .badge { display: inline-block; padding: 2px 9px; border-radius: 999px; font-size: 11.5px; font-weight: 600; line-height: 1.5; }
  .badge.proposed, .badge.blocked { background: #FEF3C7; color: #92400E; }
  .badge.shared, .badge.in_progress, .badge.active { background: #DBEAFE; color: #1E40AF; }
  .badge.achieved, .badge.completed, .badge.closed { background: #DCFCE7; color: #166534; }
  .badge.refining { background: #F3E8FF; color: #7E22CE; }
  .badge.owner { background: #DBEAFE; color: #1E40AF; }
  .badge.contributor { background: #DCFCE7; color: #166534; }
  .badge.reviewer { background: #F3E8FF; color: #7E22CE; }
  .badge.observer { background: #FFEDD5; color: #C2410C; }
  .badge.idle, .badge.pending { background: #F1F5F9; color: #475569; }
  .badge.waiting { background: #FEF3C7; color: #92400E; }
  .badge.left { background: #FEE2E2; color: #B91C1C; }
  .detail { font-family: var(--font-mono); font-size: 11px; color: var(--muted-fg); white-space: pre-wrap; word-break: break-all; max-width: 480px; }
  .kanban { display: grid; grid-template-columns: repeat(auto-fit, minmax(230px, 1fr)); gap: 16px; }
  .kb-col { background: var(--muted); border: 1px solid var(--border); border-radius: var(--radius); padding: 12px; min-height: 140px; }
  .kb-col h3 { margin: 0 0 10px; font-size: 12.5px; font-weight: 600; display: flex; justify-content: space-between; align-items: center; color: var(--fg); text-transform: uppercase; letter-spacing: 0.03em; }
  .kb-col-head { padding: 2px 4px 10px; border-bottom: 1px solid var(--border); margin-bottom: 10px; }
  .kb-col-title { font-size: 14px; font-weight: 600; color: var(--fg); display: flex; align-items: center; gap: 8px; }
  .kb-col-meta { font-size: 11px; margin-top: 4px; }
  .kb-empty { padding: 12px; text-align: center; }
  .kb-card { background: var(--card); border: 1px solid var(--border); border-radius: 8px; padding: 9px 11px; margin-bottom: 8px; font-size: 12px; box-shadow: var(--shadow); transition: transform var(--transition), box-shadow var(--transition); }
  .kb-card:hover { transform: translateY(-1px); box-shadow: var(--shadow-hover); }
  .kb-head { display: flex; justify-content: space-between; align-items: center; gap: 6px; margin-bottom: 5px; }
  .kb-who { font-weight: 600; font-size: 12px; color: var(--fg); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .kb-body { font-size: 12px; color: var(--fg); line-height: 1.45; word-break: break-word; margin-bottom: 5px; }
  .kb-body .tool-name { font-weight: 600; color: var(--primary); }
  .kb-body .cost { color: var(--accent); font-weight: 600; font-family: var(--font-mono); }
  .kb-meta { font-size: 11px; color: var(--muted-fg); font-family: var(--font-mono); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .kb-card { position: relative; }
  .kb-tip { display: none; position: absolute; bottom: calc(100% + 10px); left: 50%; transform: translateX(-50%); z-index: 400; min-width: 320px; max-width: 460px; background: var(--fg); color: var(--bg); border-radius: 12px; padding: 14px 16px; box-shadow: 0 8px 24px rgba(0,0,0,0.25); font-size: 13px; line-height: 1.6; }
  .dark .kb-tip { background: #1c2128; color: #e6edf3; border: 1px solid var(--border); }
  .kb-tip .tip-title { font-weight: 600; margin-bottom: 8px; display: flex; justify-content: space-between; align-items: center; gap: 8px; font-size: 13px; }
  .kb-desc { font-size: 13.5px; font-weight: 500; color: var(--bg); background: rgba(255,255,255,0.12); border-radius: 8px; padding: 8px 10px; margin-bottom: 8px; }
  .dark .kb-desc { color: #e6edf3; background: rgba(255,255,255,0.06); }
  .kb-tip table { border-collapse: collapse; font-size: 12.5px; }
  .kb-tip td { padding: 3px 10px 3px 0; vertical-align: top; }
  .kb-tip td.k { color: var(--muted-fg); white-space: nowrap; font-weight: 500; }
  .kb-tip td.v { word-break: break-all; font-family: var(--font-mono); font-size: 12px; }
  .kb-card:hover .kb-tip, .kb-card:focus .kb-tip { display: block; }
  .gantt { overflow-x: auto; background: var(--card); border: 1px solid var(--border); border-radius: var(--radius); padding: 14px; }
  .gantt-svg { display: block; max-width: none; }
  .gantt-legend { display: flex; gap: 14px; flex-wrap: wrap; font-size: 11.5px; color: var(--muted-fg); margin-bottom: 10px; }
  .gantt-legend .sw { width: 12px; height: 8px; border-radius: 2px; display: inline-block; margin-right: 5px; vertical-align: 0; }
  .gantt-row { display: flex; align-items: center; height: 42px; border-bottom: 1px solid var(--border); }
  .gantt-label { width: 150px; flex-shrink: 0; font-size: 12px; font-weight: 500; padding: 0 10px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--fg); }
  .gantt-track { position: relative; flex: 1; height: 100%; min-width: 420px; }
  .gantt-gridline { position: absolute; top: 0; bottom: 0; width: 1px; background: var(--border); opacity: 0.5; pointer-events: none; }
  .gantt-bar { position: absolute; height: 22px; top: 10px; border-radius: 5px; opacity: 0.92; min-width: 2px; cursor: pointer; transition: opacity var(--transition), transform var(--transition); box-shadow: 0 1px 3px rgba(0,0,0,0.15); }
  .gantt-bar:hover { opacity: 1; transform: translateY(-1px); z-index: 5; }
  .gantt-bar.work { background: linear-gradient(90deg, var(--primary), var(--secondary)); }
  .gantt-bar.tool { background: #8B5CF6; }
  .gantt-bar.step { background: #D97706; }
  .gantt-bar.human { background: #059669; }
  .gantt-point { position: absolute; top: 14px; width: 12px; height: 12px; border-radius: 50%; margin-left: -6px; cursor: pointer; transition: transform var(--transition); border: 2px solid var(--card); box-shadow: 0 0 0 1px var(--border); z-index: 2; }
  .gantt-point:hover { transform: scale(1.7); z-index: 6; }
  .gantt-point.tool { background: #8B5CF6; }
  .gantt-point.step { background: #D97706; }
  .gantt-point.human { background: #059669; }
  .gantt-tip { display: none; position: absolute; bottom: calc(100% + 8px); left: 50%; transform: translateX(-50%); z-index: 400; background: var(--fg); color: var(--bg); border-radius: 8px; padding: 8px 10px; box-shadow: var(--shadow-hover); font-size: 11px; line-height: 1.55; white-space: nowrap; max-width: 340px; }
  .dark .gantt-tip { background: #1c2128; color: #e6edf3; border: 1px solid var(--border); }
  .gantt-tip .mono { font-family: var(--font-mono); }
  .gantt-bar:hover .gantt-tip, .gantt-point:hover .gantt-tip, .gantt-bar:focus .gantt-tip, .gantt-point:focus .gantt-tip { display: block; }
  .gantt-axis { position: relative; height: 22px; margin-left: 150px; font-size: 10.5px; color: var(--muted-fg); font-family: var(--font-mono); margin-bottom: 4px; }
  .gantt-axis span { position: absolute; transform: translateX(-50%); white-space: nowrap; }
  .lifecycle { position: relative; background: var(--card); border: 1px solid var(--border); border-radius: var(--radius); padding: 24px 20px 18px; margin-bottom: 20px; box-shadow: var(--shadow); }
  .lc-track { display: flex; align-items: flex-start; position: relative; }
  .lc-step { flex: 1; text-align: center; position: relative; min-width: 90px; z-index: 1; }
  /* connector: from the previous dot center to this dot center */
  .lc-step::before { content: ""; position: absolute; top: 7px; left: -50%; width: 100%; height: 3px; background: var(--border); z-index: 0; }
  .lc-step:first-child::before { display: none; }
  .lc-step.done::before, .lc-step.current::before { background: linear-gradient(90deg, var(--primary), var(--secondary)); }
  .lc-step .dot { width: 15px; height: 15px; border-radius: 50%; margin: 0 auto 8px; background: var(--card); border: 3px solid var(--border); position: relative; z-index: 2; box-sizing: border-box; transition: all 200ms ease; }
  .lc-step.done .dot { background: var(--primary); border-color: var(--primary); }
  .lc-step.current .dot { background: var(--accent); border-color: var(--accent); box-shadow: 0 0 0 5px rgba(217,119,6,0.18); }
  .lc-step .name { font-size: 12px; font-weight: 500; color: var(--muted-fg); letter-spacing: 0.01em; }
  .lc-step.done .name { color: var(--fg); }
  .lc-step.current .name { color: var(--accent); font-weight: 600; }
  .lc-step .date { font-size: 11px; color: var(--muted-fg); margin-top: 3px; font-family: var(--font-mono); }
  .goal-hero { background: var(--card); border: 1px solid var(--border); border-radius: var(--radius); padding: 22px 24px; margin-bottom: 20px; box-shadow: var(--shadow); border-left: 4px solid var(--primary); }
  .goal-hero h2 { font-size: 19px; margin: 0 0 8px; font-weight: 700; text-transform: none; letter-spacing: -0.01em; }
  .goal-body { color: var(--muted-fg); margin: 0 0 12px; max-width: 760px; font-size: 14px; line-height: 1.6; }
  .muted { color: var(--muted-fg); font-size: 12px; }
  .mono { font-family: var(--font-mono); }
  .goal-row { display: flex; justify-content: space-between; align-items: center; gap: 12px; padding: 10px 12px; border-bottom: 1px solid var(--border); border-radius: 6px; transition: background var(--transition); }
  .goal-row:last-child { border-bottom: none; }
  .goal-row:hover { background: var(--muted); }
  .goal-row.current { background: rgba(30,64,175,0.05); }
  .goal-row-title { font-weight: 500; font-size: 13.5px; display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .goal-row-meta { color: var(--muted-fg); font-size: 11.5px; white-space: nowrap; }
  @media (prefers-reduced-motion: reduce) {
    * { animation: none !important; transition: none !important; }
  }
</style>
</head>
<body>
<header>
  <h1><span class="mark">Teamx</span> Enterprise</h1>
  <nav>
    <button data-view="kanban" class="active">KanBan</button>
    <button data-view="goal">Goal</button>
    <button data-view="cost">Cost</button>
    <button data-view="timeline">Timeline</button>
    <button data-view="members">Members</button>
  </nav>
</header>
<div class="toolbar">
  <div class="toolbar-left">
    <label>Team <select id="team"></select></label>
    <label>Goal <select id="goal"><option value="">All</option></select></label>
  </div>
  <div class="toolbar-right">
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

  <!-- ============ Timeline view (Gantt) ============ -->
  <div class="view" id="view-timeline">
    <div class="gantt" id="gantt"></div>
  </div>

  <!-- ============ Members view ============ -->
  <div class="view" id="view-members">
    <section class="panel"><h2>Members</h2><table id="membersTable"><tbody></tbody></table></section>
  </div>
</div>
<script>
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
  // Gantt scoped to the selected goal's lifecycle window.
  if (sel) {
    drawGantt(tl.items ?? [], { el: "#goalGantt", from: sel.created_at, to: sel.closed_at || new Date().toISOString() })
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
    return `<div class="kb-col">
      <div class="kb-col-head">
        <div class="kb-col-title">${esc(col.display_name)} ${col.role ? badge(col.role) : ""}</div>
        <div class="kb-col-meta muted">
          ${fmtMs(s.duration_ms)} · ${fmtNum(tasks.length)} tasks${tasks.length > 0 ? ` · ${done} with human` : ""}
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
  const tipRows = []
  const addRow = (k, v) => { if (v != null && v !== "") tipRows.push([k, String(v)]) }
  addRow("member", who)
  addRow("node", r.node_name)
  addRow("started", fmtDate(r.started_at))
  if (r.ended_at) addRow("ended", fmtDate(r.ended_at))
  if (r.duration_ms != null) addRow("duration", fmtMs(r.duration_ms))
  addRow("human", r.has_human ? "yes" : "no")
  // description first, then the rest of detail fields (skip description dup)
  if (desc) addRow("task", desc)
  if (d_) { for (const [k, v] of Object.entries(d_)) { if (v == null || k === "description" || k === "task") continue; const val = typeof v === "object" ? JSON.stringify(v) : String(v); addRow(k, val.length > 300 ? val.slice(0, 300) + "…" : val) } }
  const descHtml = desc ? `<div class="kb-desc">${esc(desc)}</div>` : ""
  const tip = `<div class="kb-tip" role="tooltip">
    <div class="tip-title"><span>${esc(tag)}</span><span class="badge work_session">work_session</span></div>
    ${descHtml}
    <table>${tipRows.map(([k, v]) => `<tr><td class="k">${esc(k)}</td><td class="v">${esc(v)}</td></tr>`).join("")}</table>
  </div>`
  return `<div class="kb-card" tabindex="0">
    <div class="kb-head"><span class="kb-who">${esc(who)}</span><span class="badge ${r.has_human ? "active" : "idle"}">${esc(tag)}</span></div>
    <div class="kb-body">${desc ? esc(desc) : `Worked <b>${hrs}h</b>`}${!desc && d_.sessionID ? " · <span class='mono'>" + esc(String(d_.sessionID)) + "</span>" : ""}</div>
    <div class="kb-meta muted">${fmtDate(r.started_at)}${r.ended_at ? " → " + fmtDate(r.ended_at) : ""}</div>
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

// ---- Timeline (Gantt) view ----
async function renderTimeline() {
  const q = qs()
  const [teams, tl] = await Promise.all([jget("/api/teams"), jget("/api/timeline?" + q)])
  fillTeamSelect(teams)
  drawGantt(tl.items ?? [])
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
  // Adaptive scale: pick px per hour so the whole span stays readable.
  const spanH = span / 3600000
  let scale
  if (spanH <= 12) scale = 30          // a few hours: roomy
  else if (spanH <= 72) scale = 10     // ~3 days
  else if (spanH <= 24 * 14) scale = 4 // ~2 weeks
  else scale = 1.5                     // longer: compact but scrollable
  const W = Math.max(760, labelW + spanH * scale)   // total width (px)
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
  svg.push(`<svg width="${W}" height="${H}" xmlns="http://www.w3.org/2000/svg" role="img" class="gantt-svg">`)
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
      <stop offset="0%" stop-color="#1E40AF"/><stop offset="100%" stop-color="#3B82F6"/>
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
      const bx = x(s), bw = Math.max(x(e) - bx, 2)
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
    // human events as small green dots (approval/input/command) at their time
    items2.filter((it) => it.kind.startsWith("human")).forEach((it) => {
      const s = Date.parse(it.started_at)
      if (!Number.isFinite(s)) return
      const hx = x(s)
      let d = null; try { d = typeof it.detail === "string" ? JSON.parse(it.detail) : it.detail } catch { d = null }
      const label = d ? (d.text ?? d.name ?? d.response ?? "") : ""
      svg.push(`<circle cx="${hx}" cy="${midY}" r="4" fill="#059669" stroke="#fff" stroke-width="1.2">
        <title>${esc(it.kind)}${label ? " · " + esc(String(label).slice(0, 60)) : ""}</title>
      </circle>`)
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
    <span><span class="sw" style="background:#059669;border-radius:50%"></span>human action</span>
    <span><span class="sw" style="background:var(--accent);border-radius:2px;width:14px"></span>today</span>
  </div>`
  el.innerHTML = `<div style="min-width:560px">${legend}${svg.join("")}</div>`
}

// ---- Members view ----
async function renderMembers() {
  const q = qs()
  const data = await jget("/api/members?" + q)
  const members = data.members ?? []
  $("#membersTable tbody").innerHTML = members.map((m) => {
    const a = m.activity ?? {}
    return `<tr>
      <td>${esc(m.display_name)}</td>
      <td>${m.role ? badge(m.role) : "—"}</td>
      <td>${badge(m.state)}</td>
      <td>${fmtDate(m.joined_at)}</td>
      <td>${fmtDate(m.last_seen_at)}</td>
      <td>${fmtMs(a.duration_ms)}</td>
      <td>${fmtNum(a.tokens_input)}</td>
      <td>${fmtCost(a.cost)}</td>
      <td>${fmtNum(a.count)}</td>
    </tr>`
  }).join("")
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
async function boot() {
  try {
    const teams = await jget("/api/teams")
    fillTeamSelect(teams)
    await loadMemberNames()
    await refreshGoalSelect()
    renderKanban()
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
    // No token → 401 with the URL hint.
    (
        StatusCode::UNAUTHORIZED,
        "unauthorized: open the URL printed by `teamx ui` (with ?token=...)",
    )
        .into_response()
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
}

/// Guard the API routes: requires the cookie, else 401.
#[allow(clippy::result_large_err)]
fn guard(state: &S, headers: &axum::http::HeaderMap) -> Result<(), axum::response::Response> {
    if authorized(state, headers.get(header::COOKIE).and_then(|v| v.to_str().ok())) {
        Ok(())
    } else {
        Err((StatusCode::UNAUTHORIZED, "unauthorized").into_response())
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
    let mut out = Vec::new();
    for m in &rows {
        let mid = m["id"].as_str().unwrap_or("").to_string();
        let a = agg
            .get(&mid)
            .cloned()
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        let mut row = m.clone();
        if let Some(o) = row.as_object_mut() {
            o.insert("activity".to_string(), a);
        }
        out.push(row);
    }
    Json(json!({ "members": out })).into_response()
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
