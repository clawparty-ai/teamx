//! console_data.rs — Teamx Console data layer.
//!
//! The console shows exactly what the `teamx` CLI sees from its workspace:
//!
//! - **network mode** (the workspace has a teamx server URL + an imported
//!   invitation letter): the same RPC calls the opencode plugin makes
//!   (`team.status`, `events`, `tunnel.list`), authenticated by the letter's
//!   client certificate (the server derives the member from the cert CN).
//! - **project mode** (no server URL): the same subcommands run as a local
//!   `teamx` child process against the project-scoped `.teamx` DB.
//!
//! The module also owns `.teamx/console.json` (cloud-node list), which is the
//! only file the console writes.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// How the workspace resolves team data.
#[derive(Clone)]
pub enum Mode {
    /// No server URL: run `teamx` child processes against the local DB.
    Local,
    /// Network mode: talk to the teamx server over mTLS RPC.
    Network(String),
}

/// Resolve the network mode from `--server-url` / `TEAMX_SERVER_URL` / the
/// workspace's imported letter (same discovery the plugin and CLI use).
pub fn resolve_mode(server_url_override: Option<&str>) -> Mode {
    let url = server_url_override
        .filter(|u| !u.is_empty())
        .map(str::to_string)
        .or_else(|| std::env::var("TEAMX_SERVER_URL").ok().filter(|u| !u.is_empty()))
        .or_else(crate::tunnel_client::discover_server_url);
    match url {
        Some(u) => Mode::Network(u),
        None => Mode::Local,
    }
}

/// Execution context captured once at launch.
#[derive(Clone)]
pub struct Ctx {
    pub mode: Mode,
    /// Optional explicit session (project mode). Fallback picks the single
    /// local member session in the workspace DB.
    pub session: Option<String>,
    /// Project-scoped teamx home (`<cwd>/.teamx`).
    pub home: PathBuf,
    /// Where `.teamx/console.json` lives (default `<home>/console.json`).
    pub config_path: PathBuf,
}

impl Ctx {
    pub fn session_or<'a>(&'a self, dft: &'a str) -> &'a str {
        self.session.as_deref().unwrap_or(dft)
    }
    pub fn server_url(&self) -> Option<&str> {
        match &self.mode {
            Mode::Network(u) => Some(u),
            Mode::Local => None,
        }
    }
}

/// Pick a default local session key when the workspace DB has exactly one
/// member. Returns None when there are several (caller should pass --session)
/// or when the DB is missing.
pub fn pick_local_session(home: &Path) -> Option<String> {
    let db_path = home.join("teamx.db");
    if !db_path.is_file() {
        return None;
    }
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY;
    let conn = rusqlite::Connection::open_with_flags(&db_path, flags).ok()?;
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT session_key FROM members
             WHERE state NOT IN ('left','denied') ORDER BY joined_at DESC LIMIT 5",
        )
        .ok()?;
    let keys: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .ok()?
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if keys.len() == 1 {
        Some(keys[0].clone())
    } else {
        None
    }
}

/// Result of one panel fetch, ready to serialize to the browser.
#[derive(Serialize)]
pub struct RunOut {
    pub ok: bool,
    /// "rpc" (network) or "cli" (project mode)
    pub mode: &'static str,
    /// Human command label (what the equivalent CLI invocation is)
    pub cmd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    pub elapsed_ms: u64,
}

fn fail(cmd: &str, error: String, mode: &'static str) -> RunOut {
    RunOut {
        ok: false,
        mode,
        cmd: cmd.to_string(),
        error: Some(error),
        data: None,
        elapsed_ms: 0,
    }
}

/// Strip a CLI/RPC `{"data": ...}` envelope when present.
fn unwrap(v: Value) -> Value {
    if let Some(d) = v.get("data") {
        if d.is_array() || d.is_object() {
            return d.clone();
        }
    }
    v
}

/// Run a `teamx` subcommand as a child process (project mode). The child
/// inherits the console's cwd + env, so it sees the same workspace state as
/// the user's own CLI invocations.
fn local_cli(args: &[&str]) -> Result<Value, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let mut cmd = Command::new(&exe);
    cmd.args(args).arg("--json").stdout(Stdio::piped()).stderr(Stdio::piped());
    let start = Instant::now();
    let child = cmd.spawn().map_err(|e| format!("spawn {}: {e}", exe.display()))?;
    let out = child
        .wait_with_output()
        .map_err(|e| format!("wait: {e}"))?;
    let _elapsed_ms = start.elapsed().as_millis() as u64;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let err = if err.is_empty() {
            format!("exit {}", out.status.code().unwrap_or(-1))
        } else {
            err
        };
        return Err(err);
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&text)
        .map(unwrap)
        .map_err(|_| format!("unparseable output: {}", text.chars().take(300).collect::<String>()))
}

/// Network-mode RPC (same transport the opencode plugin uses).
fn net_rpc(url: &str, method: &str, args: Value) -> Result<Value, String> {
    crate::tunnel_client::run_rpc(url, method, args).map(unwrap)
}

fn require_local_session(ctx: &Ctx) -> Result<String, String> {
    match &ctx.session {
        Some(s) => Ok(s.clone()),
        None => pick_local_session(&ctx.home).ok_or_else(|| {
            "no local session: pass `--session <key>` (workspace has zero or several local members)"
                .to_string()
        }),
    }
}

