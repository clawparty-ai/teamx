//! `teamx local-team` — one-command launch of a TEAM.md as a local
//! single-machine multi-agent working set.
//!
//! Reuses:
//!   - teamfile parser (`.teamx/TEAM.md` member/role/duties/skills)
//!   - agents spawn/list/stop (headless `opencode serve` registry)
//! New in this module: per-member workspace + persona (AGENTS.md) + optional
//! shared-repo clone + optional ttyd web TUI per member.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::cli::LocalTeamCmd;
use crate::teamfile;

/// Members who should NOT be auto-launched by default (acted by a human / main
/// session). Owner runs the team from their own opencode; not a headless worker.
fn is_auto_member(role: Option<&str>) -> bool {
    !matches!(role, Some("owner") | Some("Owner"))
}

/// Resolve team file path: explicit > <cwd>/.teamx/TEAM.md.
fn resolve_team_file(explicit: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let p = cwd.join(".teamx").join("TEAM.md");
    if p.is_file() {
        Ok(p)
    } else {
        Err(format!("TEAM.md not found at {}", p.display()))
    }
}

fn member_base_dir() -> PathBuf {
    // Place members under <cwd>/.teamx/local-team/<key> (kept out of the shared
    // repo tree; .teamx is already gitignored at the workspace level).
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(".teamx").join("local-team")
}

/// Write the member's persona so a headless opencode knows who it is.
fn write_persona(dir: &Path, m: &teamfile::MemberProfile, tf: &teamfile::TeamFile, goals: &[String]) -> Result<(), String> {
    let mut s = String::new();
    s.push_str(&format!("# 你是 {}\n\n", m.display_name));
    if let Some(role) = &m.role {
        s.push_str(&format!("- 团队角色：{role}\n"));
    }
    if let Some(desc) = &m.description {
        s.push_str(&format!("- 职责分工：{desc}\n"));
    }
    if !m.skills.is_empty() {
        s.push_str(&format!("- 技能：{}\n", m.skills.join("、")));
    }
    if !m.outputs.is_empty() {
        s.push_str(&format!("- 产出：{}\n", m.outputs.join("、")));
    }
    if let Some(bg) = &tf.background {
        if !bg.is_empty() {
            s.push_str(&format!("\n## 团队背景\n{bg}\n"));
        }
    }
    if !goals.is_empty() {
        s.push_str("\n## 团队目标\n");
        for g in goals {
            s.push_str(&format!("- {g}\n"));
        }
    }
    s.push_str("\n## 工作方式\n- 主动推进任务并在完成时汇报；如需人类判断请明确说明。\n");
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("AGENTS.md"), s).map_err(|e| e.to_string())?;
    Ok(())
}

/// Clone the shared repo into the member dir (optional).
fn clone_repo_into(dir: &Path, repo: &str, server: Option<&str>, team_id: Option<&str>) -> Result<Option<PathBuf>, String> {
    let url: Option<String> = match server {
        Some(u) if !u.is_empty() => Some(u.to_string()),
        _ => std::env::var("TEAMX_SERVER_URL").ok().filter(|u| !u.is_empty()),
    };
    let Some(url) = url else { return Ok(None) }; // no server configured -> skip quietly
    let target = dir.join(repo);
    if target.exists() {
        return Ok(Some(target));
    }
    let _ = std::fs::create_dir_all(dir);
    match crate::git_client::clone(&url, repo, Some(target.to_str().unwrap_or_default()), team_id) {
        Ok(_) => Ok(Some(target)),
        Err(e) => {
            eprintln!("teamx local-team: git clone skipped for member (err): {e}");
            Ok(None)
        }
    }
}

fn spawn_ttyd(dir: &Path, name: &str, port: u16, opencode_url: &str, opencode_bin: &str) -> Result<u16, String> {
    let shell = format!(
        "cd {} && exec {} attach {} -c",
        dir.display(),
        shell_quote(opencode_bin),
        shell_quote(opencode_url)
    );
    let log = dir.join(format!(".ttyd-{name}.log"));
    let logf = std::fs::File::create(&log).map_err(|e| e.to_string())?;
    let errf = logf.try_clone().map_err(|e| e.to_string())?;

    // Port is chosen by the caller (per-member, unique). Spawn and confirm the
    // child stays alive (a ttyd that lost the port exits quickly).
    let child = Command::new("ttyd")
        .args(["-p", &port.to_string(), "-W", "-T", "xterm-256color", "-t", "disableReconnect=true", "/bin/bash", "-lc", &shell])
        .stdout(Stdio::from(logf))
        .stderr(Stdio::from(errf))
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| format!("ttyd spawn failed (is ttyd installed?): {e}"))?;
    std::thread::sleep(std::time::Duration::from_millis(400));
    if is_alive(child.id()) {
        std::fs::write(dir.join(format!(".ttyd-{name}.pid")), child.id().to_string()).ok();
        std::fs::write(dir.join(format!(".ttyd-{name}.port")), port.to_string()).ok();
        Ok(port)
    } else {
        Err(format!("ttyd on {port} exited (port likely taken)"))
    }
}

