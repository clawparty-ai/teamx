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
pub fn fetch_panel(ctx: &Ctx, id: &str) -> RunOut {
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
            // Tasks are `taskx` docs in the workspace (project mode / local
            // git checkout). The CLI is authoritative; errors surface as-is.
            let res = match &ctx.session {
                Some(s) => local_cli(&["task", "list", "--mine", "--session", s]),
                None => local_cli(&["task", "list"]),
            };
            finish("teamx task list", false, res)
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
            if let Some(m) = members.iter().find(|m| m.get("id").and_then(Value::as_str) == Some(&id)) {
                return Some(json!({
                    "id": id,
                    "name": m.get("display_name").and_then(Value::as_str).unwrap_or(&id),
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
}