/// Fetch one panel's data. Never writes team state.
pub fn fetch_panel(ctx: &Ctx, id: &str, query_id: Option<&str>) -> RunOut {
    match id {
        "teams" => {
            let res = teams_status(ctx).map(|mut data| {
                attach_my(ctx, &mut data);
                data
            });
            finish("teamx team status", ctx.server_url().is_some(), res)
        }
        "events" => {
            let res = events_data(ctx);
            finish("teamx events", ctx.server_url().is_some(), res)
        }
        "tunnels" => {
            let res = match &ctx.mode {
                Mode::Network(url) => net_rpc(url, "tunnel.list", json!({})),
                Mode::Local => local_cli(&["tunnel", "list"]),
            };
            finish("teamx tunnel list", ctx.server_url().is_some(), res)
        }
        "tasks" => {
            // Network mode: server-backed taskx (`task.list` RPC). Project mode:
            // `taskx` docs in the workspace via the local CLI.
            let res = match &ctx.mode {
                Mode::Network(url) => net_rpc(url, "task.list", json!({ "mine": true })),
                Mode::Local => match &ctx.session {
                    Some(s) => local_cli(&["task", "list", "--mine", "--session", s]),
                    None => local_cli(&["task", "list"]),
                },
            };
            finish("teamx task list", ctx.server_url().is_some(), res)
        }
        "discussions" => {
            let res = discussions_data(ctx);
            finish("teamx discussion list", ctx.server_url().is_some(), res)
        }
        "discussion" => {
            let res = discussion_data(ctx, query_id);
            finish("teamx discussion open", ctx.server_url().is_some(), res)
        }
        "docs" => {
            let res = docs_data(ctx);
            finish("teamx docs", ctx.server_url().is_some(), res)
        }
        "doc" => {
            let res = doc_data(ctx, query_id);
            finish("teamx doc", ctx.server_url().is_some(), res)
        }
        // File browser (design docs/design/console-file-browser.md). Local-only
        // by design: the console runs inside the workspace, so it reads the
        // filesystem directly through the shared `fs::resolve_with` guard.
        "files" => {
            let res = files_data(ctx, query_id);
            finish("teamx fs list", ctx.server_url().is_some(), res)
        }
        "file" => {
            let res = file_data(ctx, query_id);
            finish("teamx fs read", ctx.server_url().is_some(), res)
        }
        other => RunOut {
            ok: false,
            mode: "cli",
            cmd: other.to_string(),
            error: Some(format!("unknown panel `{other}`")),
            data: None,
            elapsed_ms: 0,
        },
    }
}

/// `team.status` through the workspace's natural channel (RPC or local CLI).
fn teams_status(ctx: &Ctx) -> Result<Value, String> {
    match &ctx.mode {
        Mode::Network(url) => {
            let sess = ctx.session_or("console").to_string();
            net_rpc(url, "team.status", json!({ "session": sess }))
        }
        Mode::Local => {
            let sess = require_local_session(ctx)?;
            local_cli(&["team", "status", "--session", &sess])
        }
    }
}

/// `events` for the first team the member belongs to.
fn events_data(ctx: &Ctx) -> Result<Value, String> {
    let tid = team_id(ctx)?;
    match &ctx.mode {
        Mode::Network(url) => net_rpc(url, "events", json!({ "team": tid })),
        Mode::Local => local_cli(&["events", "--team", &tid]),
    }
}

/// Poll new team events after `after` seq for SSE streaming. Network mode talks
/// to the serve `/rpc events`; Local mode returns empty (no live channel).
/// Returns `(events_in_order, max_seq)` where events are the rows with seq > after.
pub fn poll_team_events(ctx: &Ctx, after: i64) -> Result<(Vec<Value>, i64), String> {
    let tid = team_id(ctx)?;
    let url = match &ctx.mode {
        Mode::Network(u) => u.clone(),
        Mode::Local => return Ok((Vec::new(), after)),
    };
    let data = net_rpc(&url, "events", json!({ "team": tid, "after": after }))?;
    let rows = data.get("events").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut max = after;
    for e in &rows {
        if let Some(s) = e.get("seq").and_then(Value::as_i64) {
            if s > max {
                max = s;
            }
        }
    }
    Ok((rows, max))
}

/// `discussion list` for the member's team. Includes CLOSED threads (shown with a
/// lock in the console) so history is visible, not only the active ones.
fn discussions_data(ctx: &Ctx) -> Result<Value, String> {
    match &ctx.mode {
        Mode::Network(url) => net_rpc(url, "discussion.list", json!({ "all": true })),
        Mode::Local => {
            let sess = require_local_session(ctx)?;
            local_cli(&["discussion", "list", "--all", "--session", &sess])
        }
    }
}

/// `discussion open <id>` — one thread's messages.
fn discussion_data(ctx: &Ctx, id: Option<&str>) -> Result<Value, String> {
    let id = id.ok_or_else(|| "discussion id is required".to_string())?;
    match &ctx.mode {
        Mode::Network(url) => net_rpc(url, "discussion.open", json!({ "id": id })),
        Mode::Local => {
            let sess = require_local_session(ctx)?;
            local_cli(&["discussion", "open", id, "--session", &sess])
        }
    }
}

// ---------------------------------------------------------------------------
// File panel (design docs/design/console-file-browser.md, DQ-0001…0006)
// ---------------------------------------------------------------------------

/// Workspace root the File panel browses: the directory holding `<cwd>/.teamx`.
/// DQ-0001 anchors the browse root to the session cwd — never the whole disk.
pub(crate) fn workspace_root(ctx: &Ctx) -> std::path::PathBuf {
    ctx.home
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| ctx.home.clone())
}

/// Path of `p` relative to `root` (`.` for the root itself).
fn rel_of(root: &std::path::Path, p: &std::path::Path) -> String {
    match p.strip_prefix(root) {
        Ok(r) if !r.as_os_str().is_empty() => r.to_string_lossy().replace('\\', "/"),
        _ => ".".to_string(),
    }
}

/// Parent path for the breadcrumb (`None` at the root).
fn parent_rel(rel: &str) -> Option<String> {
    if rel == "." || rel.is_empty() {
        return None;
    }
    match rel.rsplit_once('/') {
        Some((head, _)) if !head.is_empty() => Some(head.to_string()),
        _ => Some(".".to_string()),
    }
}

/// Repo scan bounds (DQ-0006): depth 3, a hard cap, and the noise excludes.
const MAX_REPOS: usize = 50;
const REPO_SCAN_DEPTH: usize = 3;
const REPO_EXCLUDES: [&str; 5] = ["node_modules", "target", "vendor", "dist", "build"];

