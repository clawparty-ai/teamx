//! console_html.rs — embedded HTML pages for `teamx console`.
//!
//! Look & feel mirrors the owner dashboard (`teamx ui`) but the brand is
//! "Teamx Console". The single-page console has five read-only views
//! (Teams / My Tasks / Events / Tunnels / Cloud); every panel is rendered by
//! calling the matching `teamx` CLI/RPC data endpoint on click (no polling).

/// Main console page (single-page, vanilla JS, no external deps).
pub fn page_html() -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Teamx Console</title>
<style>
{css}
</style>
</head>
<body>
<header>
  <div class="logo">
    <div class="logo-icon">Tx</div>
    <span class="logo-text">Teamx Console</span>
  </div>
  <nav>
    <button data-view="teams" class="active">Teams</button>
    <button data-view="tasks">My Tasks</button>
    <button data-view="events">Events</button>
    <button data-view="tunnels">Tunnels</button>
    <button data-view="cloud">Cloud</button>
  </nav>
  <div class="header-right">
    <button class="theme-toggle" id="themeToggle" title="Toggle light/dark mode">☀️</button>
    <div class="user-info"><div class="user-avatar">M</div><span>Member</span></div>
  </div>
</header>
<div class="toolbar">
  <div class="ws">workspace <b id="wsPath"></b></div>
  <div class="toolbar-right">
    <span class="muted" id="lastRun"></span>
    <button class="btn" id="refreshBtn">⟳ Refresh</button>
  </div>
</div>
<div class="container">
  <div class="view active" id="view-teams"><div class="cards" id="teamCards"></div><div id="teamBody"></div></div>
  <div class="view" id="view-tasks">
    <section class="panel"><h2>Tasks assigned to me <span class="count" id="taskCount"></span></h2>
      <table><thead><tr><th>Title</th><th>State</th><th>Priority</th><th>Updated</th></tr></thead><tbody id="taskRows"></tbody></table>
    </section>
  </div>
  <div class="view" id="view-events">
    <section class="panel"><h2>Recent team events <span class="count" id="evCount"></span></h2>
      <ul class="ev-list" id="evList"></ul></section>
  </div>
  <div class="view" id="view-tunnels">
    <section class="panel"><h2>Tunnels I can reach <span class="count" id="tunCount"></span></h2>
      <div class="muted" style="margin-bottom:10px">每个隧道先在终端执行「复制」的 forward 命令映射到本地，再去 Cloud 页 iframe 打开。</div>
      <table><thead><tr><th>Name</th><th>Kind</th><th>Target</th><th>Provider</th><th></th></tr></thead><tbody id="tunnelRows"></tbody></table>
      <div class="cmd-line" id="cmdLine"></div></section>
  </div>
  <div class="view" id="view-cloud">
    <div class="cloud-layout">
      <div>
        <section class="panel" style="margin-bottom:0">
          <h2>Cloud nodes <span class="muted" id="nodeHint"></span></h2>
          <div id="nodeList"></div>
          <div class="node-add">
            <input id="nodeName" placeholder="name · caicai headless">
            <input id="nodeUrl" placeholder="http://127.0.0.1:19034">
            <button class="btn btn-primary" id="addNodeBtn">+ Add</button>
          </div>
        </section>
      </div>
      <div class="iframe-wrap" id="iframeWrap">
        <div class="iframe-empty" id="iframeEmpty"><div class="big">🖥️</div><div id="iframeEmptyText">选择一个云节点入口，经 console 反向代理 iframe 内嵌。</div></div>
        <iframe id="nodeFrame" style="display:none"></iframe>
      </div>
    </div>
  </div>
</div>
<script>
{js}
</script>
</body>
</html>"#,
        css = CSS,
        js = JS
    )
}

