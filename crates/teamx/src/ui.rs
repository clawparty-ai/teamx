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
<title>Teamx Enterprise</title>
<style>
  :root { color-scheme: light dark; }
  * { box-sizing: border-box; }
  body { font: 14px/1.5 system-ui, sans-serif; margin: 0; background: #f6f8fa; color: #1f2328; }
  .dark body { background: #0d1117; color: #c9d1d9; }
  header { background: #24292f; color: #fff; display: flex; align-items: center; gap: 20px; padding: 0 20px; height: 52px; position: sticky; top: 0; z-index: 10; }
  .dark header { background: #161b22; }
  header h1 { font-size: 16px; margin: 0; white-space: nowrap; }
  nav { display: flex; gap: 4px; }
  nav button { background: none; border: none; color: #c9d1d9; font-size: 14px; padding: 8px 12px; border-radius: 6px; cursor: pointer; }
  nav button:hover { background: rgba(255,255,255,0.08); color: #fff; }
  nav button.active { background: #0969da; color: #fff; }
  .dark nav button.active { background: #1f6feb; }
  .toolbar { display: flex; gap: 10px; flex-wrap: wrap; padding: 14px 20px; align-items: center; background: #fff; border-bottom: 1px solid #d0d7de; }
  .dark .toolbar { background: #161b22; border-bottom-color: #30363d; }
  .container { max-width: 1280px; margin: 0 auto; padding: 20px; }
  .view { display: none; }
  .view.active { display: block; }
  .cards { display: grid; grid-template-columns: repeat(auto-fit, minmax(160px, 1fr)); gap: 12px; margin-bottom: 16px; }
  .card { background: #fff; border: 1px solid #d0d7de; border-radius: 8px; padding: 14px 16px; }
  .dark .card { background: #161b22; border-color: #30363d; }
  .card .label { font-size: 12px; color: #57606a; }
  .dark .card .label { color: #8b949e; }
  .card .value { font-size: 22px; font-weight: 600; margin-top: 4px; }
  .grid2 { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; margin-top: 16px; }
  @media (max-width: 900px) { .grid2 { grid-template-columns: 1fr; } }
  section.panel { background: #fff; border: 1px solid #d0d7de; border-radius: 8px; padding: 16px; margin-bottom: 16px; }
  .dark section.panel { background: #161b22; border-color: #30363d; }
  h2 { font-size: 14px; margin: 0 0 12px; }
  table { width: 100%; border-collapse: collapse; font-size: 13px; }
  th, td { text-align: left; padding: 6px 8px; border-bottom: 1px solid #eaeef2; }
  .dark th, .dark td { border-bottom-color: #21262d; }
  th { color: #57606a; font-weight: 600; }
  .dark th { color: #8b949e; }
  .badge { display: inline-block; padding: 2px 8px; border-radius: 10px; font-size: 12px; font-weight: 500; }
  .badge.proposed, .badge.blocked { background: #fef2c0; color: #9a6700; }
  .dark .badge.proposed, .dark .badge.blocked { background: #3d2f00; color: #d29922; }
  .badge.shared, .badge.in_progress { background: #ddf4ff; color: #0969da; }
  .dark .badge.shared, .dark .badge.in_progress { background: #0a2942; color: #58a6ff; }
  .badge.achieved, .badge.completed, .badge.closed { background: #dafbe1; color: #1a7f37; }
  .dark .badge.achieved, .dark .badge.completed, .dark .badge.closed { background: #122d1d; color: #3fb950; }
  .badge.refining { background: #fbefff; color: #8250df; }
  .badge.owner { background: #ddf4ff; color: #0969da; }
  .badge.contributor { background: #dafbe1; color: #1a7f37; }
  .badge.reviewer { background: #fbefff; color: #8250df; }
  .badge.observer { background: #fff1e5; color: #bf8700; }
  .badge.active { background: #dafbe1; color: #1a7f37; }
  .badge.idle, .badge.pending { background: #f6f8fa; color: #57606a; }
  .badge.waiting { background: #fef2c0; color: #9a6700; }
  .badge.left { background: #ffebe9; color: #cf222e; }
  .detail { font-family: ui-monospace, monospace; font-size: 11px; color: #57606a; white-space: pre-wrap; word-break: break-all; max-width: 480px; }
  .dark .detail { color: #8b949e; }
  .kanban { display: grid; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); gap: 12px; }
  .kb-col { background: #f6f8fa; border: 1px solid #d0d7de; border-radius: 8px; padding: 10px; min-height: 120px; }
  .dark .kb-col { background: #161b22; border-color: #30363d; }
  .kb-col h3 { margin: 0 0 8px; font-size: 13px; display: flex; justify-content: space-between; align-items: center; }
  .kb-card { background: #fff; border: 1px solid #d0d7de; border-radius: 6px; padding: 8px 10px; margin-bottom: 6px; font-size: 12px; }
  .dark .kb-card { background: #0d1117; border-color: #30363d; }
  .gantt { overflow-x: auto; }
  .gantt-row { display: flex; align-items: center; height: 34px; border-bottom: 1px solid #eaeef2; }
  .dark .gantt-row { border-bottom-color: #21262d; }
  .gantt-label { width: 140px; flex-shrink: 0; font-size: 12px; padding: 0 8px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .gantt-track { position: relative; flex: 1; height: 100%; min-width: 400px; }
  .gantt-bar { position: absolute; height: 16px; top: 9px; border-radius: 4px; background: #0969da; opacity: 0.85; min-width: 2px; }
  .gantt-bar.work { background: #0969da; }
  .gantt-bar.tool { background: #8250df; }
  .gantt-bar.step { background: #bf8700; }
  .gantt-bar.human { background: #1a7f37; }
  .gantt-point { position: absolute; top: 10px; width: 10px; height: 10px; border-radius: 50%; margin-left: -5px; }
  .gantt-point.tool { background: #8250df; }
  .gantt-point.step { background: #bf8700; }
  .gantt-point.human { background: #1a7f37; }
  .gantt-axis { display: flex; margin-left: 140px; font-size: 11px; color: #57606a; }
  .gantt-axis span { flex: 1; border-left: 1px solid #eaeef2; padding-left: 4px; }
  .dark .gantt-axis span { border-left-color: #21262d; }
  .lifecycle { position: relative; padding: 8px 0 0 0; }
  .lc-track { display: flex; align-items: flex-start; gap: 0; }
  .lc-step { flex: 1; text-align: center; position: relative; min-width: 90px; }
  .lc-step .dot { width: 12px; height: 12px; border-radius: 50%; margin: 0 auto 6px; background: #d0d7de; position: relative; z-index: 1; }
  .dark .lc-step .dot { background: #30363d; }
  .lc-step.done .dot { background: #0969da; }
  .lc-step.current .dot { background: #1f6feb; box-shadow: 0 0 0 4px rgba(9,105,218,0.25); }
  .lc-step.current .dot::after { content: ""; position: absolute; inset: -6px; border-radius: 50%; border: 2px solid #0969da; }
  .lc-step .name { font-size: 12px; color: #57606a; }
  .dark .lc-step .name { color: #8b949e; }
  .lc-step .date { font-size: 11px; color: #8b949e; margin-top: 2px; }
  .lc-line { height: 2px; background: #d0d7de; position: relative; top: -7px; margin: 0 6%; }
  .dark .lc-line { background: #30363d; }
  .lc-line.fill { background: #0969da; }
  .goal-hero { background: #fff; border: 1px solid #d0d7de; border-radius: 8px; padding: 18px 20px; margin-bottom: 16px; }
  .dark .goal-hero { background: #161b22; border-color: #30363d; }
  .goal-hero h2 { font-size: 18px; margin: 0 0 6px; }
  .goal-body { color: #57606a; margin: 0 0 10px; max-width: 720px; }
  .dark .goal-body { color: #8b949e; }
  .filters label { display: flex; flex-direction: column; font-size: 11px; color: #57606a; gap: 3px; }
  .dark .filters label { color: #8b949e; }
  select, input, button { padding: 5px 8px; border: 1px solid #d0d7de; border-radius: 5px; background: #fff; color: #1f2328; }
  .dark select, .dark input, .dark button { background: #0d1117; border-color: #30363d; color: #c9d1d9; }
  button { cursor: pointer; }
  button:hover { border-color: #0969da; }
  .muted { color: #8b949e; font-size: 12px; }
</style>
</head>
<body>
<header>
  <h1>Teamx Enterprise</h1>
  <nav>
    <button data-view="goal" class="active">Goal</button>
    <button data-view="kanban">KanBan</button>
    <button data-view="cost">Cost</button>
    <button data-view="timeline">Timeline</button>
    <button data-view="members">Members</button>
  </nav>
</header>
<div class="toolbar">
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

<div class="container">
  <!-- ============ Goal view (default) ============ -->
  <div class="view active" id="view-goal">
    <div id="goalHero"></div>
    <div id="goalLifecycle"></div>
    <div class="cards" id="goalCards"></div>
    <div class="grid2">
      <section class="panel"><h2>By member</h2><table id="gByMember"><tbody></tbody></table></section>
      <section class="panel"><h2>By node</h2><table id="gByNode"><tbody></tbody></table></section>
    </div>
    <section class="panel"><h2>Goal timeline (recent events)</h2><table id="goalEvents"><tbody></tbody></table></section>
  </div>

  <!-- ============ KanBan view ============ -->
  <div class="view" id="view-kanban">
    <div class="kanban" id="kanbanCols"></div>
  </div>

  <!-- ============ Cost view ============ -->
  <div class="view" id="view-cost">
    <div class="cards" id="costCards"></div>
    <div class="grid2">
      <section class="panel"><h2>Cost by member</h2><table id="costByMember"><tbody></tbody></table></section>
      <section class="panel"><h2>Cost by node</h2><table id="costByNode"><tbody></tbody></table></section>
    </div>
    <div class="grid2">
      <section class="panel"><h2>Tool usage</h2><table id="costTools"><tbody></tbody></table></section>
      <section class="panel"><h2>Files edited</h2><table id="costFiles"><tbody></tbody></table></section>
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
function qs() {
  const p = new URLSearchParams({ team: $("#team").value })
  const from = $("#from").value, to = $("#to").value, kind = $("#kind").value
  if (from) p.set("from", new Date(from).toISOString())
  if (to) p.set("to", new Date(to).toISOString())
  if (kind) p.set("kind", kind)
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
async function renderGoal() {
  const q = qs()
  const [teams, goal, overview, byMember, byNode] = await Promise.all([
    jget("/api/teams"), jget("/api/goal?" + q), jget("/api/overview?" + q),
    jget("/api/by_member?" + q), jget("/api/by_node?" + q),
  ])
  fillTeamSelect(teams)
  const g = goal.goal
  const hero = $("#goalHero")
  if (g) {
    hero.innerHTML = `<div class="goal-hero">
      <h2>${esc(g.title)} ${g.state ? badge(g.state) : ""}</h2>
      ${g.body ? `<p class="goal-body">${esc(g.body)}</p>` : ""}
      <div class="muted">created ${fmtDate(g.created_at)} · updated ${fmtDate(g.updated_at)}</div>
    </div>`
  } else {
    hero.innerHTML = `<div class="goal-hero"><h2>No goal set</h2><p class="goal-body muted">Use <code>teamx goal set</code> to define the team goal.</p></div>`
  }
  renderLifecycle(goal.lifecycle, g?.state)
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
    (m) => esc(m.member_id?.slice(0, 8)), (m) => fmtMs(m.duration_ms),
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
    (e) => esc(e.member_id?.slice(0, 8)), (e) => esc(row(e.payload)),
  ])
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
  const html = `<div class="lifecycle"><div class="lc-track">
    ${steps.map((s, i) => {
      const cls = s.state === current ? "current" : (i < steps.length - 1 || s.state === current ? "done" : "")
      return `<div class="lc-step ${cls}"><div class="dot"></div><div class="name">${esc(s.state)}</div><div class="date">${s.at ? fmtDate(s.at).split(", ")[0] : "now"}</div></div>`
    }).join("")}
  </div><div class="lc-line ${steps.some(s => s.state === current) ? "fill" : ""}"></div></div>`
  el.innerHTML = html
}

// ---- KanBan view ----
async function renderKanban() {
  const q = qs()
  const kanban = await jget("/api/kanban?" + q)
  $("#kanbanCols").innerHTML = (kanban.columns ?? []).map((col) => `
    <div class="kb-col">
      <h3><span>${esc(col.label)}</span><span class="badge ${esc(col.key)}">${fmtNum(col.count)}</span></h3>
      ${(col.rows ?? []).slice(0, 8).map((r) => `
        <div class="kb-card">
          <div class="muted">${fmtDate(r.started_at)} · ${esc(r.member_id?.slice(0, 8))}</div>
          <div class="detail">${esc(row(r.detail))}</div>
        </div>`).join("")}
    </div>`).join("")
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
    (m) => esc(m.member_id?.slice(0, 8)), (m) => fmtNum(m.tokens_input),
    (m) => fmtNum(m.tokens_output), (m) => fmtCost(m.cost), (m) => fmtNum(m.count),
  ])
  $("#costByNode tbody").innerHTML = tableRows(byNode, [
    (n) => esc(n.node_name ?? n.node_id?.slice(0, 8)), (n) => fmtNum(n.tokens_input),
    (n) => fmtNum(n.tokens_output), (n) => fmtCost(n.cost), (n) => fmtNum(n.count),
  ])
  $("#costTools tbody").innerHTML = tableRows(tools, [(t) => esc(t.tool), (t) => fmtNum(t.count)])
  $("#costFiles tbody").innerHTML = tableRows(files, [(f) => esc(f.file), (f) => fmtNum(f.count)])
}

// ---- Timeline (Gantt) view ----
async function renderTimeline() {
  const q = qs()
  const [teams, tl] = await Promise.all([jget("/api/teams"), jget("/api/timeline?" + q)])
  fillTeamSelect(teams)
  drawGantt(tl.items ?? [])
}

function drawGantt(items) {
  const el = $("#gantt")
  if (!items || items.length === 0) { el.innerHTML = '<div class="muted">no activity in this range</div>'; return }
  let min = Infinity, max = -Infinity
  for (const it of items) {
    const s = Date.parse(it.started_at); if (Number.isFinite(s)) min = Math.min(min, s)
    let e = s
    if (it.ended_at) { const ee = Date.parse(it.ended_at); if (Number.isFinite(ee)) e = ee }
    else if (it.duration_ms) e = s + it.duration_ms
    if (Number.isFinite(e)) max = Math.max(max, e)
  }
  if (!Number.isFinite(min) || !Number.isFinite(max)) { el.innerHTML = '<div class="muted">no timestamps</div>'; return }
  const pad = (max - min) * 0.05
  min -= pad; max += pad
  const span = max - min
  const byMember = new Map()
  for (const it of items) {
    if (!byMember.has(it.member_id)) byMember.set(it.member_id, [])
    byMember.get(it.member_id).push(it)
  }
  const memberNames = new Map()
  for (const mid of byMember.keys()) {
    memberNames.set(mid, byMember.get(mid)[0].node_name ?? mid.slice(0, 8))
  }
  const divisions = 6
  const axis = `<div class="gantt-axis">${Array.from({ length: divisions + 1 }, (_, i) => {
    const t = min + (span * i) / divisions
    return `<span>${new Date(t).toLocaleDateString()}</span>`
  }).join("")}</div>`
  const rows = Array.from(byMember.entries()).map(([mid, items2]) => {
    const bars = items2.map((it) => {
      const s = Date.parse(it.started_at)
      if (!Number.isFinite(s)) return ""
      let e = s
      if (it.ended_at) { const ee = Date.parse(it.ended_at); if (Number.isFinite(ee)) e = ee }
      else if (it.duration_ms) e = s + it.duration_ms
      const leftPct = ((s - min) / span) * 100
      const widthPct = Math.max(((e - s) / span) * 100, 0.15)
      const isBar = it.kind === "work_session" || it.kind === "command"
      const kindCls = it.kind.startsWith("human") ? "human" : (it.kind === "step_finish" ? "step" : (it.kind === "tool_call" ? "tool" : "work"))
      const style = isBar ? `left:${leftPct}%;width:${widthPct}%;` : `left:${leftPct}%;`
      return `<div class="${isBar ? "gantt-bar" : "gantt-point"} ${kindCls}" style="${style}" title="${esc(it.kind)} @ ${it.started_at}${it.duration_ms ? " · " + fmtMs(it.duration_ms) : ""}"></div>`
    }).join("")
    return `<div class="gantt-row"><div class="gantt-label" title="${esc(mid)}">${esc(memberNames.get(mid))}</div><div class="gantt-track">${bars}</div></div>`
  }).join("")
  el.innerHTML = `<div style="min-width:560px">${axis}${rows}</div>`
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
function fillTeamSelect(teams) {
  const sel = $("#team")
  if (sel.options.length === 0 && teams?.teams) {
    for (const t of teams.teams) { const o = document.createElement("option"); o.value = t.id; o.textContent = t.name; sel.appendChild(o) }
    sel.addEventListener("change", refresh)
  }
}
function refresh() {
  const active = $$("nav button").find((b) => b.classList.contains("active"))?.dataset.view ?? "goal"
  if (active === "timeline") renderTimeline()
  else if (active === "cost") renderCost()
  else if (active === "kanban") renderKanban()
  else if (active === "members") renderMembers()
  else renderGoal()
}
$("#from").addEventListener("change", refresh)
$("#to").addEventListener("change", refresh)
$("#kind").addEventListener("change", refresh)
async function boot() {
  try {
    const teams = await jget("/api/teams")
    fillTeamSelect(teams)
    renderGoal()
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

// ---------------------------------------------------------------------------
// Enterprise dashboard v2 API: goal-centric views.
// ---------------------------------------------------------------------------

/// Goal-centric overview: the team's current goal + its lifecycle phases
/// (derived from the events ledger: goal.set / goal.shared / goal.state_changed
/// / goal.achieved / team.completed) with per-phase activity aggregates.
async fn api_goal(State(state): State<S>, Query(params): Query<HashMap0>, headers: axum::http::HeaderMap) -> axum::response::Response {
    if let Err(r) = guard(&state, &headers) {
        return r;
    }
    let team = match params.team.as_deref() {
        Some(t) => t,
        None => return (StatusCode::BAD_REQUEST, "missing team").into_response(),
    };
    let c = lock(&state);

    // Current goal row.
    let goal = match c.query_row(
        "SELECT g.id, g.title, g.body, g.state, g.created_at, g.updated_at
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
            }))
        },
    ) {
        Ok(g) => Some(g),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response();
        }
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

    Json(json!({ "team_id": team, "goal": goal, "lifecycle": lifecycle })).into_response()
}

/// Members view: every member with role/state + per-member activity aggregates
/// (duration/tokens/cost) across the team.
async fn api_members(State(state): State<S>, Query(params): Query<HashMap0>, headers: axum::http::HeaderMap) -> axum::response::Response {
    if let Err(r) = guard(&state, &headers) {
        return r;
    }
    let team = match params.team.as_deref() {
        Some(t) => t,
        None => return (StatusCode::BAD_REQUEST, "missing team").into_response(),
    };
    let c = lock(&state);
    let mut stmt = match c.prepare(
        "SELECT m.id, m.display_name, m.role, m.state, m.joined_at, m.last_seen_at
         FROM members m WHERE m.team_id = ?1 AND m.state NOT IN ('left','denied')
         ORDER BY m.joined_at ASC",
    ) {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    };
    let rows = match stmt.query_map([team], |r| {
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

/// KanBan view: activity columns grouped by kind family
/// (work / human / tooling), each with counts + recent rows.
async fn api_kanban(State(state): State<S>, Query(params): Query<HashMap0>, headers: axum::http::HeaderMap) -> axum::response::Response {
    if let Err(r) = guard(&state, &headers) {
        return r;
    }
    let team = match params.team.as_deref() {
        Some(t) => t,
        None => return (StatusCode::BAD_REQUEST, "missing team").into_response(),
    };
    let (from, to, _kind, member) = extract_filters(&params);
    let limit = params.limit.unwrap_or(50);
    let c = lock(&state);

    // Column: work (work_session), human (human_*), tooling (tool_call/
    // step_finish/command/file_edit). Each column = kind filter + counts + recent.
    let columns = [
        ("work", "work_session", "Work sessions"),
        ("human", "human_input", "Human activity"),
        ("tooling", "tool_call", "Tool calls"),
        ("steps", "step_finish", "Steps"),
    ];
    let mut out = Vec::new();
    for (key, kind, label) in columns {
        let rows = crate::activity::rows(&c, team, member, None, Some(kind), from, to, limit)
            .unwrap_or(serde_json::json!([]));
        let count = rows.as_array().map(|a| a.len() as i64).unwrap_or(0);
        out.push(json!({ "key": key, "label": label, "kind": kind, "count": count, "rows": rows }));
    }
    Json(json!({ "columns": out })).into_response()
}

/// Timeline (Gantt) view data: per-member work segments + event points.
async fn api_timeline(State(state): State<S>, Query(params): Query<HashMap0>, headers: axum::http::HeaderMap) -> axum::response::Response {
    if let Err(r) = guard(&state, &headers) {
        return r;
    }
    let team = match params.team.as_deref() {
        Some(t) => t,
        None => return (StatusCode::BAD_REQUEST, "missing team").into_response(),
    };
    let (from, to, _kind, member) = extract_filters(&params);
    let c = lock(&state);
    match crate::activity::timeline(&c, team, member, from, to) {
        Ok(v) => Json(json!({ "items": v })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    }
}