/// Panel `files`: one directory listing + the repo index.
fn files_data(ctx: &Ctx, rel: Option<&str>) -> Result<Value, String> {
    let root = workspace_root(ctx);
    // `resolve_with` canonicalizes, so compare against a canonical root
    // (macOS `/var` → `/private/var` would otherwise break `rel_of`).
    let canon_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
    let rel = rel.filter(|r| !r.is_empty()).unwrap_or(".");
    let dir = crate::fs::resolve_with(&root, rel, crate::fs::PathPolicy::Console)?;
    if !dir.is_dir() {
        return Err(format!("`{rel}` is not a directory"));
    }
    let here = rel_of(&canon_root, &dir);
    // The directory may itself be a repo's `.git` (e.g. `repo-a/.git`) or live
    // inside `.teamx` at any depth.
    let in_git_dir = here == ".git" || here.ends_with("/.git");
    let in_teamx = here.split('/').any(|seg| seg == ".teamx");
    let mut entries: Vec<Value> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            // Private/credential entries are hidden EVERYWHERE (they are not
            // listed at all, not merely un-previewable); inside `.teamx` the
            // extra credential dirs go too, and inside `.git` the internals
            // collapse.
            if crate::fs::is_private(&name) {
                continue;
            }
            if in_teamx && crate::fs::is_teamx_denied(&name) {
                continue;
            }
            if in_git_dir && crate::fs::is_git_internal(&name) {
                continue;
            }
            let md = std::fs::metadata(&p).ok();
            let is_dir = md.as_ref().map(|m| m.is_dir()).unwrap_or(false);
            entries.push(json!({
                "name": name,
                "type": if is_dir { "directory" } else { "file" },
                "size": md.as_ref().map(|m| m.len()).unwrap_or(0),
                "modified": md.as_ref().and_then(|m| m.modified().ok()).map(|t| {
                    t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
                }).unwrap_or(0),
            }));
        }
    }
    entries.sort_by(|a, b| {
        let ad = a["type"].as_str() == Some("directory");
        let bd = b["type"].as_str() == Some("directory");
        bd.cmp(&ad).then(a["name"].as_str().cmp(&b["name"].as_str()))
    });
    let repos = repos_index(&canon_root);
    Ok(json!({
        "ok": true,
        "root": root.display().to_string(),
        "path": here,
        "parent": parent_rel(&here),
        "entries": entries,
        "repos": repos,
    }))
}

/// Index the git repos under `root` (depth ≤ 3, noise excluded, capped).
fn repos_index(root: &std::path::Path) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    scan_repos(root, root, 0, &mut out);
    // Newest commit first; directory mtime breaks ties (DQ-0006).
    out.sort_by(|a, b| {
        let at = a["head_time"].as_i64().unwrap_or(0).max(a["mtime"].as_i64().unwrap_or(0));
        let bt = b["head_time"].as_i64().unwrap_or(0).max(b["mtime"].as_i64().unwrap_or(0));
        bt.cmp(&at)
    });
    out.truncate(MAX_REPOS);
    out
}

fn scan_repos(root: &std::path::Path, dir: &std::path::Path, depth: usize, out: &mut Vec<Value>) {
    if depth > REPO_SCAN_DEPTH || out.len() >= MAX_REPOS {
        return;
    }
    if dir.join(".git").exists() {
        out.push(repo_entry(root, dir));
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if name.starts_with('.') || REPO_EXCLUDES.contains(&name.as_str()) {
            continue;
        }
        scan_repos(root, &p, depth + 1, out);
    }
}

fn repo_entry(root: &std::path::Path, dir: &std::path::Path) -> Value {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| rel_of(root, dir));
    let mtime = std::fs::metadata(dir)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    json!({
        "path": rel_of(root, dir),
        "name": name,
        "branch": git_branch(dir),
        "head_time": git_head_time(dir),
        "mtime": mtime,
    })
}

/// Resolve the real `.git` directory (it is a file for worktrees/submodules).
fn git_dir(repo: &std::path::Path) -> Option<std::path::PathBuf> {
    let dot = repo.join(".git");
    if dot.is_dir() {
        return Some(dot);
    }
    let txt = std::fs::read_to_string(&dot).ok()?;
    let p = txt.trim().strip_prefix("gitdir:")?.trim();
    let p = std::path::PathBuf::from(p);
    Some(if p.is_absolute() { p } else { repo.join(p) })
}

/// Current branch from `.git/HEAD` (zero-cost); a detached HEAD yields the
/// short sha. `None` when unreadable.
fn git_branch(repo: &std::path::Path) -> Option<String> {
    let head = std::fs::read_to_string(git_dir(repo)?.join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(r) = head.strip_prefix("ref: refs/heads/") {
        return Some(r.to_string());
    }
    if let Some(r) = head.strip_prefix("ref: ") {
        return Some(r.to_string());
    }
    // Detached: show a short sha.
    Some(head.chars().take(8).collect())
}

/// HEAD commit time via the git CLI (cheap fallback to `None` when git is
/// absent or the repo is empty).
fn git_head_time(repo: &std::path::Path) -> i64 {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["log", "-1", "--format=%ct"])
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .trim()
            .parse::<i64>()
            .unwrap_or(0),
        _ => 0,
    }
}

/// Preview caps (DQ-0004).
const MAX_PREVIEW_BYTES: usize = 1024 * 1024;
const MAX_PREVIEW_LINES: usize = 5000;
const MAX_INLINE_IMAGE_BYTES: u64 = 5 * 1024 * 1024;
/// PDFs are embedded via the native browser viewer; cap so a huge file does
/// not stall the panel (the raw endpoint would still serve it on download).
const MAX_INLINE_PDF_BYTES: u64 = 32 * 1024 * 1024;
const IMAGE_EXTS: [&str; 3] = ["png", "jpg", "jpeg"];
const GIF_EXT: &str = "gif";