/// Very small posix single-quote escape for shell fragments.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    { unsafe { libc::kill(pid as i32, 0) == 0 } }
    #[cfg(not(unix))]
    { let _ = pid; true }
}

// ---------------------------------------------------------------------------
// Server bootstrap + member activation (docs/52)
// ---------------------------------------------------------------------------

fn teamx_bin() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("teamx"))
}

/// The workspace `.teamx` dir (cwd based; also where local-team keeps state).
fn ws_teamx_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(".teamx")
}

fn team_db() -> PathBuf {
    ws_teamx_dir().join("teamx.db")
}

/// Run `teamx <args...>` as a child. HOME points at the .teamx used for letters
/// + CA; DB is the workspace authoritative DB (shared). Returns stdout.
fn run_teamx(args: &[&str], home: &Path) -> Result<String, String> {
    let mut cmd = std::process::Command::new(teamx_bin());
    cmd.args(args);
    cmd.env("TEAMX_HOME", home);
    cmd.env("TEAMX_DB", team_db());
    let out = cmd.output().map_err(|e| format!("failed to run {}: {e}", teamx_bin().display()))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let tail = stderr.trim().lines().last().unwrap_or("").to_string();
        return Err(format!("teamx {} failed: {tail}", args.join(" ")));
    }
    Ok(stdout)
}

/// Try to reach a server at `url` by TCP connect (host:port). We avoid mTLS
/// health checks here because a freshly bootstrapped serve has no client cert
/// for this workspace yet; a listening port is enough to decide reuse vs boot.
fn server_reachable(url: &str) -> bool {
    let hostport = url
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_end_matches('/')
        .to_string();
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().unwrap_or(0)),
        None => (hostport, if url.starts_with("https") { 443 } else { 80 }),
    };
    if port == 0 {
        return false;
    }
    use std::net::ToSocketAddrs;
    let addr = match (host.as_str(), port).to_socket_addrs() {
        Ok(mut it) => it.next(),
        Err(_) => None,
    };
    let Some(addr) = addr else { return false };
    std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(700)).is_ok()
}

/// Ensure a teamx serve is reachable from this workspace:
///   - if TEAMX_SERVER_URL points at a reachable server, join it;
///   - otherwise bootstrap a local `teamx serve` on a free port and return URL.
fn ensure_server(server_opt: Option<&str>) -> Result<String, String> {
    // 1. existing server (env or arg) reachable
    let candidate = server_opt
        .map(|s| s.trim_end_matches('/').to_string())
        .or_else(|| std::env::var("TEAMX_SERVER_URL").ok().map(|s| s.trim_end_matches('/').to_string()));
    if let Some(u) = candidate {
        if server_reachable(&u) {
            return Ok(u);
        }
        // If a URL was explicitly given but unreachable, fail loudly (don't
        // silently start a second server that the user did not ask for).
        if server_opt.is_some() {
            return Err(format!("server {u} is not reachable"));
        }
        // env only but not reachable -> fall through to bootstrap below
    }

    // 2. bootstrap local serve
    std::fs::create_dir_all(ws_teamx_dir()).map_err(|e| e.to_string())?;
    let mut port: u16 = 5781;
    for p in 5781..=5810 {
        if std::net::TcpListener::bind(("127.0.0.1", p)).is_ok() {
            port = p;
            break;
        }
    }
    let url = format!("https://127.0.0.1:{port}");
    let log = ws_teamx_dir().join("local-team").join("server.log");
    std::fs::create_dir_all(log.parent().unwrap()).ok();
    let logf = std::fs::File::create(&log).map_err(|e| e.to_string())?;
    let errf = logf.try_clone().map_err(|e| e.to_string())?;
    let mut cmd = std::process::Command::new(teamx_bin());
    cmd.args([
        "serve",
        "--addr",
        "127.0.0.1",
        "--port",
        &port.to_string(),
        "--db",
        team_db().to_str().unwrap_or(""),
    ]);
    cmd.env("TEAMX_HOME", ws_teamx_dir());
    cmd.env("TEAMX_DB", team_db());
    cmd.stdout(Stdio::from(logf));
    cmd.stderr(Stdio::from(errf));
    cmd.stdin(Stdio::null());
    let child = cmd.spawn().map_err(|e| format!("spawn serve: {e}"))?;
    std::fs::write(ws_teamx_dir().join("local-team").join("server.pid"), child.id().to_string()).ok();
    // wait for health
    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_millis(200));
        if server_reachable(&url) {
            return Ok(url);
        }
    }
    Err(format!("local serve did not become healthy on {url} (see {})", log.display()))
}

