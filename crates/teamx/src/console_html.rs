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
    <button data-view="discussions">Discussions</button>
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
      <ul class="ev-list" id="evList"></ul>
      <div class="ev-more-wrap"><button class="btn btn-mini" id="evMore" style="display:none">加载更早事件</button></div>
    </section>
  </div>
  <div class="view" id="view-discussions">
    <section class="panel">
      <div class="dsc-toolbar">
        <h2 style="margin:0">Discussions</h2>
        <button class="btn btn-mini" id="dscNew">+ 新建话题</button>
        <button class="btn btn-mini" id="dscBack" style="display:none">← 返回列表</button>
      </div>
      <div id="dscList"></div>
      <div id="dscThread" style="display:none"></div>
    </section>
  </div>
</div>
<div class="fs-overlay" id="dscOverlay" style="display:none">
  <div class="fs-dialog dsc-dialog">
    <div class="fs-head">
      <span class="fs-title" id="dscOvlTitle">参与讨论</span>
      <button class="btn btn-mini" id="dscOvlClose">✕</button>
    </div>
    <div class="dsc-ovl-body">
      <div id="dscOvlContext" class="muted"></div>
      <textarea id="dscOvlInput" placeholder="写下你的回复…"></textarea>
      <div class="dsc-ovl-actions">
        <button class="btn" id="dscOvlSend">发送</button>
        <button class="btn btn-mini" id="dscOvlCancel">取消</button>
      </div>
    </div>
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
.ev-text .md-render { font-size:13px; line-height:1.5; }
.ev-text .md-render p:first-child { margin-top:0; }
.ev-text .md-render p:last-child { margin-bottom:0; }
.ev-text .md-render pre { margin:0.4em 0; }
 .ev-more-wrap { text-align:center; padding:10px 0 2px; }
 .dsc-toolbar { display:flex; align-items:center; justify-content:space-between; gap:8px; margin-bottom:10px; flex-wrap:wrap; }
 .dsc-row { display:flex; align-items:center; gap:10px; padding:10px 8px; border-bottom:1px solid var(--border); cursor:pointer; border-radius:4px; }
 .dsc-row:hover { background:var(--muted); }
 .dsc-row.closed { opacity:.55; }
 .dsc-row-title { flex:1; font-weight:600; color:var(--fg); overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
 .dsc-row-meta { color:var(--muted-fg); font-size:12px; white-space:nowrap; }
 .dsc-row-join { flex-shrink:0; }
 .dsc-empty { padding:18px; text-align:center; color:var(--muted-fg); }
 .dsc-floor { padding:10px 8px; border-bottom:1px solid var(--border); }
 .dsc-floor-head { display:flex; align-items:center; gap:8px; font-size:12px; color:var(--muted-fg); margin-bottom:4px; flex-wrap:wrap; }
 .dsc-floor-actions { margin-left:auto; display:flex; gap:6px; }
 .dsc-thread-actions { display:flex; gap:6px; margin-left:auto; }
 .dsc-toolbar-sub { margin: -6px 0 8px; }
 .dsc-floor-no { font-weight:700; color:var(--primary); min-width:28px; }
 .dsc-floor-author { font-weight:600; color:var(--fg); }
  .dsc-floor-body { font-size:14px; color:var(--fg); word-break:break-word; }
 .dsc-quote { background:var(--muted); border-left:3px solid var(--border); border-radius:4px; padding:5px 8px; margin:4px 0; font-size:12.5px; color:var(--muted-fg); }
  .dsc-quote .q-who { font-weight:600; color:var(--fg); }
  .dsc-quote .md-render { font-size:12.5px; }
  .dsc-quote .md-render p { margin:0.2em 0; }
  .dsc-quote .md-render h1, .dsc-quote .md-render h2, .dsc-quote .md-render h3, .dsc-quote .md-render h4 { font-size:13px; margin:0.2em 0; }
  .dsc-quote .md-render ul { margin:0.2em 0; padding-left:1.2em; }
  .dsc-quote .md-render pre { margin:0.2em 0; }
 .dsc-composer { display:flex; gap:8px; margin-top:12px; align-items:flex-start; }
 .dsc-composer textarea { flex:1; min-height:96px; resize:vertical; font-family:inherit; font-size:13px; padding:8px 10px; line-height:1.5; }
 .dsc-composer .btn { margin-top:auto; }
 .dsc-dialog { width:min(680px, 94vw); }
 .dsc-ovl-body { display:flex; flex-direction:column; gap:10px; padding:16px; }
 .dsc-ovl-body textarea { min-height:220px; resize:vertical; font-family:inherit; font-size:14px; line-height:1.6; padding:10px 12px; }
 .dsc-ovl-actions { display:flex; gap:8px; justify-content:flex-end; }
 .dsc-replybar { font-size:12px; color:var(--primary); background:var(--muted); border-radius:4px; padding:3px 8px; margin-bottom:4px; display:inline-flex; gap:6px; align-items:center; }
 .dsc-form { display:flex; flex-direction:column; gap:8px; margin-top:12px; padding:12px; background:var(--muted); border-radius:6px; }
 .dsc-form input, .dsc-form textarea { font-family:inherit; font-size:13px; }
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
.fs-dialog { background:var(--card); border:1px solid var(--border); border-radius:var(--radius); box-shadow:var(--shadow-hover); width:min(1500px,98vw); height:min(920px,95vh); display:flex; flex-direction:column; overflow:hidden; }
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
.md-render { font-size:14px; line-height:1.6; } .md-render h1,.md-render h2,.md-render h3,.md-render h4 { margin:0.5em 0 0.3em; } .md-render p { margin:0.4em 0; } .md-render code { background:var(--muted); padding:1px 4px; border-radius:3px; } .md-render pre { background:var(--muted); padding:10px; border-radius:6px; overflow:auto; }
.fs-btn-actions { display:flex; gap:6px; align-items:center; }
.fs-btn-actions .act { font-size:12px; cursor:pointer; color:var(--muted-fg); }
.fs-btn-actions .act:hover { color:var(--primary); }
.fs-headbtns .btn-mini { font-size:16px; padding:4px 11px; }

/* overlay dialog inverts the page theme: light page → dark dialog, dark page → light dialog */
.fs-dialog { color:var(--fg); }
body:not(.dark) .fs-dialog { background:#22223A; color:#E0E0E0; border-color:#333355; }
body:not(.dark) .fs-dialog .fs-head { background:#2A2A40; color:#E0E0E0; border-bottom-color:#333355; }
body:not(.dark) .fs-dialog .fs-breadcrumb { background:#2A2A40; color:#7BB3F5; border-bottom-color:#333355; }
body:not(.dark) .fs-dialog .fs-left { border-right-color:#333355; }
body:not(.dark) .fs-dialog .fs-right { background:#22223A; }
body:not(.dark) .fs-dialog .fs-row { border-bottom-color:#333355; }
body:not(.dark) .fs-dialog .fs-row:hover { background:#333355; }
body:not(.dark) .fs-dialog .fs-row .meta { color:#909399; }
body:not(.dark) .fs-dialog pre { background:#1A1A2E; color:#E0E0E0; border-color:#333355; }
body:not(.dark) .fs-dialog .fs-preview-empty { color:#909399; }
.dark .fs-dialog { background:#FFFFFF; color:#333333; border-color:#EBEEF5; }
.dark .fs-dialog .fs-head { background:#F2F6FC; color:#333333; border-bottom-color:#EBEEF5; }
.dark .fs-dialog .fs-breadcrumb { background:#FFFFFF; color:#4A90E5; border-bottom-color:#EBEEF5; }
.dark .fs-dialog .fs-left { border-right-color:#EBEEF5; }
.dark .fs-dialog .fs-right { background:#FFFFFF; }
.dark .fs-dialog .fs-row { border-bottom-color:#EBEEF5; }
.dark .fs-dialog .fs-row:hover { background:#F2F6FC; }
.dark .fs-dialog .fs-row .meta { color:#909399; }
.dark .fs-dialog pre { background:#F2F6FC; color:#333333; border-color:#EBEEF5; }
.dark .fs-dialog .fs-preview-empty { color:#909399; }
a.btn { text-decoration:none; }
"#;

const JS: &str = r##"
const $=(s)=>document.querySelector(s);
const $$=(s)=>Array.from(document.querySelectorAll(s));
const esc=(s)=>String(s??"").replace(/[&<>"']/g,c=>({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[c]));
const badge=(s)=>{const b=s||"idle";return `<span class="badge ${esc(b)}">${esc(b)}</span>`};
const fmtN=(n)=>{ n=Number(n)||0; return n>=1e9?(n/1e9).toFixed(1)+"B": n>=1e6?(n/1e6).toFixed(1)+"M": n>=1e3?(n/1e3).toFixed(1)+"k": String(n); };
const fmt$=(c)=>{ c=Number(c)||0; return "$"+ (c>=1? c.toFixed(2): c.toFixed(4)); };
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
  else if(name==="discussions") loadDiscussions();
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
// Only the owner/admin console tunnels are privileged. Any other "web" tunnel
// (a shared web service, a member's own console, etc.) is a normal Web entry —
// labelling every non-ttyd/opencode tunnel "Admin" was misleading.
const isAdminName=(name)=>{const n=(name||"").toLowerCase();return n.includes("teamx-ui")||n.includes("admin");};
const openLabel=(kind,name)=>{
  if(kind==="ttyd") return {icon:"🖥", label:"TUI"};
  if(kind==="headless") return {icon:"🌐", label:"Web"};
  if(isAdminName(name)) return {icon:"🛡", label:"Admin"};
  return {icon:"🌐", label:"Web"};
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
        window.__memNames=window.__memNames||{}; window.__memNames[m.id]=m.display_name;
        // Admin-capable = owner/admin role or team-lead (owner/co-lead) OF THIS
        // TEAM only. t.my is per-team data the server attached (attach_my); we
        // must NOT gate on `teams.some(...)` (any team) or an async /api/me —
        // that would leak admin visibility across every team the viewer holds.
        const my = t.my || {};
        const canAdmin = (my.is_lead === true) || (my.role === "owner" || my.role === "admin") ||
            (t.my_is_lead === true) || (t.my_role === "owner" || t.my_role === "admin");
        // fs-* tunnels are served by the 📁 file browser, not opened as pages.
        // Only the privileged admin-console tunnels (teamx-ui*/admin*) are
        // gated to this team's lead; ordinary web services stay visible to all.
        const mtVisible=mt.filter(t=> t.name && !t.name.startsWith("fs-") && (!isAdminName(t.name) || canAdmin));
        const mtOpen=mtVisible;
        const act=m.activity||{};
        rows.push(`<tr>
          <td class="mono" style="font-size:11px;color:var(--muted-fg)">${m.user?esc(m.user.display_name):"—"}</td>
          <td>${esc(m.display_name)}</td>
          <td>${badge(m.role)}</td>
          <td>${badge(m.state)}</td>
          <td>${m.online?"<span class='badge running'>● online</span>":"<span class='badge idle'>○ offline</span>"}</td>
          <td class="mono" style="font-size:11px">${esc(m.ip||"—")}</td>
          <td class="mono" style="font-size:11px">${fmtN(act.tokens)} tok · ${fmt$(act.cost)}</td>
          <td>${mtVisible.length?mtVisible.map(x=>`<span class="tun-chip"><span class="dot"></span>${esc(kindOf(x.name))} · ${esc(x.name)}</span>`).join(""):'<span class="muted">—</span>'}</td>
          <td><button class="btn btn-mini fs-btn" data-mid="${esc(m.id)}" title="文件浏览器">📁</button> ${mtOpen.length?mtOpen.map(x=>{const oi=openLabel(kindOf(x.name),x.name); return `<button class="btn btn-mini" data-member="${esc(m.display_name)}" data-name="${esc(x.name)}" data-kind="${esc(kindOf(x.name))}">${oi.icon} ${esc(oi.label)}</button>`;}).join(" "):''}</td>
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
  $$("#teamBody button[data-kind]").forEach(b=>b.addEventListener("click",async ()=>{
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
    fsOpenByMember(b.dataset.mid, (window.__memNames||{})[b.dataset.mid]||"member");
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
  await new Promise(res=>setTimeout(res,600)); // let the forward settle
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
    const txt=await r.text();
    let d; try{ d=JSON.parse(txt); }catch(_){ rows.innerHTML=`<div class="errorbox">空/非法响应 (HTTP ${r.status}): ${esc(txt.slice(0,120))}</div>`; return; }
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
      (isDir?"":`<span class="fs-btn-actions"><span class="act dl" title="下载">⬇</span></span>`) +
      `<span class="meta">${meta}</span>`;
    if(isDir){
      div.addEventListener("click",()=>fsLoad(joinPath(FS.path,e.name)));
    } else {
      div.addEventListener("click",()=>fsPreview(joinPath(FS.path,e.name)));
      div.querySelector(".dl")?.addEventListener("click",(ev)=>{ev.stopPropagation(); fsDownload(joinPath(FS.path,e.name),e.name);});
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
    if(/\.(md|markdown)$/i.test(path)){
      pre.innerHTML=`<div class="md-render">${mdRender(d.content)}</div>`;
    } else {
      pre.innerHTML=`<pre>${esc(d.content)}${d.truncated?"\n… (截断)":""}</pre>`;
    }
  }catch(e){ pre.innerHTML=`<div class="errorbox">${esc(String(e))}</div>`; }
}
function fsDownload(path,name){
  const a=document.createElement("a");
  a.href=fsProxy("/api/download?path=")+encodeURIComponent(path);
  if(name) a.download=name;
  document.body.appendChild(a); a.click(); a.remove();
}
function mdRender(src){
  const esc2=(t)=>String(t??"").replace(/[&<>"]/g,c=>({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;"}[c]));
  let lines=(src||"").replace(/\r\n/g,"\n").split("\n");
  let out=[]; let inCode=false, code=[]; let inList=false;
  const inline=(t)=>esc2(t)
    .replace(/`([^`]+)`/g,"<code>$1</code>")
    .replace(/\*\*([^*]+)\*\*/g,"<b>$1</b>")
    .replace(/\*([^*]+)\*/g,"<i>$1</i>")
    .replace(/\[([^\]]+)\]\(([^)]+)\)/g,(m,text,url)=>{
      const u=String(url).trim();
      // Only allow safe schemes; anything else (javascript:/data:/vbscript:…) is
      // rendered as inert code text, never as an href (stored-XSS guard).
      if(/^https?:\/\//i.test(u) || /^mailto:/i.test(u)){
        return `<a href="${u}" target="_blank" rel="noopener noreferrer">${text}</a>`;
      }
      return `<code>${text} (${u})</code>`;
    });
  const flush=()=>{ if(inCode){ out.push("<pre><code>"+code.map(esc2).join("\n")+"</code></pre>"); code=[]; inCode=false; } };
  const closeList=()=>{ if(inList){ out.push("</ul>"); inList=false; } };
  for(const ln of lines){
    if(/^```/.test(ln)){ closeList(); if(inCode){flush();} else { inCode=true; } continue; }
    if(inCode){ code.push(ln); continue; }
    if(/^####\s+/.test(ln)){ closeList(); out.push("<h4>"+inline(ln.replace(/^####\s+/,""))+"</h4>"); }
    else if(/^###\s+/.test(ln)){ closeList(); out.push("<h3>"+inline(ln.replace(/^###\s+/,""))+"</h3>"); }
    else if(/^##\s+/.test(ln)){ closeList(); out.push("<h2>"+inline(ln.replace(/^##\s+/,""))+"</h2>"); }
    else if(/^#\s+/.test(ln)){ closeList(); out.push("<h1>"+inline(ln.replace(/^#\s+/,""))+"</h1>"); }
    else if(/^[-*]\s+/.test(ln)){ if(!inList){ out.push("<ul>"); inList=true; } out.push("<li>"+inline(ln.replace(/^[-*]\s+/,""))+"</li>"); }
    else if(/^\s*$/.test(ln)){ closeList(); out.push(""); }
    else { closeList(); out.push("<p>"+inline(ln)+"</p>"); }
  }
  flush(); closeList();
  return out.join("\n");
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
const EV_PAGE = 80;            // events per page
let evAll = [];                // all events, chronological ascending (as returned)
let evIdx = 0;                 // index of the oldest rendered event (next page goes further back)

function evRender(){
  // newest first: walk evAll backwards from the end, `EV_PAGE` items at a time.
  const end = evAll.length - evIdx;
  const start = Math.max(0, end - EV_PAGE);
  const page = evAll.slice(start, end).reverse();
  $("#evCount").textContent = evAll.length;
  $("#evMore").style.display = (start > 0) ? "" : "none";
  $("#evList").innerHTML = page.map(e=>{
    const kind=(e.type||e.kind||"").replace(/^[a-z]+\./,"");
    const who=e.member_id||""; const pl=e.payload||{};
    const text=summarize(e,pl);
    return `<li class="ev-item"><span class="ev-time">${esc(tf(e.created_at))}</span><span class="ev-kind">${badge(kind)}</span><div class="ev-text"><div class="md-render">${mdRender(text)}</div><span class="who">· ${esc((who).slice(0,12))}</span></div></li>`;
  }).join("") || `<li class="muted">暂无事件。</li>`;
}

async function loadEvents(){
  $("#evList").innerHTML=`<li class="loading">loading …</li>`;
  const p=await panel("events"); setLast(p);
  if(!p.ok){ $("#evList").innerHTML=""; const host=$("#view-events .panel"); host.insertAdjacentHTML("afterbegin",errBox(p)); return; }
  const d = p.data
  evAll = (d && Array.isArray(d.events)) ? d.events : (Array.isArray(d) ? d : [])
  evIdx = 0;
  evRender();
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
$("#evMore").addEventListener("click", ()=>{ evIdx += EV_PAGE; evRender(); });

/* ---------- Discussions (BBS-like threads) ---------- */
let curDsc = null;           // open discussion id
let curDscMeta = null;       // title/closed info for the open thread
let dscReplyTo = null;       // {message_id, text} when replying to a floor
let dscTimer = null;

async function dscAction(action, payload){
  const r = await fetch("/api/discussion/"+action, {
    method:"POST", headers:{"Content-Type":"application/json"},
    body: JSON.stringify(payload||{}), cache:"no-store",
  });
  let j=null; try{ j=await r.json(); }catch(_){ j=null; }
  if(!j||!j.ok) throw new Error((j&&j.error)||("discussion "+action+" failed"));
  return j;
}

async function loadDiscussions(){
  curDsc = null; dscReplyTo = null; stopDscPoll();
  $("#dscBack").style.display="none"; $("#dscThread").style.display="none"; $("#dscList").style.display="";
  $("#dscNew").style.display="";
  const p = await panel("discussions"); setLast(p);
  const host=$("#dscList");
  if(!p.ok){ host.innerHTML=""; $("#view-discussions .panel").insertAdjacentHTML("afterbegin",errBox(p)); return; }
  const arr = ((p.data||{}).discussions)||[];
  host.innerHTML = arr.map(d=>{
    const closed = d.closed===true;
    const icon = closed?"🔒 ":"💬 ";
    const last = d.last_active || d.created_at || "";
    const joinBtn = closed ? "" : `<button class="btn btn-mini dsc-row-join" data-id="${esc(d.discussion_id)}" data-title="${esc(d.title||"(无标题)")}">参与讨论</button>`;
    return `<div class="dsc-row${closed?" closed":""}" data-id="${esc(d.discussion_id)}">
      <span>${icon}</span><span class="dsc-row-title">${esc(d.title||"(无标题)")}</span>
      <span class="dsc-row-meta">${esc(d.author||"")} · ${esc(fmtN(d.messages||0))} 回复 · ${esc(tf(last))}</span>
      ${joinBtn}
    </div>`;
  }).join("") || `<div class="dsc-empty">暂无讨论，点击「新建话题」发起第一个讨论。</div>`;
  host.querySelectorAll(".dsc-row").forEach(row=>{
    const jb=row.querySelector(".dsc-row-join");
    if(jb) jb.addEventListener("click",(e)=>{ e.stopPropagation(); openDscOverlay({id:row.dataset.id, title:jb.dataset.title, mode:"thread"}); });
    row.addEventListener("click",()=>openDiscussion(row.dataset.id));
  });
}

async function openDiscussion(id){
  curDsc = id; dscReplyTo = null;
  $("#dscList").style.display="none"; $("#dscNew").style.display="none"; $("#dscBack").style.display="";
  $("#dscThread").style.display=""; $("#dscThread").innerHTML=`<div class="dsc-empty">加载中…</div>`;
  const p = await panel("discussion?id="+encodeURIComponent(id)); setLast(p);
  const host=$("#dscThread");
  if(!p.ok){ host.innerHTML=""; $("#view-discussions .panel").insertAdjacentHTML("afterbegin",errBox(p)); return; }
  const d = p.data||{};
  curDscMeta = d;
  renderThread(d);
  stopDscPoll(); startDscPoll();
}

function floorQuoteHtml(rt){
  if(!rt) return "";
  const who = rt.author ? ` · <span class="q-who">${esc(rt.author)}</span>` : "";
  const txt = rt.text || rt.body || "";
  return `<div class="dsc-quote">${who}<div class="md-render">${mdRender(txt)}</div></div>`;
}

function renderThread(d){
  const host=$("#dscThread");
  if(!host) return;
  const msgs = (d.messages||[]).slice().sort((a,b)=>String(b.created_at||"").localeCompare(String(a.created_at||"")));
  const closed = d.closed===true;
  const title = d.title||"(无标题)";
  const quotesHtml = (m)=>{
    let h="";
    if(m.reply_to && typeof m.reply_to==="object"){
      h += `<div class="dsc-quote"><span class="q-who">引用</span> ${esc(m.reply_to.message_id||"")}<div class="md-render">${mdRender(m.reply_to.text||m.reply_to.body||"")}</div></div>`;
    }
    const qs = Array.isArray(m.quotes)?m.quotes:[];
    qs.forEach(q=>{ h += `<div class="dsc-quote"><span class="q-who">引用 ${esc(q.kind||"")}</span><div class="md-render">${mdRender(q.text||"")}</div></div>`; });
    return h;
  };
  const floors = msgs.map((m,i)=>{
    // Messages arrive newest-first; number them chronologically so the oldest
    // floor is #1 (rendered at the bottom) and the newest gets the highest #.
    const no = msgs.length - i;
    const author = m.author||"";
    return `<div class="dsc-floor" data-mid="${esc(m.message_id)}" data-author="${esc(author)}" data-no="${no}">
      <div class="dsc-floor-head"><span class="dsc-floor-no">#${no}</span><span class="dsc-floor-author">${esc(author)}</span><span>${esc(tf(m.created_at))}</span>
        <span class="dsc-floor-actions">${closed?"":`
          <button class="btn btn-mini dsc-join-floor" data-act="join-floor" data-mid="${esc(m.message_id)}" data-author="${esc(author)}" data-no="${no}">参与讨论</button>
          <button class="btn btn-mini" data-act="quote">引用</button>`}
        </span>
      </div>
      ${quotesHtml(m)}
      <div class="dsc-floor-body md-render">${mdRender(m.body||"")}</div>
    </div>`;
  }).join("");
  const replyBar = dscReplyTo
    ? `<div class="dsc-replybar">引用回复 ${esc(dscReplyTo.mid)} <button class="btn btn-mini" id="dscClearReply">✕</button></div>` : "";
  host.innerHTML = `
    <div class="dsc-toolbar"><h3 style="margin:0">${esc(title)} ${closed?"🔒":""}</h3>
      ${closed?`<span class="muted">已关闭</span>`:`
        <div class="dsc-thread-actions">
          <button class="btn btn-mini" id="dscJoinThread">💬 参与讨论</button>
          <button class="btn btn-mini" id="dscCloseThread" title="关闭讨论">关闭讨论</button>
        </div>`}
    </div>
    <div class="dsc-toolbar-sub">${closed?`<span class="muted">该讨论已关闭，仅可浏览。</span>`:replyBar}</div>
    <div id="dscFloors">${floors||`<div class="dsc-empty">暂无消息。</div>`}</div>
    ${closed?"":`
    <div class="dsc-composer">
      <textarea id="dscInput" placeholder="写下你的回复…"></textarea>
      <button class="btn" id="dscSend">发送</button>
    </div>`}
  `;
  wireFloors(host);
  const back=$("#dscBack");
  back.onclick = ()=>loadDiscussions();
  const newBtn=$("#dscNew"); newBtn.onclick = ()=>showNewForm();
  const jt=$("#dscJoinThread"); if(jt) jt.onclick = ()=>openDscOverlay({ id:curDsc, title, mode:"thread" });
  const send=$("#dscSend"); if(send) send.onclick = async ()=>{
    const input=$("#dscInput");
    const text=(input?input.value:"").trim(); if(!text) return;
    try{
      if(dscReplyTo){ await dscAction("reply",{id:curDsc,message_id:dscReplyTo.mid,body:text}); }
      else { await dscAction("say",{id:curDsc,body:text}); }
      dscReplyTo=null; openDiscussion(curDsc);
    }catch(e){ alert(String(e.message||e)); }
  };
  const closeThread=$("#dscCloseThread"); if(closeThread) closeThread.onclick = async ()=>{
    if(!confirm("确认关闭该讨论？关闭后不可再发言。")) return;
    try{ await dscAction("close",{id:curDsc}); openDiscussion(curDsc); }
    catch(e){ alert(String(e.message||e)); }
  };
  const clearReply=$("#dscClearReply"); if(clearReply) clearReply.onclick = ()=>{ dscReplyTo=null; renderThread(curDscMeta); };
}

// Bind floor buttons (quote + join-floor) within a scope element. Used both for
// the initial full render and when a poll only refreshes #dscFloors.
function wireFloors(scope){
  if(!scope) return;
  scope.querySelectorAll("[data-act=quote]").forEach(btn=>{
    btn.addEventListener("click",()=>{
      const floor = btn.closest(".dsc-floor");
      const mid = floor.dataset.mid, author = floor.dataset.author, no = floor.dataset.no;
      const bodyEl = floor.querySelector(".dsc-floor-body");
      dscReplyTo = { mid, author, text:(bodyEl?bodyEl.textContent:"").slice(0,160) };
      // only refresh the floors region (do not disturb composer)
      if(curDscMeta) renderThreadFloors(curDscMeta);
    });
  });
  scope.querySelectorAll(".dsc-join-floor").forEach(btn=>{
    btn.addEventListener("click",()=>{
      openDscOverlay({ id:curDsc, title:(curDscMeta&&curDscMeta.title)||"", mode:"reply", mid:btn.dataset.mid, author:btn.dataset.author, no:btn.dataset.no });
    });
  });
}

// Render ONLY the floors list into #dscFloors (keeps composer/input intact).
function renderThreadFloors(d){
  const el=$("#dscFloors"); if(!el) return;
  const closed = (d&&d.closed)===true;
  const msgs = (d&&d.messages||[]).slice().sort((a,b)=>String(b.created_at||"").localeCompare(String(a.created_at||"")));
  const quotesHtml=(m)=>{ let h="";
    if(m.reply_to && typeof m.reply_to==="object"){
      h += `<div class="dsc-quote"><span class="q-who">引用</span> ${esc(m.reply_to.message_id||"")}<div class="md-render">${mdRender(m.reply_to.text||m.reply_to.body||"")}</div></div>`;
    }
    (Array.isArray(m.quotes)?m.quotes:[]).forEach(q=>{ h += `<div class="dsc-quote"><span class="q-who">引用 ${esc(q.kind||"")}</span><div class="md-render">${mdRender(q.text||"")}</div></div>`; });
    return h;
  };
  const floors=msgs.map((m,i)=>{
    const no=msgs.length-i, author=m.author||"";
    return `<div class="dsc-floor" data-mid="${esc(m.message_id)}" data-author="${esc(author)}" data-no="${no}">
      <div class="dsc-floor-head"><span class="dsc-floor-no">#${no}</span><span class="dsc-floor-author">${esc(author)}</span><span>${esc(tf(m.created_at))}</span>
        <span class="dsc-floor-actions">${closed?"":`
          <button class="btn btn-mini dsc-join-floor" data-act="join-floor" data-mid="${esc(m.message_id)}" data-author="${esc(author)}" data-no="${no}">参与讨论</button>
          <button class="btn btn-mini" data-act="quote">引用</button>`}
        </span>
      </div>
      ${quotesHtml(m)}
      <div class="dsc-floor-body md-render">${mdRender(m.body||"")}</div>
    </div>`;
  }).join("");
  el.innerHTML = floors||`<div class="dsc-empty">暂无消息。</div>`;
  wireFloors(el);
}

// Poll only when NEW messages arrive; otherwise leave the DOM untouched so the
// user's input / reading is never interrupted by a periodic refresh.
async function pollDiscussion(){
  if(!curDsc) return;
  const p=await panel("discussion?id="+encodeURIComponent(curDsc)).catch(()=>null);
  if(!p || !p.ok) return;
  const d=p.data||{};
  const oldCount=(curDscMeta&&curDscMeta.messages||[]).length;
  const newCount=(d.messages||[]).length;
  const wasClosed=(curDscMeta&&curDscMeta.closed)===true;
  const nowClosed=(d.closed)===true;
  if(newCount===oldCount && wasClosed===nowClosed) return; // nothing new -> keep DOM
  curDscMeta=d;
  // If the thread just closed, refresh the full page (toolbar + composer hide).
  if(nowClosed && !wasClosed){ openDiscussion(curDsc); return; }
  renderThreadFloors(d);
}

// Popup overlay composer for joining a discussion (thread-level or a specific
// floor). mode: "thread" -> plain say; "reply" -> reply to message_id.
function openDscOverlay(o){
  const ovl=$("#dscOverlay"); if(!ovl) return;
  const replying = o.mode==="reply" && o.mid;
  $("#dscOvlTitle").textContent = replying ? "回复讨论" : "参与讨论";
  $("#dscOvlContext").innerHTML = replying
    ? `回复 <b>#${esc(String(o.no||""))}</b> · ${esc(o.author||"")} 的发言`
    : (o.title ? `在 <b>${esc(o.title)}</b> 中发言` : "");
  const ta=$("#dscOvlInput"); ta.value="";
  ovl.style.display="flex";
  setTimeout(()=>ta.focus(),0);
  // ensure single-bind handlers
  const send=$("#dscOvlSend"), cancel=$("#dscOvlCancel"), close=$("#dscOvlClose");
  const closeAll=()=>{ ovl.style.display="none"; };
  const doSend=async ()=>{
    const text=ta.value.trim(); if(!text) return;
    try{
      if(replying){ await dscAction("reply",{id:o.id, message_id:o.mid, body:text}); }
      else { await dscAction("say",{id:o.id, body:text}); }
      closeAll();
      if(curDsc===o.id) openDiscussion(curDsc); else loadDiscussions();
    }catch(e){ alert(String(e.message||e)); }
  };
  // replace onclick each time (overlay singleton re-opened) — simple & safe.
  send.onclick = doSend;
  cancel.onclick = closeAll;
  close.onclick = closeAll;
  ta.onkeydown = (e)=>{ if(e.key==="Enter" && (e.ctrlKey||e.metaKey)){ e.preventDefault(); doSend(); } };
}

function showNewForm(){
  $("#dscList").style.display="none"; $("#dscThread").style.display=""; $("#dscBack").style.display=""; $("#dscNew").style.display="none";
  const host=$("#dscThread");
  host.innerHTML = `<div class="dsc-form">
    <b>新建话题</b>
    <input id="dscNewTitle" placeholder="话题标题" />
    <textarea id="dscNewBody" placeholder="开场消息（可选）"></textarea>
    <div><button class="btn" id="dscNewSubmit">创建</button> <button class="btn btn-mini" id="dscNewCancel">取消</button></div>
  </div>`;
  $("#dscNewSubmit").onclick = async ()=>{
    const title=$("#dscNewTitle").value.trim(); if(!title){ alert("标题不能为空"); return; }
    const body=$("#dscNewBody").value.trim();
    try{ const r=await dscAction("create",{title,body}); openDiscussion(r.data.discussion_id || r.data.id); }
    catch(e){ alert(String(e.message||e)); }
  };
  $("#dscNewCancel").onclick = ()=>loadDiscussions();
}

// SSE is the primary driver for live updates; this 60s poll is only a fallback
// when SSE is unavailable/disconnected (network hiccups), so the page never
// goes permanently stale.
function startDscPoll(){ stopDscPoll(); dscTimer=setInterval(()=>{ if(curDsc) pollDiscussion(); },60000); }
function stopDscPoll(){ if(dscTimer){ clearInterval(dscTimer); dscTimer=null; } }

// Entry buttons (nav switch handles rendering; also ensure back/new work after full reload).
$("#dscNew").addEventListener("click", showNewForm);
$("#dscBack").addEventListener("click", loadDiscussions);

/* ---------- SSE live events (Discussion + Events) ---------- */
let sseConn = null;
function handleStreamEvent(j){
  if(!j || !j.type) return;
  // Discussion events: refresh list or current thread (only when a new message
  // actually arrived, so the input/composer is never disturbed).
  if(j.type==="discussion.created" || j.type==="discussion.message" || j.type==="discussion.closed"){
    if(currentView==="discussions"){
      if(curDsc) pollDiscussion();
      else loadDiscussions();
    }
  }
  // Any other notable event: refresh Events view if it is visible.
  else if(currentView==="events"){
    loadEvents();
  }
}
function connectStream(){
  if(!window.EventSource) return;
  if(sseConn) sseConn.close();
  sseConn = new EventSource("/api/stream");
  sseConn.addEventListener("event",(e)=>{ try{ handleStreamEvent(JSON.parse(e.data||"{}")); }catch(_){} });
  // EventSource auto-reconnects; nothing else needed here.
}

loadMe();
connectStream();
bindFsOverlay();
showView("teams");
"##;