/// Panel `file`: preview one file (text/markdown/image) or mark it for download.
fn file_data(ctx: &Ctx, rel: Option<&str>) -> Result<Value, String> {
    let root = workspace_root(ctx);
    let canon_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
    let rel = rel.filter(|r| !r.is_empty()).ok_or("file: `id` (path) is required")?;
    let file = crate::fs::resolve_with(&root, rel, crate::fs::PathPolicy::Console)?;
    if !file.is_file() {
        return Err(format!("`{rel}` is not a file"));
    }
    let here = rel_of(&canon_root, &file);
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let size = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
    let ext = file
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();

    // DQ-0003: credential-like files are metadata-only.
    if crate::fs::is_sensitive_name(&name) {
        return Ok(json!({
            "ok": true, "path": here, "name": name, "size": size,
            "kind": "sensitive",
            "note": "该文件可能是凭据/密钥，仅显示元数据，不提供预览。",
        }));
    }
    // SVG renders as markup in a browser → XSS surface; download only (DQ-0004).
    if ext == "svg" {
        return Ok(json!({
            "ok": true, "path": here, "name": name, "size": size,
            "kind": "download", "note": "SVG 不内联预览（避免 XSS），请下载查看。",
        }));
    }
    // PDFs render in the browser's NATIVE viewer through the raw `/api/file`
    // endpoint — no PDF.js, no extra bundle weight. The panel embeds an
    // <iframe> pointing at that endpoint.
    if ext == "pdf" {
        if size > MAX_INLINE_PDF_BYTES {
            return Ok(json!({
                "ok": true, "path": here, "name": name, "size": size,
                "kind": "download",
                "note": format!("PDF 超过 {} MiB，请下载查看。", MAX_INLINE_PDF_BYTES / 1024 / 1024),
            }));
        }
        return Ok(json!({
            "ok": true, "path": here, "name": name, "size": size,
            "kind": "pdf",
        }));
    }
    if IMAGE_EXTS.contains(&ext.as_str()) || ext == GIF_EXT {
        if size > MAX_INLINE_IMAGE_BYTES {
            return Ok(json!({
                "ok": true, "path": here, "name": name, "size": size,
                "kind": "download", "note": "图片超过 5 MiB，不内联预览。",
            }));
        }
        let bytes = std::fs::read(&file).map_err(|e| e.to_string())?;
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let mime = if ext == "png" {
            "image/png"
        } else if ext == GIF_EXT {
            "image/gif"
        } else {
            "image/jpeg"
        };
        return Ok(json!({
            "ok": true, "path": here, "name": name, "size": size,
            "kind": "image", "mime": mime,
            "data_uri": format!("data:{mime};base64,{b64}"),
        }));
    }

    // PDF: preview the matching extracted text (same stem) and offer the
    // original for download. PDFs are binary, so an inline preview is useless;
    // the useful thing is the plain-text extract produced by the KB pipeline.
    if ext == "pdf" {
        let matched = find_pdf_text(&root, &file);
        let (content, text_path, truncated) = match &matched {
            Some(tp) => {
                let raw = std::fs::read(tp).unwrap_or_default();
                let trunc_bytes = raw.len() > MAX_PREVIEW_BYTES;
                let slice = &raw[..raw.len().min(MAX_PREVIEW_BYTES)];
                let text = String::from_utf8_lossy(slice).to_string();
                let total_lines = text.lines().count();
                let trunc_lines = total_lines > MAX_PREVIEW_LINES;
                let content = if trunc_lines {
                    text.lines()
                        .take(MAX_PREVIEW_LINES)
                        .collect::<Vec<_>>()
                        .join("\n")
                } else {
                    text
                };
                (content, Some(rel_of(&canon_root, tp)), trunc_bytes || trunc_lines)
            }
            None => (String::new(), None, false),
        };
        return Ok(json!({
            "ok": true, "path": here, "name": name, "size": size,
            "kind": "pdf",
            "content": content,
            "text_path": text_path,
            "truncated": truncated,
            "download_path": here,
        }));
    }

    // Text-ish: read a bounded prefix and reject obvious binaries.
    let raw = std::fs::read(&file).map_err(|e| e.to_string())?;
    let truncated_bytes = raw.len() > MAX_PREVIEW_BYTES;
    let slice = &raw[..raw.len().min(MAX_PREVIEW_BYTES)];
    if slice.iter().take(8192).any(|b| *b == 0) {
        return Ok(json!({
            "ok": true, "path": here, "name": name, "size": size,
            "kind": "download", "note": "二进制文件，请下载查看。",
        }));
    }
    let text = String::from_utf8_lossy(slice).to_string();
    let total_lines = text.lines().count();
    let truncated_lines = total_lines > MAX_PREVIEW_LINES;
    let content = if truncated_lines {
        text.lines()
            .take(MAX_PREVIEW_LINES)
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        text
    };
    let kind = if ext == "md" || ext == "markdown" {
        "markdown"
    } else {
        "text"
    };
    Ok(json!({
        "ok": true, "path": here, "name": name, "size": size,
        "kind": kind,
        "content": content,
        "lines": total_lines.min(MAX_PREVIEW_LINES),
        "truncated": truncated_bytes || truncated_lines,
    }))
}

/// Locate the plain-text extract for a PDF: a same-stem `.txt` next to it, or
/// under the common pipeline output dirs (`docs/text/`, `papers/txt/`).
fn find_pdf_text(root: &std::path::Path, pdf: &std::path::Path) -> Option<std::path::PathBuf> {
    let stem = pdf.file_stem()?;
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(dir) = pdf.parent() {
        // Same directory, then walk up looking for the pipeline output dirs.
        candidates.push(dir.join(stem).with_extension("txt"));
        let mut anc = Some(dir);
        for _ in 0..5 {
            let Some(a) = anc else { break };
            candidates.push(a.join("txt").join(stem).with_extension("txt"));
            candidates.push(a.join("docs/text").join(stem).with_extension("txt"));
            anc = a.parent();
        }
    }
    candidates.push(root.join("docs/text").join(stem).with_extension("txt"));
    candidates.push(root.join("papers/txt").join(stem).with_extension("txt"));
    candidates.into_iter().find(|p| {
        p.is_file() && std::fs::metadata(p).map(|m| m.len() > 0).unwrap_or(false)
    })
}