/// Login page (brand Teamx Console).
pub fn login_html() -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Login | Teamx Console</title>
<style>
{css}
body {{ display:flex; min-height:100vh; flex-direction:column; }}
header {{ position:static; }}
.main {{ flex:1; display:flex; align-items:center; justify-content:center; padding:40px 20px; }}
.card {{ max-width:420px; width:100%; background:var(--card); border:1px solid var(--border); border-radius:var(--radius); box-shadow:var(--shadow); padding:40px 44px; text-align:center; }}
.card h1 {{ font-size:20px; font-weight:600; margin:0 0 6px; }}
.subtitle {{ font-size:13px; color:var(--muted-fg); margin-bottom:26px; }}
.form {{ display:flex; flex-direction:column; gap:12px; }}
.input {{ font-family:var(--font-mono); }}
.note {{ margin-top:16px; font-size:12px; color:var(--muted-fg); line-height:1.7; }}
</style></head>
<body>
<header>
  <div class="logo"><div class="logo-icon">Tx</div><span class="logo-text">Teamx Console</span></div>
</header>
<div class="main"><div class="card">
  <h1>Teamx Console</h1>
  <div class="subtitle">Member workspace · enter your access token to login</div>
  <div class="form">
    <input class="input" id="tokenInput" placeholder="Paste token here…" autofocus autocomplete="off" spellcheck="false">
    <button class="btn btn-primary" id="loginBtn">Login</button>
    <div class="error" id="errMsg" style="display:none">Invalid token, please try again.</div>
  </div>
  <div class="note">启动 `teamx console` 时终端会打印一次性 token（每次启动不同）。<br>console 只读展示当前工作空间（.teamx）里的团队信息。</div>
</div></div>
<script>
(function(){{
  var b=document.getElementById("loginBtn"),i=document.getElementById("tokenInput"),e=document.getElementById("errMsg");
  function go(){{ var t=i.value.trim(); if(!t){{i.focus();return}} e.style.display="none"; b.disabled=true; b.textContent="Logging in…"; location.href="/?token="+encodeURIComponent(t); }}
  b.addEventListener("click",go); i.addEventListener("keydown",function(ev){{ if(ev.key==="Enter")go(); }});
}})();
</script>
</body></html>"#,
        css = CSS
    )
}

/// Unauthorized page (401).
pub fn unauthorized_html() -> String {
    message_page("401 - Unauthorized", "请输入有效的访问 token 登录 Teamx Console。", "/login", "Go to login")
}

/// Not-found page.
pub fn not_found_html() -> String {
    message_page("404 - Not Found", "页面不存在（Teamx Console）。", "/", "Back to console")
}

fn message_page(title: &str, msg: &str, href: &str, btn: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>
{css}
body {{ display:flex; min-height:100vh; flex-direction:column; }}
header {{ position:static; }}
.main {{ flex:1; display:flex; align-items:center; justify-content:center; padding:40px 20px; }}
.card {{ max-width:460px; width:100%; background:var(--card); border:1px solid var(--border); border-radius:var(--radius); box-shadow:var(--shadow); padding:48px 44px; text-align:center; }}
.card .big {{ font-size:40px; margin-bottom:12px; }}
.card h1 {{ font-size:18px; font-weight:600; margin:0 0 8px; }}
.card p {{ font-size:13px; color:var(--muted-fg); margin:0 0 22px; }}
</style></head>
<body>
<header><div class="logo"><div class="logo-icon">Tx</div><span class="logo-text">Teamx Console</span></div></header>
<div class="main"><div class="card"><div class="big">🛡️</div><h1>{title}</h1><p>{msg}</p><a class="btn btn-primary" href="{href}" style="display:inline-block;text-decoration:none">{btn}</a></div></div>
</body></html>"#,
        title = title,
        msg = msg,
        href = href,
        btn = btn,
        css = CSS
    )
}