/// Ensure a team exists (owner session). If the workspace DB already has a team
/// we return its id; otherwise create from TEAM.md (bootstrap issues letters).
fn ensure_team(team_name: &str) -> Result<String, String> {
    // See whether a team already exists in this workspace DB.
    let list = run_teamx(&["team", "list", "--json", "--session", OWNER_SESSION], &ws_teamx_dir())?;
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&list) {
        if let Some(arr) = v.get("teams").and_then(|x| x.as_array()) {
            if let Some(first) = arr.first() {
                if let Some(id) = first.get("team_id").and_then(|x| x.as_str()) {
                    return Ok(id.to_string());
                }
            }
        }
    }
    // No team yet -> create (TEAM.md auto-bootstrap issues letters).
    let out = run_teamx(&["team", "create", team_name, "--json", "--session", OWNER_SESSION], &ws_teamx_dir())?;
    let v: serde_json::Value = serde_json::from_str(&out).map_err(|e| format!("parse create: {e}: {out}"))?;
    let tid = v
        .get("team")
        .and_then(|t| t.get("id"))
        .and_then(|x| x.as_str())
        .or_else(|| v.get("team_id").and_then(|x| x.as_str()))
        .ok_or_else(|| format!("could not parse team id from create output: {out}"))?;
    Ok(tid.to_string())
}
const OWNER_SESSION: &str = "local-owner";

/// Activate a member in the workspace team from its bootstrapped letter:
/// import into the member's workspace .teamx, then approve (auto). Returns
/// member_id on success; Ok(None) when no letter (owner / not bootstrapped).
fn activate_member(key: &str) -> Result<Option<String>, String> {
    let letter_file = ws_teamx_dir().join("members").join(key).join("invitation.letter");
    if !letter_file.is_file() {
        return Ok(None);
    }
    let letter = std::fs::read_to_string(&letter_file).map_err(|e| e.to_string())?;
    let member_home = member_base_dir().join(key).join(".teamx");
    std::fs::create_dir_all(&member_home).map_err(|e| e.to_string())?;

    let imp = run_teamx(
        &["team", "import", "--json", "--session", key, &letter],
        &member_home,
    )?;
    let mid = serde_json::from_str::<serde_json::Value>(&imp)
        .ok()
        .and_then(|v| v.get("member_id").and_then(|x| x.as_str()).map(|s| s.to_string()))
        .or_else(|| {
            // human output fallback: try a line like member_id:
            imp.lines()
                .find_map(|l| l.strip_prefix("member_id:").map(|s| s.trim().to_string()))
        });
    let Some(member_id) = mid else {
        // import could already be active (repeat up) or letter consumed; try approve by listing
        eprintln!("teamx local-team: import for {key} gave no member_id; assuming ok");
        return Ok(None);
    };
    // auto-approve (bootstrap letters are not auto-approve; approve now)
    let _ = run_teamx(&["team", "approve", "--json", "--session", OWNER_SESSION, &member_id], &ws_teamx_dir());
    Ok(Some(member_id))
}