/// `<key>/<id>.meta.json` + `<id>.md` instances. P0 is local-only (the console
/// runs in the workspace); remote members without a clone see an empty state.
fn docs_data(ctx: &Ctx) -> Result<Value, String> {
    let root = ctx.home.join("docs");
    let mut types: Vec<Value> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root.join("_spec")) {
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let Ok(txt) = std::fs::read_to_string(&p) else { continue };
            let Ok(v) = serde_json::from_str::<Value>(&txt) else { continue };
            let key = p.file_stem().and_then(|x| x.to_str()).unwrap_or("");
            types.push(normalize_spec(&v, key));
        }
    }
    if types.is_empty() {
        // Builtin taskx so the tab is never empty on teams without `## 文档`.
        let s = crate::doc_flow::builtin_taskx_spec();
        types.push(json!({
            "key": s.key, "title": s.title, "purpose": "内置任务类型",
            "states": s.states, "owner": s.owner, "transitions": [],
        }));
    }
    let mut docs: Vec<Value> = Vec::new();
    for t in &types {
        let key = t.get("key").and_then(|x| x.as_str()).unwrap_or("");
        let dir = root.join(key);
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let fname = p.file_name().and_then(|x| x.to_str()).unwrap_or("");
            let Some(id) = fname.strip_suffix(".meta.json") else { continue };
            let Ok(txt) = std::fs::read_to_string(&p) else { continue };
            let Ok(m) = serde_json::from_str::<Value>(&txt) else { continue };
            let title = std::fs::read_to_string(dir.join(format!("{id}.md")))
                .ok()
                .and_then(|b| first_heading(&b))
                .unwrap_or_else(|| id.to_string());
            docs.push(json!({
                "doc": m.get("doc").and_then(|x| x.as_str()).unwrap_or(key),
                "id": m.get("id").and_then(|x| x.as_str()).unwrap_or(id),
                "title": title,
                "state": m.get("state").and_then(|x| x.as_str()).unwrap_or(""),
                "owner": m.get("owner").and_then(|x| x.as_str()).unwrap_or(""),
                "updated_at": m.get("updated_at").and_then(|x| x.as_str()).unwrap_or(""),
            }));
        }
    }
    docs.sort_by(|a, b| {
        let ka = (
            a.get("doc").and_then(|x| x.as_str()).unwrap_or(""),
            a.get("updated_at").and_then(|x| x.as_str()).unwrap_or(""),
        );
        let kb = (
            b.get("doc").and_then(|x| x.as_str()).unwrap_or(""),
            b.get("updated_at").and_then(|x| x.as_str()).unwrap_or(""),
        );
        kb.cmp(&ka)
    });
    Ok(json!({ "ok": true, "types": types, "docs": docs }))
}

fn normalize_spec(v: &Value, fallback_key: &str) -> Value {
    let key = v
        .get("key")
        .or_else(|| v.get("doc"))
        .and_then(|x| x.as_str())
        .unwrap_or(fallback_key);
    let title = v
        .get("title")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(key);
    json!({
        "key": key,
        "title": title,
        "purpose": v.get("purpose").and_then(|x| x.as_str()).unwrap_or(""),
        "states": v.get("states").cloned().unwrap_or_else(|| json!([])),
        "owner": v.get("owner").and_then(|x| x.as_str()).unwrap_or(""),
        "transitions": v.get("transitions").cloned().unwrap_or_else(|| json!([])),
    })
}

/// First `# heading` line of a markdown doc, used as its list title.
fn first_heading(md: &str) -> Option<String> {
    for line in md.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("# ") {
            let r = rest.trim();
            if !r.is_empty() {
                return Some(r.to_string());
            }
        }
    }
    None
}

/// `doc` panel — one document's body + meta (scanned across `<key>/<id>.meta.json`).
fn doc_data(ctx: &Ctx, id: Option<&str>) -> Result<Value, String> {
    let id = id.ok_or_else(|| "doc id is required".to_string())?;
    let root = ctx.home.join("docs");
    let Ok(rd) = std::fs::read_dir(&root) else {
        return Err("no .teamx/docs directory in this workspace".to_string());
    };
    for e in rd.flatten() {
        let dir = e.path();
        if !dir.is_dir() {
            continue;
        }
        let meta = dir.join(format!("{id}.meta.json"));
        if !meta.exists() {
            continue;
        }
        let txt = std::fs::read_to_string(&meta).map_err(|e| e.to_string())?;
        let m: Value = serde_json::from_str(&txt).map_err(|e| e.to_string())?;
        let body = std::fs::read_to_string(dir.join(format!("{id}.md"))).unwrap_or_default();
        return Ok(json!({
            "ok": true,
            "doc": m.get("doc").cloned().unwrap_or(Value::Null),
            "id": m.get("id").cloned().unwrap_or_else(|| json!(id)),
            "state": m.get("state").cloned().unwrap_or(Value::Null),
            "owner": m.get("owner").cloned().unwrap_or(Value::Null),
            "created_at": m.get("created_at").cloned().unwrap_or(Value::Null),
            "updated_at": m.get("updated_at").cloned().unwrap_or(Value::Null),
            "history": m.get("history").cloned().unwrap_or_else(|| json!([])),
            "body": body,
        }));
    }
    Err(format!("document `{id}` not found under {}", root.display()))
}

/// Run a discussion write action (`create`/`say`/`reply`/`close`) from the
/// console. Returns the same JSON the CLI would (ok / discussion_id / seq …).
pub fn discussion_action(ctx: &Ctx, action: &str, body: &Value) -> Result<Value, String> {    match &ctx.mode {
        Mode::Network(url) => {
            let sess = ctx.session_or("console").to_string();
            match action {
                "create" => net_rpc(url, "discussion.create", json!({
                    "session": sess,
                    "title": body.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                    "body": body.get("body").and_then(|v| v.as_str()).unwrap_or(""),
                })),
                "say" => net_rpc(url, "discussion.say", json!({
                    "session": sess,
                    "id": body.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                    "body": body.get("body").and_then(|v| v.as_str()).unwrap_or(""),
                    "quotes": body.get("quotes").cloned().unwrap_or_else(|| json!([])),
                })),
                "reply" => net_rpc(url, "discussion.reply", json!({
                    "session": sess,
                    "id": body.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                    "message_id": body.get("message_id").and_then(|v| v.as_str()).unwrap_or(""),
                    "body": body.get("body").and_then(|v| v.as_str()).unwrap_or(""),
                })),
                "close" => {
                    let mut args = json!({
                        "session": sess,
                        "id": body.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                    });
                    if let Some(s) = body.get("summary").and_then(|v| v.as_str()) {
                        if !s.trim().is_empty() {
                            args["summary"] = json!(s);
                        }
                    }
                    net_rpc(url, "discussion.close", args)
                }
                _ => Err(format!("unknown discussion action `{action}`")),
            }
        }
        Mode::Local => {
            let sess = require_local_session(ctx)?;
            let cmd: Vec<String> = match action {
                "create" => vec![
                    "discussion".into(), "create".into(),
                    body.get("title").and_then(|v| v.as_str()).unwrap_or("").into(),
                    "--body".into(), body.get("body").and_then(|v| v.as_str()).unwrap_or("").into(),
                    "--session".into(), sess,
                ],
                "say" => vec![
                    "discussion".into(), "say".into(),
                    body.get("id").and_then(|v| v.as_str()).unwrap_or("").into(),
                    body.get("body").and_then(|v| v.as_str()).unwrap_or("").into(),
                    "--session".into(), sess,
                ],
                "reply" => vec![
                    "discussion".into(), "reply".into(),
                    body.get("id").and_then(|v| v.as_str()).unwrap_or("").into(),
                    body.get("message_id").and_then(|v| v.as_str()).unwrap_or("").into(),
                    body.get("body").and_then(|v| v.as_str()).unwrap_or("").into(),
                    "--session".into(), sess,
                ],
                "close" => {
                    let mut v: Vec<String> = vec![
                        "discussion".into(), "close".into(),
                        body.get("id").and_then(|v| v.as_str()).unwrap_or("").into(),
                    ];
                    if let Some(s) = body.get("summary").and_then(|v| v.as_str()) {
                        if !s.trim().is_empty() {
                            v.push("--summary".into());
                            v.push(s.into());
                        }
                    }
                    v.push("--session".into());
                    v.push(sess);
                    v
                }
                _ => return Err(format!("unknown discussion action `{action}`")),
            };
            let refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
            local_cli(&refs)
        }
    }
}