const CSS: &str = r#"
:root {
  --primary:#4A90E5; --primary-hover:#357ABD; --primary-light:#EBF3FC; --on-primary:#FFFFFF;
  --secondary:#67C23A; --accent:#E6A23C; --danger:#F56C6C;
  --bg:#f8f8f8; --fg:#333333; --card:#FFFFFF; --card-fg:#333333;
  --muted:#F2F6FC; --muted-fg:#909399; --border:#EBEEF5; --ring:#4A90E5; --radius:4px;
  --shadow:0 2px 4px 0 rgba(214,228,241,0.7); --shadow-hover:0 4px 12px rgba(214,228,241,0.9);
  --font-sans:"Microsoft YaHei","PingFang SC",-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;
  --font-mono:"SF Mono","Fira Code",ui-monospace,SFMono-Regular,Menlo,monospace;
  --transition:200ms ease;
}
.dark {
  --primary:#5B9EF0; --primary-hover:#7BB3F5; --primary-light:#1A2A3A; --on-primary:#FFFFFF;
  --bg:#1A1A2E; --fg:#E0E0E0; --card:#22223A; --card-fg:#E0E0E0; --muted:#2A2A40; --muted-fg:#909399;
  --border:#333355; --ring:#5B9EF0; --shadow:0 2px 4px 0 rgba(0,0,0,0.3); --shadow-hover:0 4px 12px rgba(0,0,0,0.4);
}
* { box-sizing:border-box; }
body { font-family:var(--font-sans); font-size:14px; line-height:1.5; margin:0; background:var(--bg); color:var(--fg); -webkit-font-smoothing:antialiased; }
header { position:sticky; top:0; z-index:100; display:flex; align-items:center; height:60px; padding:0 20px; background:var(--card); border-bottom:1px solid var(--border); box-shadow:0 1px 4px rgba(0,0,0,0.08); }
header .logo { display:flex; align-items:center; gap:10px; margin-right:24px; flex-shrink:0; }
header .logo .logo-icon { width:32px; height:32px; background:linear-gradient(135deg,#4A90E5,#357ABD); border-radius:6px; display:flex; align-items:center; justify-content:center; color:#fff; font-weight:700; font-size:16px; }
header .logo-text { font-size:16px; font-weight:600; color:var(--primary); letter-spacing:0.02em; }
nav { display:flex; gap:0; flex:1; justify-content:center; }
nav button { background:none; border:none; font:inherit; font-size:14px; font-weight:500; color:var(--muted-fg); padding:18px 18px; border-bottom:2px solid transparent; cursor:pointer; transition:color var(--transition),border-color var(--transition); }
nav button:hover { color:var(--primary); }
nav button.active { color:var(--primary); border-bottom-color:var(--primary); font-weight:600; }
.header-right { margin-left:auto; display:flex; align-items:center; gap:16px; flex-shrink:0; }
.user-info { display:flex; align-items:center; gap:8px; font-size:13px; color:#666; }
.user-avatar { width:30px; height:30px; border-radius:50%; background:var(--primary-light); color:var(--primary); display:flex; align-items:center; justify-content:center; font-weight:600; font-size:13px; }
.theme-toggle { width:34px; height:34px; border:1px solid var(--border); border-radius:var(--radius); background:var(--card); color:var(--fg); cursor:pointer; display:flex; align-items:center; justify-content:center; font-size:16px; }
.toolbar { display:flex; justify-content:space-between; align-items:center; gap:14px; flex-wrap:wrap; padding:10px 20px; background:var(--card); border-bottom:1px solid var(--border); }
.ws { font-size:12.5px; color:var(--muted-fg); font-family:var(--font-mono); }
.ws b { color:var(--fg); font-weight:600; }
.toolbar-right { margin-left:auto; display:flex; gap:10px; align-items:center; }
.container { width:100%; padding:20px 24px; }
.view { display:none; }
.view.active { display:block; animation:fadein 200ms ease; }
@keyframes fadein { from { opacity:0; transform:translateY(4px); } to { opacity:1; transform:none; } }
.cards { display:grid; grid-template-columns:repeat(4,1fr); gap:16px; margin-bottom:20px; }
@media (max-width:900px){ .cards { grid-template-columns:repeat(2,1fr); } }
.card { background:var(--card); border-radius:var(--radius); padding:18px 20px; box-shadow:var(--shadow); transition:box-shadow var(--transition); display:flex; align-items:center; gap:16px; }
.card:hover { box-shadow:var(--shadow-hover); }
.card-icon { width:46px; height:46px; border-radius:10px; display:flex; align-items:center; justify-content:center; font-size:21px; flex-shrink:0; }
.card-icon.blue { background:#EBF5FF; color:#4A90E5; } .card-icon.green { background:#E8F8E8; color:#67C23A; }
.card-icon.orange { background:#FDF0E0; color:#E6A23C; } .card-icon.purple { background:#F0E8FF; color:#9B59B6; }
.card .label { font-size:13px; color:var(--muted-fg); margin-bottom:4px; }
.card .value { font-size:23px; font-weight:600; color:var(--fg); line-height:1.2; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
section.panel { background:var(--card); border-radius:var(--radius); padding:16px; margin-bottom:16px; box-shadow:var(--shadow); }
h2 { font-size:14px; font-weight:600; margin:0 0 12px; color:var(--fg); }
h2 .count { font-family:var(--font-mono); font-weight:500; color:var(--muted-fg); margin-left:6px; }
table { width:100%; border-collapse:collapse; font-size:13px; }
th,td { text-align:left; padding:9px 12px; border-bottom:1px solid var(--border); vertical-align:middle; }
tbody tr { transition:background var(--transition); } tbody tr:hover { background:var(--muted); }
th { color:var(--muted-fg); font-weight:600; font-size:12px; background:var(--muted); }
.badge { display:inline-block; padding:2px 8px; border-radius:3px; font-size:12px; font-weight:500; line-height:1.5; white-space:nowrap; }
.badge.proposed,.badge.blocked,.badge.open { background:#FDF6EC; color:#E6A23C; }
.badge.shared,.badge.in_progress,.badge.active,.badge.running,.badge.lead { background:#EBF5FF; color:#4A90E5; }
.badge.achieved,.badge.completed,.badge.closed,.badge.done,.badge.contributor { background:#E8F8E8; color:#67C23A; }
.badge.refining,.badge.review,.badge.reviewer { background:#F0E8FF; color:#9B59B6; }
.badge.owner,.badge.searcher,.badge.extractor,.badge.kb-engineer { background:#EBF5FF; color:#4A90E5; }
.badge.idle,.badge.pending { background:#F2F6FC; color:#909399; }
.badge.waiting,.badge.high { background:#FDF6EC; color:#E6A23C; }
.badge.left,.badge.medium { background:#FDE8E8; color:#F56C6C; }
.badge.low { background:#E8F8E8; color:#67C23A; }
.muted { color:var(--muted-fg); font-size:12px; }
.mono { font-family:var(--font-mono); }
.loading { color:var(--muted-fg); font-size:13px; padding:20px; text-align:center; }
.errorbox { background:#FDE8E8; color:#c0392b; border:1px solid var(--border); border-radius:var(--radius); padding:14px; font-size:13px; margin-bottom:16px; }
.errorbox .mono { font-family:var(--font-mono); font-size:12px; word-break:break-all; }
pre.raw { background:var(--muted); border:1px solid var(--border); border-radius:var(--radius); padding:12px; font-family:var(--font-mono); font-size:12px; overflow:auto; max-height:520px; }
.ev-list { list-style:none; margin:0; padding:0; }
.ev-item { display:flex; align-items:flex-start; gap:10px; padding:8px 0; border-bottom:1px solid var(--border); }
.ev-item:last-child { border-bottom:none; }
.ev-time { font-family:var(--font-mono); font-size:11.5px; color:var(--muted-fg); flex-shrink:0; padding-top:2px; width:140px; }
.ev-kind { flex-shrink:0; min-width:80px; text-align:center; }
.ev-text { flex:1; min-width:0; word-break:break-word; font-size:13px; color:var(--fg); }
.ev-text .who { color:var(--muted-fg); font-size:12px; }
.copy-btn { font-size:12px; padding:3px 10px; }
.cmd-line { font-family:var(--font-mono); font-size:12px; background:var(--muted); border:1px solid var(--border); border-radius:var(--radius); padding:8px 10px; margin-top:8px; word-break:break-all; display:none; color:var(--fg); }
.cloud-layout { display:grid; grid-template-columns:320px 1fr; gap:16px; }
@media (max-width:1000px){ .cloud-layout { grid-template-columns:1fr; } }
.node-card { background:var(--card); border:1px solid var(--border); border-radius:var(--radius); padding:12px 14px; margin-bottom:10px; cursor:pointer; transition:border-color var(--transition),box-shadow var(--transition); }
.node-card:hover { border-color:var(--primary); box-shadow:var(--shadow-hover); }
.node-card.sel { border-color:var(--primary); box-shadow:0 0 0 2px rgba(74,144,229,0.15); }
.node-title { display:flex; align-items:center; justify-content:space-between; gap:8px; font-weight:600; font-size:13.5px; }
.node-title .del { color:var(--danger); background:none; border:none; cursor:pointer; font-size:14px; padding:0 4px; }
.node-url { font-family:var(--font-mono); font-size:11.5px; color:var(--muted-fg); margin-top:4px; word-break:break-all; }
.node-add { display:flex; gap:8px; margin-top:12px; flex-wrap:wrap; }
.node-add input { font:inherit; font-size:13px; padding:6px 10px; border:1px solid var(--border); border-radius:var(--radius); background:var(--card); color:var(--fg); min-width:0; }
.node-add input:first-child { width:140px; } .node-add input:nth-child(2){ flex:1; }
.iframe-wrap { background:var(--card); border:1px solid var(--border); border-radius:var(--radius); overflow:hidden; box-shadow:var(--shadow); position:relative; min-height:520px; }
.iframe-wrap iframe { width:100%; height:calc(100vh - 230px); min-height:520px; border:0; display:block; }
.iframe-empty { padding:70px 24px; text-align:center; color:var(--muted-fg); font-size:13px; }
.iframe-empty .big { font-size:36px; margin-bottom:10px; }
.btn { padding:6px 14px; border:1px solid var(--border); border-radius:var(--radius); background:var(--card); color:var(--fg); font-size:13px; font-weight:500; cursor:pointer; transition:all var(--transition); }
.btn:hover { border-color:var(--primary); color:var(--primary); }
.btn-primary { background:var(--primary); border-color:var(--primary); color:var(--on-primary); }
.btn-primary:hover { background:var(--primary-hover); color:var(--on-primary); }
a.btn { text-decoration:none; }
"#;

const JS: &str = r##"
const $=(s)=>document.querySelector(s);
const $$=(s)=>Array.from(document.querySelectorAll(s));
const esc=(s)=>String(s??"").replace(/[&<>"']/g,c=>({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[c]));
const badge=(s)=>{const b=s||"idle";return `<span class="badge ${esc(b)}">${esc(b)}</span>`};
const tf=(iso)=>{ if(!iso) return "—"; const d=new Date(iso); return isNaN(d)?iso:d.toLocaleString(); };

/* ---------- theme (same behavior as teamx ui) ---------- */
const themeKey="teamx-console-theme";
if(localStorage.getItem(themeKey)==="dark")document.body.classList.add("dark");
const applyTheme=()=>$("#themeToggle").textContent=document.body.classList.contains("dark")?"☀️":"🌙";
applyTheme();
$("#themeToggle").addEventListener("click",()=>{document.body.classList.toggle("dark");localStorage.setItem(themeKey,document.body.classList.contains("dark")?"dark":"light");applyTheme();});

$("#wsPath").textContent = window.location.origin.replace(/^https?:\/\//,"") + "  ·  cwd .teamx";

/* ---------- panels ---------- */
let currentView="teams";
function showView(name){ currentView=name; $$("nav button").forEach(b=>b.classList.toggle("active",b.dataset.view===name)); $$(".view").forEach(v=>v.classList.toggle("active",v.id==="view-"+name)); renderView(name); }
async function renderView(name){
  if(name==="teams") loadTeams(); else if(name==="tasks") loadTasks();
  else if(name==="events") loadEvents(); else if(name==="tunnels") loadTunnels(); else if(name==="cloud") loadCloud();
}
$$("nav button").forEach(b=>b.addEventListener("click",()=>showView(b.dataset.view)));
$("#refreshBtn").addEventListener("click",()=>renderView(currentView));

async function panel(id){
  const r=await fetch("/api/panels/"+id,{cache:"no-store"});
  if(!r.ok){ const txt=await r.text(); return {ok:false,error:("HTTP "+r.status+" "+(txt||"").slice(0,200))}; }
  return r.json();
}
function setLast(p){ $("#lastRun").textContent = p.ok?("✓ "+p.cmd+" ["+p.mode+"] "+new Date().toLocaleTimeString()):"✗ "+p.cmd; }
function errBox(p){ return `<div class="errorbox"><b>命令未成功</b><br><span class="mono">${esc(p.cmd||"")}</span><br>${esc(p.error||"unknown error")}</div>`; }
function rawPre(label,v){ return `<section class="panel"><h2>${esc(label)} <span class="muted">(raw)</span></h2><pre class="raw">${esc(typeof v==="string"?v:JSON.stringify(v,null,2))}</pre></section>`; }

/* ---------- Teams ---------- */
async function loadTeams(){
  $("#teamBody").innerHTML=`<div class="loading">loading …</div>`;
  const p=await panel("teams"); setLast(p);
  const teams=(p.ok&&p.data&&p.data.teams)||[];
  $("#teamCards").innerHTML=[
    `<div class="card"><div class="card-icon blue">👥</div><div class="card-body"><div class="label">My teams</div><div class="value">${teams.length}</div></div></div>`,
    `<div class="card"><div class="card-icon orange">🎯</div><div class="card-body"><div class="label">Goal state</div><div class="value">${esc((teams[0]&&teams[0].goal&&teams[0].goal.state)||"—")}</div></div></div>`,
    `<div class="card"><div class="card-icon green">🧩</div><div class="card-body"><div class="label">My role</div><div class="value">${esc((teams[0]&&teams[0].my&&teams[0].my.role)||"—")}</div></div></div>`,
    `<div class="card"><div class="card-icon purple">🟢</div><div class="card-body"><div class="label">My state</div><div class="value">${esc((teams[0]&&teams[0].my&&teams[0].my.state)||"—")}</div></div></div>`
  ].join("");
  if(!p.ok){ $("#teamBody").innerHTML=errBox(p); return; }
  $("#teamBody").innerHTML = teams.map(t=>{
    const g=t.goal||{}; const members=t.members||[];
    return `<section class="panel">
      <h2>${esc(t.name||t.team&&t.team.name||"(team)")} ${badge(t.state||(t.team&&t.team.state))} ${g.title?`<span class="muted" style="font-weight:400">— ${esc(g.title)}</span>`:""}</h2>
      ${g.title?`<div style="margin-bottom:12px">${badge(g.state)} <span class="muted">goal</span></div>`:""}
      <div class="muted" style="margin-bottom:8px">Members · ${members.length}</div>
      <table><thead><tr><th>Member</th><th>Role</th><th>State</th><th>Online</th></tr></thead><tbody>
        ${members.map(m=>`<tr><td>${esc(m.display_name)}</td><td>${badge(m.role)}</td><td>${badge(m.state)}</td><td>${m.online?"<span class='badge running'>● online</span>":"<span class='badge idle'>○ offline</span>"}</td></tr>`).join("")||`<tr><td colspan=4 class="muted">no members</td></tr>`}
      </tbody></table></section>`;
  }).join("") || `<section class="panel"><div class="muted">你尚未加入任何团队。</div></section>`;
}

/* ---------- My Tasks ---------- */
async function loadTasks(){
  $("#taskRows").innerHTML=`<tr><td colspan=4 class="loading">loading …</td></tr>`;
  const p=await panel("tasks"); setLast(p);
  if(!p.ok){ $("#taskRows").innerHTML=""; const host=$("#view-tasks .panel"); host.insertAdjacentHTML("afterbegin",errBox(p)); return; }
  let arr=[]; const d=p.data;
  if(Array.isArray(d)) arr=d; else if(d&&Array.isArray(d.tasks)) arr=d.tasks;
  $("#taskCount").textContent=arr.length;
  $("#taskRows").innerHTML = arr.map(x=>`<tr><td>${esc(x.title||x.id||JSON.stringify(x).slice(0,60))}</td><td>${badge(x.state||x.status)}</td><td>${badge(x.priority||"medium")}</td><td class="mono" style="font-size:12px;color:var(--muted-fg)">${esc(x.updated_at||x.updated||"")}</td></tr>`).join("")
    || `<tr><td colspan=4 class="muted">当前工作空间没有分派给你的 taskx 任务。</td></tr>`;
}

/* ---------- Events ---------- */
async function loadEvents(){
  $("#evList").innerHTML=`<li class="loading">loading …</li>`;
  const p=await panel("events"); setLast(p);
  if(!p.ok){ $("#evList").innerHTML=""; const host=$("#view-events .panel"); host.insertAdjacentHTML("afterbegin",errBox(p)); return; }
  let evs=[]; const d=p.data;
  if(d&&Array.isArray(d.events)) evs=d.events; else if(Array.isArray(d)) evs=d;
  evs=evs.slice(0,80);
  $("#evCount").textContent=evs.length;
  $("#evList").innerHTML = evs.map(e=>{
    const kind=(e.type||e.kind||"").replace(/^[a-z]+\./,"");
    const who=e.member_id||""; const pl=e.payload||{};
    const text=summarize(e,pl);
    return `<li class="ev-item"><span class="ev-time">${esc(tf(e.created_at))}</span><span class="ev-kind">${badge(kind)}</span><div class="ev-text">${esc(text)} <span class="who">· ${esc((who).slice(0,12))}</span></div></li>`;
  }).join("") || `<li class="muted">暂无事件。</li>`;
}
function summarize(e,pl){
  if(pl&&pl.message) return String(pl.message);
  if(pl&&pl.display_name&&e.type&&e.type.includes("member")) return `${pl.display_name} · ${e.type}`;
  if(e.type==="team.created"&&pl.name) return `团队创建：${pl.name}`;
  if(e.type==="goal.set"&&pl.title) return `目标更新：${pl.title}`;
  const keys=["text","title","name","detail"];
  for(const k of keys){ if(pl&&pl[k]) return String(pl[k]); }
  try{ return JSON.stringify(pl).slice(0,120); }catch(_){ return e.type; }
}

/* ---------- Tunnels ---------- */
async function loadTunnels(){
  $("#tunnelRows").innerHTML=`<tr><td colspan=5 class="loading">loading …</td></tr>`;
  const p=await panel("tunnels"); setLast(p);
  if(!p.ok){ $("#tunnelRows").innerHTML=""; const host=$("#view-tunnels .panel"); host.insertAdjacentHTML("afterbegin",errBox(p)); return; }
  let arr=[]; const d=p.data;
  if(d&&Array.isArray(d.tunnels)) arr=d.tunnels; else if(Array.isArray(d)) arr=d;
  $("#tunCount").textContent=arr.length;
  $("#cmdLine").style.display="none";
  $("#tunnelRows").innerHTML = arr.map(x=>{
    const name=x.name||""; const target=x.target_port||x.port||""; const provider=x.provider_member_id||"";
    const kind = name.startsWith("ttyd")?"ttyd":name.startsWith("opencode")?"headless":"tunnel";
    return `<tr><td class="tunnel-name">${esc(name)}</td><td>${badge(kind==="headless"?"running":kind==="ttyd"?"active":"open")}<span class="muted"> ${esc(kind)}</span></td><td class="mono">${esc(target)}</td><td class="mono" style="font-size:12px">${esc(provider.slice(0,8))}…</td><td><button class="btn copy-btn" data-name="${esc(name)}">复制 forward</button></td></tr>`;
  }).join("")||`<tr><td colspan=5 class="muted">当前团队没有已暴露的隧道（或项目模式下无服务器）。</td></tr>`;
  $$(".copy-btn").forEach(b=>b.addEventListener("click",()=>{
    const cmd=`teamx tunnel forward ${b.dataset.name} --session <your-session>`;
    $("#cmdLine").textContent=cmd; $("#cmdLine").style.display="block";
    if(navigator.clipboard)navigator.clipboard.writeText(cmd);
  }));
}

/* ---------- Cloud (iframe via console reverse proxy) ---------- */
let nodes=[], selNode="";
async function loadCloud(){ const r=await fetch("/api/cloud",{cache:"no-store"}); if(r.ok){ const c=await r.json(); nodes=(c.nodes||[]); } renderNodes(); }
function renderNodes(){
  $("#nodeHint").textContent = nodes.length?("· "+nodes.length):"";
  $("#nodeList").innerHTML = nodes.map(n=>`<div class="node-card ${n.id===selNode?"sel":""}" data-node="${esc(n.id)}">
      <div class="node-title"><span>${esc(n.name)}</span><span><span class="badge running">${esc(n.kind)}</span> <button class="del" title="remove" data-del="${esc(n.id)}">✕</button></span></div>
      <div class="node-url">${esc(n.url)}</div></div>`).join("") || `<div class="muted" style="padding:8px 0">尚未添加云节点。先 tunnel forward，再添加本地地址。</div>`;
  $$("#nodeList .node-card").forEach(c=>c.addEventListener("click",(e)=>{ if(e.target.dataset.del) return; selNode=c.dataset.node; renderNodes(); openNode(selNode); }));
  $$("#nodeList [data-del]").forEach(b=>b.addEventListener("click",async(e)=>{ e.stopPropagation(); const id=b.dataset.del; await fetch("/api/cloud/"+encodeURIComponent(id),{method:"DELETE"}); if(selNode===id)selNode=""; loadCloud(); }));
  if(selNode) openNode(selNode);
}
function openNode(id){
  const n=nodes.find(x=>x.id===id); if(!n) return;
  const frame=$("#nodeFrame"), empty=$("#iframeEmpty");
  empty.style.display=""; frame.style.display="none";
  $("#iframeEmptyText").innerHTML=`加载 ${esc(n.name)} …<br><span class="muted">经 console 同源反代 /api/cloud/${esc(n.id)}/（剥离 X-Frame-Options）</span><br><br><a class="btn" target="_blank" href="${esc(n.url)}">在新标签页打开</a>`;
  frame.src="/api/cloud/"+encodeURIComponent(n.id)+"/";
  frame.onload=()=>{ empty.style.display="none"; frame.style.display="block"; };
  frame.onerror=()=>{ empty.style.display=""; $("#iframeEmptyText").textContent="代理加载失败，请确认该端口已 tunnel forward 到本机。"+n.url; };
}
$("#addNodeBtn").addEventListener("click",async()=>{
  const name=$("#nodeName").value.trim(), url=$("#nodeUrl").value.trim();
  if(!name||!url){ alert("请输入 name 与本地 URL"); return; }
  const r=await fetch("/api/cloud",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({name,url})});
  if(!r.ok){ alert("添加失败: "+await r.text()); return; }
  const c=await r.json(); nodes=c.nodes||nodes;
  $("#nodeName").value=""; $("#nodeUrl").value="";
  renderNodes();
});
loadCloud();
showView("teams");
"##;
