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
    <button data-view="teams" class="active">Members</button>
    <button data-view="tasks">My Tasks</button>
    <button data-view="events">Events</button>
  </nav>
  <div class="header-right">
    <button class="theme-toggle" id="themeToggle" title="Toggle light/dark mode">☀️</button>
    <div class="user-info"><div class="user-avatar" id="meAvatar">?</div><span id="meName">Member</span></div>
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
  <div class="view active" id="view-teams">
    <div class="cards" id="teamCards"></div>
    <div id="teamBody"></div>
  </div>
  <div class="view" id="view-tasks">
    <section class="panel"><h2>Tasks assigned to me <span class="count" id="taskCount"></span></h2>
      <table><thead><tr><th>Title</th><th>State</th><th>Priority</th><th>Updated</th></tr></thead><tbody id="taskRows"></tbody></table>
    </section>
  </div>
  <div class="view" id="view-events">
    <section class="panel"><h2>Recent team events <span class="count" id="evCount"></span></h2>
      <ul class="ev-list" id="evList"></ul></section>
  </div>
</div>
<div class="fs-overlay" id="fsOverlay" style="display:none">
  <div class="fs-dialog">
    <div class="fs-head">
      <span class="fs-title">📁 <span id="fsWho">文件浏览器</span></span>
      <div class="fs-headbtns">
        <label class="btn btn-mini">⬆ 上传<input type="file" id="fsFile" style="display:none"></label>
        <button class="btn btn-mini" id="fsRefresh">⟳</button>
        <button class="btn btn-mini" id="fsClose">✕</button>
      </div>
    </div>
    <div class="fs-breadcrumb" id="fsCrumb">/</div>
    <div class="fs-body">
      <div class="fs-left" id="fsRows"></div>
      <div class="fs-right" id="fsPreview"><div class="fs-preview-empty">选择文件预览</div></div>
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
/// Login page — mirrors the `teamx ui` login (same layout/CSS), branded
/// "Teamx Console". No extra hint text.
pub fn login_html() -> String {
    r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Login | Teamx Console</title>
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
    <span class="logo-text">Teamx Console</span>
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
    <h1>Teamx Console</h1>
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
</html>"#
    .to_string()
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
tr.user-group td { background:var(--muted); color:var(--muted-fg); font-weight:600; font-size:12.5px; letter-spacing:0.02em; }
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
.btn-mini { font-size:12px; padding:3px 10px; }
.btn-mini.busy { opacity:.7; }
.tun-chip { display:inline-flex; align-items:center; gap:4px; background:var(--muted); border:1px solid var(--border); border-radius:12px; padding:2px 8px; margin:2px 4px 2px 0; font-size:11.5px; color:var(--fg); }
.tun-chip .dot { width:7px; height:7px; border-radius:50%; background:#67C23A; }
.btn { padding:6px 14px; border:1px solid var(--border); border-radius:var(--radius); background:var(--card); color:var(--fg); font-size:13px; font-weight:500; cursor:pointer; transition:all var(--transition); }
.btn:hover { border-color:var(--primary); color:var(--primary); }
.btn-primary { background:var(--primary); border-color:var(--primary); color:var(--on-primary); }
.btn-primary:hover { background:var(--primary-hover); color:var(--on-primary); }

/* ---------- file browser overlay ---------- */
.fs-overlay { position:fixed; inset:0; background:rgba(20,20,30,.45); z-index:900; display:flex; align-items:center; justify-content:center; padding:24px; }
.fs-dialog { background:var(--card); border:1px solid var(--border); border-radius:var(--radius); box-shadow:var(--shadow-hover); width:min(1100px,96vw); height:min(720px,92vh); display:flex; flex-direction:column; overflow:hidden; }
.fs-head { display:flex; align-items:center; gap:12px; padding:10px 14px; background:var(--muted); border-bottom:1px solid var(--border); }
.fs-title { flex:1; font-weight:600; font-size:14px; }
.fs-headbtns { display:flex; gap:8px; }
.fs-breadcrumb { padding:6px 14px; font-family:var(--font-mono); font-size:12px; color:var(--primary); border-bottom:1px solid var(--border); background:var(--card); }
.fs-body { flex:1; display:flex; min-height:0; }
.fs-left { width:46%; min-width:300px; overflow:auto; border-right:1px solid var(--border); }
.fs-right { flex:1; overflow:auto; padding:12px; background:var(--card); }
.fs-row { display:flex; align-items:center; gap:10px; padding:6px 12px; border-bottom:1px solid var(--border); font-size:13px; cursor:pointer; }
.fs-row:hover { background:var(--muted); }
.fs-row .ic { width:20px; text-align:center; }
.fs-row .nm { flex:1; min-width:0; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
.fs-row .meta { color:var(--muted-fg); font-size:11.5px; font-family:var(--font-mono); }
.fs-preview-empty { color:var(--muted-fg); text-align:center; padding-top:80px; }
.fs-right pre { background:var(--muted); border:1px solid var(--border); border-radius:var(--radius); padding:12px; font-family:var(--font-mono); font-size:12px; white-space:pre-wrap; word-break:break-word; max-height:640px; overflow:auto; }
.fs-right img { max-width:100%; }
.fs-btn-actions { display:flex; gap:6px; align-items:center; }
.fs-btn-actions .act { font-size:12px; cursor:pointer; color:var(--muted-fg); }
.fs-btn-actions .act:hover { color:var(--primary); }
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

/* ---------- header identity (current workspace member) ---------- */
async function loadMe(){
  try {
    const r=await fetch("/api/me",{cache:"no-store"});
    if(!r.ok) return;
    const p=await r.json();
    if(p.ok && p.data){
      window.__me=p.data;
      const name=p.data.name||"Member";
      $("#meName").textContent=name;
      $("#meAvatar").textContent=(name.replace(/\(.*\)$/,"").trim()[0]||"?").toUpperCase();
    }
  } catch(_){ /* keep Member fallback */ }
}

/* ---------- panels ---------- */
let currentView="teams";
function showView(name){ currentView=name; $$("nav button").forEach(b=>b.classList.toggle("active",b.dataset.view===name)); $$(".view").forEach(v=>v.classList.toggle("active",v.id==="view-"+name)); renderView(name); }
async function renderView(name){
  if(name==="teams") loadTeams(); else if(name==="tasks") loadTasks();
  else if(name==="events") loadEvents();
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

/* ---------- Teams (+ per-member tunnel open dock) ---------- */
const kindOf=(name)=>(name||"").includes("ttyd")?"ttyd":((name||"").includes("opencode")?"headless":"web");
const openLabel=(kind,name)=>{
  if(kind==="ttyd") return {icon:"🖥", label:"TUI"};
  if(kind==="headless") return {icon:"🌐", label:"Web"};
  return {icon:"🌐", label:"Admin"}; // teamx-ui etc.
};
async function loadTeams(){
  $("#teamBody").innerHTML=`<div class="loading">loading …</div>`;
  const [p, pt] = await Promise.all([panel("teams"), panel("tunnels")]);
  if(p.ok) setLast(p); else if(pt.ok) setLast(pt);
  const teams=(p.ok&&p.data&&p.data.teams)||[];
  const tuns=[];
  if(pt.ok){
    const d=pt.data;
    if(d&&Array.isArray(d.tunnels)) tuns.push(...d.tunnels);
    else if(Array.isArray(d)) tuns.push(...d);
  }
  // tunnel provider -> list
  const byMember=new Map();
  window.__memTunnels=new Map();
  for(const t of tuns){
    const k=t.provider_member_id||"_";
    if(!byMember.has(k)) byMember.set(k,[]);
    byMember.get(k).push(t);
  }
  $("#teamCards").innerHTML=[
    `<div class="card"><div class="card-icon blue">👥</div><div class="card-body"><div class="label">My teams</div><div class="value">${teams.length}</div></div></div>`,
    `<div class="card"><div class="card-icon orange">🎯</div><div class="card-body"><div class="label">Goal state</div><div class="value">${esc((teams[0]&&teams[0].goal&&teams[0].goal.state)||"—")}</div></div></div>`,
    `<div class="card"><div class="card-icon green">🧩</div><div class="card-body"><div class="label">My role</div><div class="value">${esc((teams[0]&&teams[0].my&&teams[0].my.role)||"—")}</div></div></div>`,
    `<div class="card"><div class="card-icon purple">🟢</div><div class="card-body"><div class="label">Tunnels</div><div class="value">${tuns.length}</div></div></div>`
  ].join("");
  if(!p.ok && !pt.ok){ $("#teamBody").innerHTML=(p.error?errBox(p):errBox(pt)); return; }
  $("#teamBody").innerHTML = teams.map(t=>{
    const g=t.goal||{};
    const members=(t.members||[]).filter(m=>m.state!=="left"&&m.state!=="denied");
    // Group rows by the underlying person ("用户"): a user may hold several
    // member seats (personal + workers). Groups are sorted by user name.
    const fmtN=(n)=>{ n=Number(n)||0; return n>=1e9?(n/1e9).toFixed(1)+"B": n>=1e6?(n/1e6).toFixed(1)+"M": n>=1e3?(n/1e3).toFixed(1)+"k": String(n); };
    const fmt$=(c)=>{ c=Number(c)||0; return "$"+ (c>=1? c.toFixed(2): c.toFixed(4)); };
    const groups=new Map();
    for(const m of members){
      const ukey = (m.user&&m.user.display_name) || "(no user)";
      if(!groups.has(ukey)) groups.set(ukey,{ name: (m.user&&m.user.display_name)||m.display_name, members: [] });
      groups.get(ukey).members.push(m);
    }
    const groupArr=[...groups.entries()].sort((a,b)=>a[1].name.localeCompare(b[1].name));
    const rows=[];
    for(const [,grp] of groupArr){
      rows.push(`<tr class="user-group"><td colspan="9">👤 ${esc(grp.name)} · ${grp.members.length} seat(s)</td></tr>`);
      for(const m of grp.members){
        const mt=byMember.get(m.id)||[];
        window.__memTunnels.set(m.id, mt);
        const act=m.activity||{};
        rows.push(`<tr>
          <td class="mono" style="font-size:11px;color:var(--muted-fg)">${m.user?esc(m.user.display_name):"—"}</td>
          <td>${esc(m.display_name)}</td>
          <td>${badge(m.role)}</td>
          <td>${badge(m.state)}</td>
          <td>${m.online?"<span class='badge running'>● online</span>":"<span class='badge idle'>○ offline</span>"}</td>
          <td class="mono" style="font-size:11px">${esc(m.ip||"—")}</td>
          <td class="mono" style="font-size:11px">${fmtN(act.tokens)} tok · ${fmt$(act.cost)}</td>
          <td>${mt.length?mt.map(x=>`<span class="tun-chip"><span class="dot"></span>${esc(kindOf(x.name))} · ${esc(x.name)}</span>`).join(""):'<span class="muted">—</span>'}</td>
          <td><button class="btn btn-mini fs-btn" data-mid="${esc(m.id)}" data-name="${esc(m.display_name)}" title="文件浏览器">📁</button> ${mtOpen.length?mtOpen.map(x=>{const oi=openLabel(kindOf(x.name),x.name); return `<button class="btn btn-mini" data-member="${esc(m.display_name)}" data-name="${esc(x.name)}" data-kind="${esc(kindOf(x.name))}">${oi.icon} ${esc(oi.label)}</button>`;}).join(" "):''}</td>
        </tr>`);
      }
    }
    const table=rows.join("");
    return `<section class="panel">
      <h2>${esc(t.name||(t.team&&t.team.name)||"(team)")} ${badge(t.state||(t.team&&t.team.state))} ${g.title?`<span class="muted" style="font-weight:400">— ${esc(g.title)}</span>`:""}</h2>
      ${g.title?`<div style="margin-bottom:10px">${badge(g.state)} <span class="muted">goal</span></div>`:""}
      <table><thead><tr><th>User</th><th>Member</th><th>Role</th><th>State</th><th>Online</th><th>IP</th><th>Cost</th><th>Tunnels</th><th>Open</th></tr></thead><tbody>
        ${table||`<tr><td colspan=9 class="muted">no members</td></tr>`}
      </tbody></table></section>`;
  }).join("") || `<section class="panel"><div class="muted">你尚未加入任何团队。</div></section>`;

  // attach open handlers
  $$("#teamBody button[data-name]").forEach(b=>b.addEventListener("click",async ()=>{
    const name=b.dataset.name, member=b.dataset.member, kind=b.dataset.kind;
    b.disabled=true; b.classList.add("busy"); b.textContent="forwarding…";
    try{
      const r=await fetch("/api/tunnels/forward",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({name})});
      const j=await r.json().catch(()=>({}));
      if(!r.ok || !j.ok){ alert("自动 forward 失败："+(j.error||("HTTP "+r.status))); return; }
      // All kinds open in a NEW tab at the real local address: ttyd/headless
      // are plain http, web (teamx-ui) is https — use the URL exactly as the
      // server recorded it (self-signed cert warning once for https).
      window.open(j.url, "_blank");
      b.textContent="✓ "+kind;
    } catch(e){ alert("自动 forward 失败："+e); }
    finally{ b.disabled=false; b.classList.remove("busy"); }
  }));
  // 📁 File browser entry.
  $$("#teamBody button.fs-btn").forEach(b=>b.addEventListener("click",()=>{
    fsOpenByMember(b.dataset.mid, b.dataset.name||"member");
  }));
}

/* ---------- boot ---------- */

/* ---------- File browser overlay (backend: teamx fs via tunnel) ---------- */
let FS = { nodeId:"", name:"", path:"" };
const fsProxy=(suffix)=>"/api/cloud/"+encodeURIComponent(FS.nodeId)+suffix;
function fsFull(rel){
  if(!rel || rel==="/") return "";
  return rel.startsWith("/") ? rel.slice(1) : rel;
}
function fsCrumbText(){ return "/" + FS.path; }
async function fsOpenByMember(memberId, display){
  const tuns = window.__memTunnels.get(memberId) || [];
  const fs = tuns.find(t => t && t.name && t.name.startsWith("fs-"));
  if(!fs){ alert("该成员节点未运行 teamx fs（无 fs-* 隧道）"); return; }
  const r=await fetch("/api/tunnels/forward",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({name:fs.name})});
  const j=await r.json().catch(()=>({}));
  if(!r.ok || !j.ok){ alert("forward 失败："+(j.error||r.status)); return; }
  FS.nodeId=j.node_id; FS.name=fs.name; FS.path="";
  document.getElementById("fsOverlay").style.display="flex";
  document.getElementById("fsWho").textContent=display+" · "+fs.name;
  fsLoad("");
}
function fsCloseOverlay(){ document.getElementById("fsOverlay").style.display="none"; FS.nodeId=""; }
async function fsLoad(rel){
  FS.path = rel || "";
  document.getElementById("fsCrumb").innerHTML = fsCrumbText();
  const rows=document.getElementById("fsRows");
  rows.innerHTML=`<div class="loading">loading …</div>`;
  try{
    const r=await fetch(fsProxy("/api/list?path=")+encodeURIComponent(fsFull(rel)),{cache:"no-store"});
    const d=await r.json();
    if(!d.ok){ rows.innerHTML=`<div class="errorbox">${esc(d.error||"list failed")}</div>`; return; }
    renderFs(d.entries||[]);
  }catch(e){ rows.innerHTML=`<div class="errorbox">${esc(String(e))}</div>`; }
}
function renderFs(entries){
  const rows=document.getElementById("fsRows");
  if(FS.path){ rows.innerHTML=`<div class="fs-row" data-up="1"><span class="ic">📁</span><span class="nm">..</span></div>`; }
  else rows.innerHTML="";
  for(const e of entries){
    const isDir=e.type==="directory";
    const icon=isDir?"📁":"📄";
    const meta=isDir?"":fmtN(e.size)+"B";
    const div=document.createElement("div");
    div.className="fs-row";
    div.innerHTML=`<span class="ic">${icon}</span><span class="nm">${esc(e.name)}</span>`+
      (isDir?"":`<span class="fs-btn-actions"><span class="act dl" title="下载">⬇</span><span class="act pv" title="预览">👁</span></span>`) +
      `<span class="meta">${meta}</span>`;
    if(isDir){
      div.addEventListener("click",()=>fsLoad(joinPath(FS.path,e.name)));
    } else {
      div.addEventListener("dblclick",()=>fsPreview(joinPath(FS.path,e.name)));
      div.querySelector(".dl")?.addEventListener("click",(ev)=>{ev.stopPropagation(); fsDownload(joinPath(FS.path,e.name),e.name);});
      div.querySelector(".pv")?.addEventListener("click",(ev)=>{ev.stopPropagation(); fsPreview(joinPath(FS.path,e.name));});
    }
    rows.appendChild(div);
  }
  const up=rows.querySelector('[data-up="1"]');
  if(up) up.addEventListener("click",()=>{ const p=FS.path.split("/").filter(Boolean); p.pop(); fsLoad(p.join("/")); });
}
function joinPath(base,name){ return [base,name].filter(x=>x && x!=="/").join("/"); }
async function fsPreview(path){
  const pre=document.getElementById("fsPreview");
  const lower=path.toLowerCase();
  if(/\.(png|jpe?g|gif|webp|svg|bmp)$/.test(lower)){
    pre.innerHTML=`<img src="${fsProxy("/api/download?path=")}${encodeURIComponent(path)}">`;
    return;
  }
  pre.innerHTML=`<div class="loading">loading …</div>`;
  try{
    const r=await fetch(fsProxy("/api/read?path=")+encodeURIComponent(path),{cache:"no-store"});
    const d=await r.json();
    if(!d.ok){ pre.innerHTML=`<div class="errorbox">${esc(d.error||"read failed")}</div>`; return; }
    pre.innerHTML=`<pre>${esc(d.content)}${d.truncated?"\n… (截断)":""}</pre>`;
  }catch(e){ pre.innerHTML=`<div class="errorbox">${esc(String(e))}</div>`; }
}
function fsDownload(path,name){
  const a=document.createElement("a");
  a.href=fsProxy("/api/download?path=")+encodeURIComponent(path);
  if(name) a.download=name;
  document.body.appendChild(a); a.click(); a.remove();
}
function bindFsOverlay(){
  document.getElementById("fsClose").addEventListener("click",fsCloseOverlay);
  document.getElementById("fsRefresh").addEventListener("click",()=>fsLoad(FS.path));
  document.getElementById("fsFile").addEventListener("change",async (ev)=>{
    const f=ev.target.files[0]; ev.target.value="";
    if(!f) return;
    const url=fsProxy("/api/upload?dir=")+encodeURIComponent(fsFull(FS.path)||".")+"&name="+encodeURIComponent(f.name);
    const r=await fetch(url,{method:"POST",body:f});
    const d=await r.json().catch(()=>({}));
    alert(d.ok?("已上传 "+f.name):("上传失败 "+(d.error||r.status)));
    fsLoad(FS.path);
  });
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
  const d = p.data
  let evs = (d && Array.isArray(d.events)) ? d.events : (Array.isArray(d) ? d : [])
  // Newest first (the backend returns chronological order).
  evs = evs.slice(0, 80).reverse()
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

loadMe();
bindFsOverlay();
showView("teams");
"##;