fn finish(cmd: &str, is_net: bool, res: Result<Value, String>) -> RunOut {
    let mode: &'static str = if is_net { "rpc" } else { "cli" };
    match res {
        Ok(data) => RunOut {
            ok: true,
            mode,
            cmd: cmd.to_string(),
            data: Some(data),
            error: None,
            elapsed_ms: 0,
        },
        Err(e) => fail(cmd, e, mode),
    }
}

/// The first team id from `team.status` (used to scope `events`).
/// The first team id from `team.status` (used to scope `events`).
fn team_id(ctx: &Ctx) -> Result<String, String> {
    let first = first_team(ctx)?;
    first
        .get("team")
        .and_then(|t| t.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| first.get("id").and_then(Value::as_str).map(str::to_string))
        .ok_or_else(|| "cannot determine team id from team.status".to_string())
}

fn team_status(ctx: &Ctx) -> Result<Value, String> {
    match &ctx.mode {
        Mode::Network(url) => {
            let sess = ctx.session_or("console").to_string();
            net_rpc(url, "team.status", json!({ "session": sess }))
        }
        Mode::Local => {
            let sess = require_local_session(ctx)?;
            local_cli(&["team", "status", "--session", &sess])
        }
    }
}

fn first_team(ctx: &Ctx) -> Result<Value, String> {
    let status = team_status(ctx)?;
    let teams = status
        .get("teams")
        .and_then(Value::as_array)
        .ok_or_else(|| "team.status returned no `teams`".to_string())?;
    teams
        .first()
        .cloned()
        .ok_or_else(|| "you are not a member of any team yet".to_string())
}

// ---------------------------------------------------------------------------
// Current member ("me") resolution for the header.
// ---------------------------------------------------------------------------

/// The current member id: from the client certificate CN (network mode) or
/// the workspace's local member row (project mode).
fn self_member_id(ctx: &Ctx) -> Option<String> {
    match &ctx.mode {
        Mode::Network(url) => {
            let mtls = crate::tunnel_client::mtls_for(url)?;
            let (_, cert) =
                x509_parser::parse_x509_certificate(mtls.cert.as_ref()).ok()?;
            let cn = cert.subject().iter_common_name().next()?.as_str().ok()?.to_string();
            crate::pki::parse_member_cn(&cn).map(|(id, _)| id)
        }
        Mode::Local => {
            let sess = ctx.session.clone().or_else(|| pick_local_session(&ctx.home))?;
            local_member_id(&ctx.home, &sess)
        }
    }
}

/// Member id for a session in the workspace DB (project mode).
fn local_member_id(home: &Path, session: &str) -> Option<String> {
    let db_path = home.join("teamx.db");
    if !db_path.is_file() {
        return None;
    }
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY;
    let conn = rusqlite::Connection::open_with_flags(&db_path, flags).ok()?;
    conn.query_row(
        "SELECT id FROM members WHERE session_key = ?1 AND state NOT IN ('left','denied')
         ORDER BY joined_at DESC LIMIT 1",
        [session],
        |r| r.get::<_, String>(0),
    )
    .ok()
}

/// Find `{ id, name, role, state, is_lead }` for the member `id` inside a
/// `team.status` document (first team that has them).
fn find_me_in(status: &Value, id: &str) -> Option<Value> {
    let teams = status.get("teams")?.as_array()?;
    for team in teams {
        if let Some(members) = team.get("members").and_then(Value::as_array) {
            if let Some(m) = members.iter().find(|m| m.get("id").and_then(Value::as_str) == Some(id)) {
                return Some(json!({
                    "id": id,
                    "name": m.get("display_name").and_then(Value::as_str).unwrap_or(id),
                    "role": m.get("role").and_then(Value::as_str).unwrap_or(""),
                    "state": m.get("state").and_then(Value::as_str).unwrap_or(""),
                    "is_lead": m.get("is_lead").and_then(Value::as_bool).unwrap_or(false),
                }));
            }
        }
    }
    None
}

/// Attach `my` ({role,state,is_lead,name}) to each team object of a
/// `team.status` result so the Teams view summary cards are populated.
fn attach_my(ctx: &Ctx, data: &mut Value) {
    let Some(id) = self_member_id(ctx) else { return };
    let Some(teams) = data.get("teams").cloned() else { return };
    let Some(arr) = teams.as_array() else { return };
    for (i, team) in arr.iter().enumerate() {
        let mine = team
            .get("members")
            .and_then(Value::as_array)
            .map(|members| members.iter().any(|m| m.get("id").and_then(Value::as_str) == Some(&id)))
            .unwrap_or(false);
        if mine {
            if let Some(me) = find_me_in(data, &id) {
                if let Some(obj) = data
                    .get_mut("teams")
                    .and_then(Value::as_array_mut)
                    .and_then(|t| t.get_mut(i))
                    .and_then(Value::as_object_mut)
                {
                    obj.insert("my".to_string(), me);
                }
            }
        }
    }
}

