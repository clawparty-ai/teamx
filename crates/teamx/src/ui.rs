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
<title>teamx · team activity</title>
<style>
  :root { color-scheme: light dark; }
  body { font: 14px/1.5 system-ui, sans-serif; margin: 0; background: #f6f8fa; }
  .dark body { background: #0d1117; }
  header { padding: 12px 20px; background: #24292f; color: #fff; display: flex; align-items: center; gap: 12px; }
  header h1 { font-size: 16px; margin: 0; }
  .container { max-width: 1100px; margin: 20px auto; padding: 0 20px; }
  .cards { display: grid; grid-template-columns: repeat(auto-fit, minmax(160px, 1fr)); gap: 12px; }
  .card { background: #fff; border: 1px solid #d0d7de; border-radius: 8px; padding: 14px 16px; }
  .dark .card { background: #161b22; border-color: #30363d; }
  .card .label { font-size: 12px; color: #57606a; }
  .dark .card .label { color: #8b949e; }
  .card .value { font-size: 22px; font-weight: 600; margin-top: 4px; }
  .grid2 { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; margin-top: 16px; }
  @media (max-width: 800px) { .grid2 { grid-template-columns: 1fr; } }
  section.panel { background: #fff; border: 1px solid #d0d7de; border-radius: 8px; padding: 16px; margin-top: 16px; }
  .dark section.panel { background: #161b22; border-color: #30363d; }
  h2 { font-size: 14px; margin: 0 0 12px; }
  table { width: 100%; border-collapse: collapse; font-size: 13px; }
  th, td { text-align: left; padding: 6px 8px; border-bottom: 1px solid #eaeef2; }
  .dark th, .dark td { border-bottom-color: #21262d; }
  th { color: #57606a; font-weight: 600; }
  .dark th { color: #8b949e; }
  .bar { height: 12px; background: #0969da; border-radius: 3px; display: inline-block; }
  .dark .bar { background: #58a6ff; }
  .filters { display: flex; gap: 10px; flex-wrap: wrap; margin-bottom: 8px; align-items: center; }
  select, input { padding: 4px 6px; border: 1px solid #d0d7de; border-radius: 5px; background: #fff; }
  .dark select, .dark input { background: #161b22; border-color: #30363d; color: #c9d1d9; }
  .note { color: #6e7781; font-size: 12px; }
  .detail { font-family: ui-monospace, monospace; font-size: 11px; color: #57606a; white-space: pre-wrap; word-break: break-all; max-width: 480px; }
  .dark .detail { color: #8b949e; }
</style>
</head>
<body>
<header><h1>teamx · team activity dashboard</h1><span id="teamLabel" class="note"></span></header>
<div class="container">
  <div class="filters">
    <label>Team <select id="team"></select></label>
    <label>From <input type="datetime-local" id="from"></label>
    <label>To <input type="datetime-local" id="to"></label>
    <label>Kind <select id="kind"><option value="">all</option>
      <option>tool_call</option><option>step_finish</option><option>command</option>
      <option>file_edit</option><option>work_session</option><option>human_input</option>
      <option>human_approval</option><option>human_command</option>
    </select></label>
    <button onclick="refresh()">Refresh</button>
  </div>

  <div class="cards" id="cards"></div>

  <div class="grid2">
    <section class="panel"><h2>By member</h2><table id="byMember"><tbody></tbody></table></section>
    <section class="panel"><h2>By node</h2><table id="byNode"><tbody></tbody></table></section>
  </div>

  <div class="grid2">
    <section class="panel"><h2>Tool usage</h2><table id="tools"><tbody></tbody></table></section>
    <section class="panel"><h2>Files edited</h2><table id="files"><tbody></tbody></table></section>
  </div>

  <section class="panel">
    <h2>Human activity (what the humans did)</h2>
    <table id="human"><tbody></tbody></table>
  </section>

  <section class="panel">
    <h2>Recent activity</h2>
    <table id="rows"><tbody></tbody></table>
  </section>
</div>
<script>
const $ = (s) => document.querySelector(s)
function fmtMs(ms) {
  if (ms == null) return "—"
  const s = Math.round(ms / 1000)
  if (s < 60) return s + "s"
  const m = Math.floor(s / 60), r = s % 60
  if (m < 60) return m + "m" + r + "s"
  const h = Math.floor(m / 60), rm = m % 60
  return h + "h" + rm + "m"
}
function fmtNum(v) { return v == null ? "—" : Number(v).toLocaleString() }
function fmtCost(v) { return v == null ? "—" : "$" + Number(v).toFixed(4) }
function esc(s) { const d = document.createElement("div"); d.textContent = s ?? ""; return d.innerHTML }
function qs() {
  const team = $("#team").value
  const from = $("#from").value
  const to = $("#to").value
  const kind = $("#kind").value
  const p = new URLSearchParams({ team })
  if (from) p.set("from", new Date(from).toISOString())
  if (to) p.set("to", new Date(to).toISOString())
  if (kind) p.set("kind", kind)
  return p.toString()
}
async function jget(path) {
  const r = await fetch(path, { headers: { Accept: "application/json" } })
  if (!r.ok) throw new Error("HTTP " + r.status)
  return r.json()
}
function row(o) { return o != null && typeof o === "object" ? JSON.stringify(o) : "—" }
async function refresh() {
  try {
    const q = qs()
    const [teams, overview, byMember, byNode, tools, files, human, rows] = await Promise.all([
      jget("/api/teams"), jget("/api/overview?" + q), jget("/api/by_member?" + q),
      jget("/api/by_node?" + q), jget("/api/tools?" + q), jget("/api/files?" + q),
      jget("/api/human?" + q), jget("/api/rows?" + q),
    ])
    const teamSel = $("#team")
    if (teamSel.options.length === 0) {
      for (const t of teams.teams ?? []) {
        const o = document.createElement("option"); o.value = t.id; o.textContent = t.name; teamSel.appendChild(o)
      }
    }
    // cards
    const o = overview.overview ?? {}
    const cards = [
      ["Total work time", fmtMs(o.duration_ms)],
      ["Total tokens", fmtNum(o.tokens_input + o.tokens_output)],
      ["Cost", fmtCost(o.cost)],
      ["Active members", fmtNum(o.active_members)],
      ["Active nodes", fmtNum(o.active_nodes)],
      ["Human actions", fmtNum(overview.human?.count)],
      ["AI actions", fmtNum(overview.ai?.count)],
      ["Rows", fmtNum(overview.overall?.count)],
    ]
    $("#cards").innerHTML = cards.map(([l, v]) => `<div class="card"><div class="label">${l}</div><div class="value">${v}</div></div>`).join("")
    // by member
    $("#byMember tbody").innerHTML = (byMember ?? []).map(m => `<tr>
      <td>${esc(m.member_id?.slice(0, 8))}</td>
      <td>${fmtMs(m.duration_ms)}</td>
      <td>${fmtNum(m.tokens_input)}/${fmtNum(m.tokens_output)}</td>
      <td>${fmtCost(m.cost)}</td>
      <td>${fmtNum(m.count)}</td></tr>`).join("")
    // by node
    $("#byNode tbody").innerHTML = (byNode ?? []).map(n => `<tr>
      <td>${esc(n.node_name ?? n.node_id?.slice(0, 8))}</td>
      <td>${fmtMs(n.duration_ms)}</td>
      <td>${fmtNum(n.tokens_input)}</td>
      <td>${fmtCost(n.cost)}</td>
      <td>${fmtNum(n.count)}</td></tr>`).join("")
    // tools
    $("#tools tbody").innerHTML = (tools ?? []).map(t => `<tr>
      <td>${esc(t.tool)}</td><td>${fmtNum(t.count)}</td></tr>`).join("")
    // files
    $("#files tbody").innerHTML = (files ?? []).map(f => `<tr>
      <td>${esc(f.file)}</td><td>${fmtNum(f.count)}</td></tr>`).join("")
    // human
    $("#human tbody").innerHTML = (human ?? []).map(h => `<tr>
      <td>${esc(h.started_at)}</td>
      <td>${esc(h.kind)}</td>
      <td>${esc(h.member_id?.slice(0, 8))}</td>
      <td class="detail">${esc(row(h.detail))}</td></tr>`).join("")
    // rows
    $("#rows tbody").innerHTML = (rows ?? []).map(r => `<tr>
      <td>${esc(r.started_at)}</td>
      <td>${esc(r.kind)}</td>
      <td>${esc(r.member_id?.slice(0, 8))}</td>
      <td>${esc(r.node_name ?? r.node_id?.slice(0, 8))}</td>
      <td>${fmtMs(r.duration_ms)}</td>
      <td>${fmtNum(r.tokens_input)}</td>
      <td>${fmtCost(r.cost)}</td>
      <td class="detail">${esc(row(r.detail))}</td></tr>`).join("")
  } catch (e) {
    $("#cards").innerHTML = `<div class="card"><div class="label">error</div><div class="value">${esc(String(e))}</div></div>`
  }
}
// Load team list then refresh.
async function boot() {
  const teams = await jget("/api/teams")
  const sel = $("#team")
  for (const t of teams.teams ?? []) { const o = document.createElement("option"); o.value = t.id; o.textContent = t.name; sel.appendChild(o) }
  $("#teamLabel").textContent = "owner-only dashboard"
  refresh()
}
boot()
setInterval(refresh, 30000)
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
    let team = match params.team.as_deref() {
        Some(t) => t,
        None => return (StatusCode::BAD_REQUEST, "missing team").into_response(),
    };
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
    let team = match params.team.as_deref() {
        Some(t) => t,
        None => return (StatusCode::BAD_REQUEST, "missing team").into_response(),
    };
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
    let team = match params.team.as_deref() {
        Some(t) => t,
        None => return (StatusCode::BAD_REQUEST, "missing team").into_response(),
    };
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
    let team = match params.team.as_deref() {
        Some(t) => t,
        None => return (StatusCode::BAD_REQUEST, "missing team").into_response(),
    };
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
    let team = match params.team.as_deref() {
        Some(t) => t,
        None => return (StatusCode::BAD_REQUEST, "missing team").into_response(),
    };
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
    let team = match params.team.as_deref() {
        Some(t) => t,
        None => return (StatusCode::BAD_REQUEST, "missing team").into_response(),
    };
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
    let team = match params.team.as_deref() {
        Some(t) => t,
        None => return (StatusCode::BAD_REQUEST, "missing team").into_response(),
    };
    let (from, to, _kind, member) = extract_filters(&params);
    let limit = params.limit.unwrap_or(100);
    let c = lock(&state);
    match crate::activity::human_rows(&c, team, member, None, from, to, limit) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    }
}