pub fn run(cmd: &LocalTeamCmd) -> Result<serde_json::Value, String> {
    match cmd {
        LocalTeamCmd::Up {
            team_file, repo, server, only, port_base, ttyd_port_base, no_ttyd, no_git, no_persona, opencode, env,
        } => {
            let tf_path = resolve_team_file(team_file.as_deref())?;
            let (tf, warnings, _) = teamfile::load_and_validate_team_file(Some(&tf_path))?;
            for w in &warnings {
                eprintln!("teamx local-team: warning: {w}");
            }
            if tf.members.is_empty() {
                return Err("TEAM.md has no members".to_string());
            }
            let opencode_bin = opencode.clone().unwrap_or_else(|| "opencode".to_string());
            let only: Vec<String> = only
                .as_deref()
                .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
                .unwrap_or_default();
            let base = port_base.unwrap_or(4200);
            let ttyd_base = ttyd_port_base.unwrap_or(7680);
            let base_dir = member_base_dir();

            // ---- server bootstrap / join + team + member activation ----
            let server_url = ensure_server(server.as_deref())?;
            let team_id = ensure_team(if tf.team_name.is_empty() { "local-team" } else { &tf.team_name })?;
            eprintln!("teamx local-team: team {team_id} ready via {server_url}");

            let target_keys: Vec<String> = tf.members.iter().filter(|m| {
                if !only.is_empty() { only.contains(&m.key) } else { is_auto_member(m.role.as_deref()) }
            }).map(|m| m.key.clone()).collect();
            for k in &target_keys {
                match activate_member(k) {
                    Ok(Some(mid)) => eprintln!("teamx local-team: member {k} active ({mid})"),
                    Ok(None) => {}
                    Err(e) => eprintln!("teamx local-team: activate {k}: {e}"),
                }
            }

            let members = tf.members.iter().filter(|m| {
                if !only.is_empty() {
                    only.contains(&m.key)
                } else {
                    is_auto_member(m.role.as_deref())
                }
            });

            let mut launched = Vec::new();
            let mut idx: u16 = 0;
            let mut ttyd_idx: u16 = 0;
            for m in members {
                let dir = base_dir.join(&m.key);
                std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;

                if !*no_persona {
                    write_persona(&dir, m, &tf, &tf.goals)?;
                }

                let mut envs = env.clone();
                if !envs.iter().any(|e| e.starts_with("TEAMX_HOME=")) {
                    // Each member gets its own .teamx under its dir so identity /
                    // git settings can be per-member later via team import.
                    envs.push(format!("TEAMX_HOME={}", dir.join(".teamx").display()));
                }

                // optional shared repo clone
                let repo_dir = if !*no_git {
                    repo.as_deref()
                        .and_then(|r| clone_repo_into(&dir, r, server.as_deref(), None).ok().flatten())
                        .unwrap_or_else(|| {
                            // no repo / clone skipped -> persona-only workspace
                            dir.clone()
                        })
                } else {
                    dir.clone()
                };
                let _ = repo_dir;

                let port = base.saturating_add(idx);
                idx += 1;
                let spawned = crate::agents::spawn_agent(&m.key, Some(&dir), Some(port), Some(&opencode_bin), &envs, false)?;
                let actual_port = spawned.get("port").and_then(|p| p.as_u64()).map(|p| p as u16).unwrap_or(port);

                let mut ttyd_url: Option<String> = None;
                if !*no_ttyd {
                    let opencode_url = format!("http://127.0.0.1:{actual_port}");
                    // unique ttyd port per member (separate base from headless)
                    let tport = ttyd_base.saturating_add(ttyd_idx);
                    ttyd_idx += 1;
                    match spawn_ttyd(&dir, &m.key, tport, &opencode_url, &opencode_bin) {
                        Ok(p) => ttyd_url = Some(format!("http://127.0.0.1:{p}")),
                        Err(e) => eprintln!("teamx local-team: ttyd skipped for {}: {e}", m.key),
                    }
                }

                launched.push(serde_json::json!({
                    "name": m.key,
                    "key": m.key,
                    "display_name": m.display_name,
                    "role": m.role,
                    "dir": dir.display().to_string(),
                    "port": actual_port,
                    "status": "running",
                    "ttyd_url": ttyd_url,
                    "ttyd": ttyd_url,
                    "pid": spawned.get("pid"),
                }));
            }
            if launched.is_empty() {
                return Err("no members to launch (all are owners? use --only <key>) ".to_string());
            }
            Ok(serde_json::json!({ "ok": true, "base_dir": base_dir.display().to_string(), "members": launched }))
        }
        LocalTeamCmd::Ps => {
            let agents = crate::agents::list_agents(false)?;
            let members = agents
                .get("instances")
                .and_then(|a| a.as_array())
                .cloned()
                .unwrap_or_default();
            let out: Vec<serde_json::Value> = members
                .iter()
                .map(|a| {
                    let name = a.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    let dir = a.get("dir").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    let ttyd_pid = std::fs::read_to_string(Path::new(&dir).join(format!(".ttyd-{name}.pid")))
                        .ok()
                        .and_then(|s| s.trim().parse::<u32>().ok());
                    let ttyd_alive = ttyd_pid.map(is_alive).unwrap_or(false);
                    let ttyd_port = std::fs::read_to_string(Path::new(&dir).join(format!(".ttyd-{name}.port")))
                        .ok()
                        .and_then(|s| s.trim().parse::<u16>().ok());
                    let mut o = a.clone();
                    o["ttyd_running"] = serde_json::json!(ttyd_alive);
                    o["ttyd_url"] = ttyd_port.map(|p| serde_json::json!(format!("http://127.0.0.1:{p}"))).unwrap_or(serde_json::Value::Null);
                    o
                })
                .collect();
            Ok(serde_json::json!({ "ok": true, "members": out }))
        }
        LocalTeamCmd::Stop { name } => {
            let agents = crate::agents::list_agents(false)?;
            let instances = agents.get("instances").and_then(|a| a.as_array()).cloned().unwrap_or_default();
            let mut stopped = Vec::new();
            for a in instances {
                let aname = a.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
                if let Some(want) = name {
                    if want != &aname {
                        continue;
                    }
                }
                let dir = a.get("dir").and_then(|x| x.as_str()).unwrap_or("").to_string();
                // kill ttyd pid file
                let pidfile = Path::new(&dir).join(format!(".ttyd-{aname}.pid"));
                if let Ok(s) = std::fs::read_to_string(&pidfile) {
                    if let Ok(pid) = s.trim().parse::<u32>() {
                        #[cfg(unix)]
                        unsafe { libc::kill(pid as i32, libc::SIGTERM); }
                    }
                    let _ = std::fs::remove_file(&pidfile);
                }
                let _ = crate::agents::stop_agent(&aname, false)?;
                stopped.push(aname);
            }
            // stop the bootstrapped local serve if this workspace started one
            let server_pid_file = ws_teamx_dir().join("local-team").join("server.pid");
            if let Ok(s) = std::fs::read_to_string(&server_pid_file) {
                if let Ok(pid) = s.trim().parse::<u32>() {
                    if is_alive(pid) {
                        #[cfg(unix)]
                        unsafe { libc::kill(pid as i32, libc::SIGTERM); }
                    }
                }
                let _ = std::fs::remove_file(&server_pid_file);
            }
            Ok(serde_json::json!({ "ok": true, "stopped": stopped }))
        }
        LocalTeamCmd::Logs { name, lines } => {
            let n = lines.unwrap_or(40) as usize;
            let dir = member_base_dir().join(name);
            let head = std::fs::read_to_string(dir.join(".ttyd-".to_string() + name + ".log"))
                .unwrap_or_default()
                .lines()
                .rev()
                .take(n)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");
            Ok(serde_json::json!({ "ok": true, "member": name, "ttyd_log": head }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::teamfile;

    fn sample_team() -> String {
        "# T\n\n## 背景\n背景内容\n\n## 目标\n- 目标一\n\n## 成员\n\n### writer\n- 姓名: Writer\n- 角色: writer\n- 分工: 写报告\n- 技能: 写作、总结\n- 输出: 报告.md\n\n### powernano\n- 姓名: owner\n- 角色: owner\n- 分工: 统筹\n".to_string()
    }

    #[test]
    fn auto_member_filters_owner() {
        assert!(is_auto_member(Some("searcher")));
        assert!(!is_auto_member(Some("owner")));
        assert!(!is_auto_member(Some("Owner")));
    }

    #[test]
    fn parses_members_and_filters_owner() {
        let tf = teamfile::parse_team_file_text(&sample_team()).unwrap();
        assert_eq!(tf.team_name, "T");
        let keys: Vec<&str> = tf.members.iter().map(|m| m.key.as_str()).collect();
        assert!(keys.contains(&"writer") && keys.contains(&"powernano"));
        let auto: Vec<&str> = tf.members.iter().filter(|m| is_auto_member(m.role.as_deref())).map(|m| m.key.as_str()).collect();
        assert_eq!(auto, vec!["writer"]);
    }

    #[test]
    fn persona_contains_identity_and_skills() {
        let tf = teamfile::parse_team_file_text(&sample_team()).unwrap();
        let w = tf.members.iter().find(|m| m.key == "writer").unwrap();
        let dir = std::env::temp_dir().join(format!("lt-persona-{}", uuid::Uuid::new_v4()));
        write_persona(&dir, w, &tf, &tf.goals).unwrap();
        let s = std::fs::read_to_string(dir.join("AGENTS.md")).unwrap();
        assert!(s.contains("Writer"));
        assert!(s.contains("写报告"));
        assert!(s.contains("写作"));
        assert!(s.contains("背景内容"));
        assert!(s.contains("目标一"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn member_base_dir_under_teamx() {
        // Without chdir this is just a sanity check of the path shape.
        let p = member_base_dir();
        assert!(p.ends_with(".teamx/local-team"));
    }
}