/// Resolve `{ id, name, role, state, is_lead }` for the current member.
fn resolve_me(ctx: &Ctx) -> Result<Value, String> {
    let id = self_member_id(ctx).ok_or_else(|| {
        "cannot identify the current member (no invitation letter / local session)".to_string()
    })?;
    let status = team_status(ctx)?;
    find_me_in(&status, &id).ok_or_else(|| {
        "current member was not found in any team's member list (yet)".to_string()
    })
}

/// Fetch "me" as a panel-style result (used by `/api/me`).
pub fn fetch_me(ctx: &Ctx) -> RunOut {
    match resolve_me(ctx) {
        Ok(data) => RunOut {
            ok: true,
            mode: "rpc",
            cmd: "teamx whoami".to_string(),
            data: Some(data),
            error: None,
            elapsed_ms: 0,
        },
        Err(e) => RunOut {
            ok: false,
            mode: "rpc",
            cmd: "teamx whoami".to_string(),
            error: Some(e),
            data: None,
            elapsed_ms: 0,
        },
    }
}

// ---------------------------------------------------------------------------
// Cloud-node list (.teamx/console.json) — the only write the console performs.
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CloudNode {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub url: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ConsoleConfig {
    pub version: i32,
    pub nodes: Vec<CloudNode>,
}

impl Default for CloudNode {
    fn default() -> Self {
        CloudNode {
            id: String::new(),
            name: String::new(),
            kind: "headless".to_string(),
            url: String::new(),
        }
    }
}

pub fn config_path(home: &Path, override_p: &Option<PathBuf>) -> PathBuf {
    match override_p {
        Some(p) => p.clone(),
        None => home.join("console.json"),
    }
}

pub fn load_config(path: &Path) -> ConsoleConfig {
    match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => ConsoleConfig::default(),
    }
}

pub fn save_config(path: &Path, cfg: &ConsoleConfig) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.is_dir() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
    }
    let s = serde_json::to_string_pretty(cfg).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(path, s).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Cloud-node URLs must point at loopback only (the console proxies them into
/// iframes, so we must not become an SSRF gadget for a page visitor).
pub fn valid_node_url(url: &str) -> bool {
    let trimmed = url.trim();
    let rest = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"));
    let rest = match rest {
        Some(r) => r,
        None => return false,
    };
    let host = rest.split('/').next().unwrap_or("");
    let host = host.split(':').next().unwrap_or("");
    host == "127.0.0.1" || host.eq_ignore_ascii_case("localhost")
}

/// Normalize a display name into a stable node id (used on POST /api/cloud).
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("node");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwrap_envelope_takes_data() {
        let v = json!({ "ok": true, "data": { "tunnels": [] } });
        assert_eq!(unwrap(v), json!({ "tunnels": [] }));
    }

    #[test]
    fn unwrap_passthrough_without_envelope() {
        let v = json!({ "teams": [] });
        assert_eq!(unwrap(v), json!({ "teams": [] }));
    }

    #[test]
    fn cloud_url_must_be_loopback() {
        assert!(valid_node_url("http://127.0.0.1:19034"));
        assert!(valid_node_url("http://localhost:19033/"));
        assert!(!valid_node_url("https://95.40.221.159:443"));
        assert!(!valid_node_url("ftp://127.0.0.1:21"));
        assert!(!valid_node_url("http://10.0.0.5:80"));
    }

    #[test]
    fn slugify_names() {
        assert_eq!(slugify("caicai · headless"), "caicai-headless");
        assert_eq!(slugify("Tokyo ttyd"), "tokyo-ttyd");
        assert_eq!(slugify("!!"), "node");
    }

    #[test]
    fn docs_data_reads_specs_and_instances() {
        let root = std::env::temp_dir().join(format!("teamx-console-docs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join(".teamx");
        std::fs::create_dir_all(home.join("docs/_spec")).unwrap();
        std::fs::create_dir_all(home.join("docs/design")).unwrap();
        std::fs::write(
            home.join("docs/_spec/design.json"),
            r#"{"key":"design","title":"方案","purpose":"purpose","states":["draft","review","done"],"owner":"tech-lead"}"#,
        )
        .unwrap();
        std::fs::write(home.join("docs/design/001-x.md"), "# 我的方案\n\n正文").unwrap();
        std::fs::write(
            home.join("docs/design/001-x.meta.json"),
            r#"{"doc":"design","id":"001-x","state":"review","owner":"caishu","updated_at":"2026-09-13 07:50","history":[{"state":"draft","by":"caishu","at":"2026-09-13 07:40","event_seq":1}]}"#,
        )
        .unwrap();
        let ctx = Ctx {
            mode: Mode::Local,
            session: None,
            home: home.clone(),
            config_path: root.join("console.json"),
        };
        let v = docs_data(&ctx).unwrap();
        let types = v["types"].as_array().unwrap();
        assert_eq!(types.len(), 1);
        assert_eq!(types[0]["key"], json!("design"));
        assert_eq!(types[0]["states"], json!(["draft", "review", "done"]));
        let docs = v["docs"].as_array().unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0]["id"], json!("001-x"));
        assert_eq!(docs[0]["title"], json!("我的方案"));
        assert_eq!(docs[0]["state"], json!("review"));

        let one = doc_data(&ctx, Some("001-x")).unwrap();
        assert_eq!(one["body"], json!("# 我的方案\n\n正文"));
        assert_eq!(one["history"].as_array().unwrap().len(), 1);

        // No workspace docs -> builtin taskx fallback type.
        let empty = Ctx {
            mode: Mode::Local,
            session: None,
            home: root.join("nope"),
            config_path: root.join("console.json"),
        };
        let ev = docs_data(&empty).unwrap();
        assert_eq!(ev["types"][0]["key"], json!("taskx"));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The File panel workspace fixture: a repo, a sensitive file and a
    /// `.teamx` whose credentials must stay invisible while its documents stay
    /// browsable.
    fn file_fixture(tag: &str) -> (std::path::PathBuf, Ctx) {
        let root = std::env::temp_dir().join(format!("teamx-console-files-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join(".teamx");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("instance.json"), "{}").unwrap();
        std::fs::write(home.join("TEAM.md"), "# Team").unwrap();
        std::fs::create_dir_all(home.join("docs/design")).unwrap();
        std::fs::write(home.join("docs/design/001.md"), "# plan").unwrap();
        // Credential material that must never surface.
        std::fs::create_dir_all(home.join("ca")).unwrap();
        std::fs::write(home.join("ca/ca.key"), "PRIVATE").unwrap();
        std::fs::create_dir_all(home.join("letters/x")).unwrap();
        std::fs::write(home.join("letters/x/client.key"), "PRIVATE").unwrap();
        std::fs::create_dir_all(home.join("members/weiye")).unwrap();
        std::fs::write(home.join("members/weiye/invitation.letter"), "PRIVATE").unwrap();
        std::fs::write(home.join("teamx.db"), "SQLITE").unwrap();
        std::fs::write(home.join("console-server.json"), "{}").unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("README.md"), "# hi\nsecond line").unwrap();
        std::fs::write(root.join("server.key"), "PRIVATE").unwrap();
        std::fs::write(root.join(".npmrc"), "//registry:_authToken=x").unwrap();
        std::fs::create_dir_all(root.join(".ssh")).unwrap();
        std::fs::write(root.join(".ssh/id_ed25519"), "PRIVATE").unwrap();
        std::fs::write(root.join("logo.svg"), "<svg/>").unwrap();
        std::fs::create_dir_all(root.join("repo-a/.git/refs/heads")).unwrap();
        std::fs::write(root.join("repo-a/.git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(root.join("repo-a/.git/COMMIT_EDITMSG"), "init\n").unwrap();
        std::fs::create_dir_all(root.join("repo-a/.git/objects")).unwrap();
        std::fs::write(root.join("repo-a/.git/objects/x"), "obj").unwrap();
        std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        let ctx = Ctx {
            mode: Mode::Local,
            session: None,
            home: home.clone(),
            config_path: root.join("console.json"),
        };
        (root, ctx)
    }

    #[test]
    fn files_data_lists_root_and_hides_private_entries() {
        let (root, ctx) = file_fixture("list");
        let v = files_data(&ctx, None).unwrap();
        let entries = v["entries"].as_array().unwrap();
        let names: Vec<&str> = entries.iter().filter_map(|e| e["name"].as_str()).collect();
        assert!(names.contains(&"src"), "{names:?}");
        assert!(names.contains(&"README.md"), "{names:?}");
        // `.teamx` is browsable now (documents live there).
        assert!(names.contains(&".teamx"), "`.teamx` must be listed: {names:?}");
        // Private material is NOT listed at all (not merely un-previewable).
        assert!(!names.contains(&"server.key"), "`server.key` must be hidden: {names:?}");
        assert!(!names.contains(&".npmrc"), "`.npmrc` must be hidden: {names:?}");
        assert_eq!(v["path"], json!("."));
        assert!(v["parent"].is_null());
        // Directories sort before files.
        assert_eq!(entries[0]["type"], json!("directory"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn files_data_shows_teamx_documents_but_hides_credentials() {
        let (root, ctx) = file_fixture("teamx");
        let v = files_data(&ctx, Some(".teamx")).unwrap();
        let names: Vec<&str> = v["entries"].as_array().unwrap().iter().filter_map(|e| e["name"].as_str()).collect();
        // Documents + config stay reachable.
        assert!(names.contains(&"docs"), "{names:?}");
        assert!(names.contains(&"TEAM.md"), "{names:?}");
        assert!(names.contains(&"instance.json"), "{names:?}");
        // Credential material never appears.
        for denied in ["ca", "letters", "members", "teamx.db", "console-server.json"] {
            assert!(!names.contains(&denied), "`{denied}` must be hidden: {names:?}");
        }
        // Documents are readable…
        let doc = file_data(&ctx, Some(".teamx/docs/design/001.md")).unwrap();
        assert_eq!(doc["kind"], json!("markdown"));
        assert_eq!(doc["content"].as_str().unwrap().trim(), "# plan");
        let team = file_data(&ctx, Some(".teamx/TEAM.md")).unwrap();
        assert_eq!(team["kind"], json!("markdown"));
        // …while credential paths are refused (any depth).
        for p in [
            ".teamx/ca/ca.key",
            ".teamx/letters/x/client.key",
            ".teamx/members/weiye/invitation.letter",
            ".teamx/teamx.db",
            ".teamx/console-server.json",
        ] {
            assert!(file_data(&ctx, Some(p)).is_err(), "`{p}` must be refused");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn files_data_collapses_git_internals_but_keeps_head() {
        let (root, ctx) = file_fixture("git");
        let v = files_data(&ctx, Some("repo-a/.git")).unwrap();
        let names: Vec<&str> = v["entries"].as_array().unwrap().iter().filter_map(|e| e["name"].as_str()).collect();
        assert!(names.contains(&"HEAD"), "{names:?}");
        assert!(names.contains(&"COMMIT_EDITMSG"), "{names:?}");
        assert!(!names.contains(&"objects"), "`.git/objects` must be collapsed: {names:?}");
        // Reading an internal is refused outright.
        assert!(files_data(&ctx, Some("repo-a/.git/objects")).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn file_data_previews_text_and_refuses_sensitive_and_svg() {
        let (root, ctx) = file_fixture("read");
        let ok = file_data(&ctx, Some("README.md")).unwrap();
        assert_eq!(ok["kind"], json!("markdown"));
        assert_eq!(ok["content"], json!("# hi\nsecond line"));

        let txt = file_data(&ctx, Some("src")).unwrap_err();
        assert!(txt.contains("not a file"), "{txt}");

        // Private files are refused outright now (hidden from listings too).
        assert!(file_data(&ctx, Some("server.key")).is_err());
        assert!(file_data(&ctx, Some(".npmrc")).is_err());

        let svg = file_data(&ctx, Some("logo.svg")).unwrap();
        assert_eq!(svg["kind"], json!("download"));

        // Traversal is refused by the shared guard.
        assert!(file_data(&ctx, Some("../etc/passwd")).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn repos_index_finds_repos_reports_branch_and_skips_noise() {
        let (root, ctx) = file_fixture("repos");
        let _ = ctx;
        let repos = repos_index(&root);
        let a = repos.iter().find(|r| r["name"] == json!("repo-a")).expect("repo-a indexed");
        assert_eq!(a["path"], json!("repo-a"));
        assert_eq!(a["branch"], json!("main"));
        // node_modules is excluded from the scan.
        assert!(!repos.iter().any(|r| r["path"] == json!("node_modules/pkg")));
        let _ = std::fs::remove_dir_all(&root);
    }
}
