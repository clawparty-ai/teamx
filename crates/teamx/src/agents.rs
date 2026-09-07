//! Headless agent (opencode instance) manager (`teamx agents`).
//!
//! Spawn / list / stop multiple `opencode serve` processes on one machine so a
//! single host can run several AI agent members (one workspace per instance)
//! without an external orchestrator such as ClawParty. State is a JSON
//! registry under `<teamx_home>/agents/instances.json`; each spawned process is
//! recorded with its pid/port/dir so `list`/`stop` can discover it even across
//! separate CLI invocations.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::cli::AgentsCmd;
use crate::db;

/// Registry root: `<teamx_home>/agents`.
fn registry_dir() -> PathBuf {
    db::teamx_home().join("agents")
}

fn registry_path() -> PathBuf {
    registry_dir().join("instances.json")
}

/// Take an exclusive advisory lock on the registry lock file for the duration
/// of `f`. Guards concurrent spawn/list/stop against read-modify-write races
/// (two `teamx agents spawn` at once) and port double-allocation.
#[cfg(unix)]
fn with_registry_lock<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    use std::os::unix::io::AsRawFd;
    let dir = registry_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let lock_path = dir.join(".lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| format!("open lock {}: {e}", lock_path.display()))?;
    // Block until the lock is available (serializes multi-instance spawn).
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if rc != 0 {
        return Err("could not acquire agents registry lock".to_string());
    }
    let result = f();
    let _ = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    result
}

/// Non-unix fallback: no advisory lock, execute directly.
#[cfg(not(unix))]
fn with_registry_lock<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    f()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Registry {
    instances: BTreeMap<String, Instance>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Instance {
    name: String,
    dir: String,
    port: u16,
    opencode: String,
    pid: Option<u32>,
    started_at: String,
    log: Option<String>,
}

impl Default for Registry {
    fn default() -> Self {
        Registry { instances: BTreeMap::new() }
    }
}

fn now_iso() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    format!("{}", secs)
}

fn load() -> Registry {
    match std::fs::read_to_string(registry_path()) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => Registry::default(),
    }
}

fn save(r: &Registry) -> Result<(), String> {
    let dir = registry_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let s = serde_json::to_string_pretty(r).map_err(|e| format!("serialize registry: {e}"))?;
    std::fs::write(registry_path(), s).map_err(|e| format!("write registry: {e}"))
}

/// True when a TCP port is already bound on 127.0.0.1.
fn port_in_use(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_err()
}

fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

fn default_dir(name: &str) -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(name)
}

/// Pick a free port starting from `preferred` (or 4200), skipping used ones.
fn pick_port(preferred: Option<u16>) -> u16 {
    let start = preferred.unwrap_or(4200);
    for p in start..=start.saturating_add(200) {
        if !port_in_use(p) {
            return p;
        }
    }
    start
}

/// Try to spawn `opencode serve` on exactly `port`. Returns the child pid if
/// the process is still alive shortly after spawn; otherwise returns the log
/// tail so the caller can decide whether to retry on the next port (a live
/// process exiting right away almost always means the port was taken by a
/// concurrent spawn).
fn attempt_spawn(
    bin: &str,
    ws: &Path,
    port: u16,
    envs: &[String],
    log_path: &Path,
) -> Result<u32, String> {
    let mut cmd = Command::new(bin);
    cmd.arg("serve").arg("--hostname").arg("127.0.0.1").arg("--port").arg(port.to_string());
    cmd.current_dir(ws);
    // Each instance keeps its own opencode DB so sessions do not collide.
    cmd.env("OPENCODE_DB", ws.join("opencode.db"));
    for kv in envs {
        if let Some((k, v)) = kv.split_once('=') {
            if !k.is_empty() {
                cmd.env(k, v);
            }
        }
    }
    let logf = std::fs::File::create(log_path).map_err(|e| format!("open log {}: {e}", log_path.display()))?;
    let errf = logf.try_clone().map_err(|e| format!("clone log: {e}"))?;
    let child = cmd
        .stdout(Stdio::from(logf))
        .stderr(Stdio::from(errf))
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn opencode: {e}"))?;
    let pid = child.id();
    std::thread::sleep(std::time::Duration::from_millis(600));
    if !is_process_alive(pid) {
        let tail = std::fs::read_to_string(log_path).unwrap_or_default();
        return Err(format!("agent on port {port} exited immediately; log tail:\n{}", tail.chars().take(500).collect::<String>()));
    }
    Ok(pid)
}

fn spawn(
    name: &str,
    dir: Option<&Path>,
    port: Option<u16>,
    opencode: Option<&str>,
    envs: &[String],
    foreground: bool,
) -> Result<serde_json::Value, String> {
    if name.is_empty() {
        return Err("instance name is required".to_string());
    }
    if name.contains('/') || name.contains('\\') || name.contains(char::is_whitespace) {
        return Err("instance name must be a single token (no spaces or slashes)".to_string());
    }
    let reg = load();
    if reg.instances.contains_key(name) {
        return Err(format!("instance `{name}` already exists"));
    }
    // Drop the early read; re-load right before insert so a concurrent spawn
    // that committed in between is not overwritten.
    drop(reg);
    let ws = dir.map(|d| d.to_path_buf()).unwrap_or_else(|| default_dir(name));
    if !ws.exists() {
        std::fs::create_dir_all(&ws).map_err(|e| format!("mkdir {}: {e}", ws.display()))?;
    }
    let bin = opencode.unwrap_or("opencode").to_string();
    let start = pick_port(port);

    if foreground {
        // Foreground: try the preferred port once; inherit stdio.
        let mut cmd = Command::new(&bin);
        cmd.arg("serve").arg("--hostname").arg("127.0.0.1").arg("--port").arg(start.to_string());
        cmd.current_dir(&ws);
        cmd.env("OPENCODE_DB", ws.join("opencode.db"));
        for kv in envs {
            if let Some((k, v)) = kv.split_once('=') {
                if !k.is_empty() {
                    cmd.env(k, v);
                }
            }
        }
        let status = cmd.status().map_err(|e| format!("failed to run opencode: {e} (is opencode installed?)"))?;
        return Ok(serde_json::json!({
            "ok": true,
            "name": name,
            "port": start,
            "dir": ws.display().to_string(),
            "exit": status.code(),
        }));
    }

    // Daemonize. Retry across candidate ports to tolerate a concurrent spawn
    // winning the preferred port between our check and the child's bind.
    std::fs::create_dir_all(registry_dir()).map_err(|e| format!("mkdir log dir: {e}"))?;
    let log_path = registry_dir().join(format!("{name}.log"));
    let mut chosen: Option<(u32, u16)> = None;
    let mut last_err: Option<String> = None;
    for offset in 0..50u16 {
        let p = start.saturating_add(offset);
        if port_in_use(p) {
            continue;
        }
        match attempt_spawn(&bin, &ws, p, envs, &log_path) {
            Ok(pid) => {
                chosen = Some((pid, p));
                break;
            }
            Err(e) => last_err = Some(e),
        }
    }
    let (pid, port) = chosen.ok_or_else(|| {
        last_err.unwrap_or_else(|| format!("could not spawn agent on any port starting at {start}"))
    })?;

    // Re-load the registry just before commit so a concurrent spawn that saved
    // meanwhile is preserved (best-effort; a true lock would need a file lock).
    let mut reg = load();
    reg.instances.insert(
        name.to_string(),
        Instance {
            name: name.to_string(),
            dir: ws.display().to_string(),
            port,
            opencode: bin,
            pid: Some(pid),
            started_at: now_iso(),
            log: Some(log_path.display().to_string()),
        },
    );
    save(&reg)?;
    Ok(serde_json::json!({
        "ok": true,
        "name": name,
        "port": port,
        "dir": ws.display().to_string(),
        "pid": pid,
        "log": log_path.display().to_string(),
    }))
}

fn status_of(pid: Option<u32>) -> &'static str {
    match pid {
        Some(p) if is_process_alive(p) => "running",
        _ => "stopped",
    }
}

fn list(running_only: bool) -> Result<serde_json::Value, String> {
    let reg = load();
    let mut out = Vec::new();
    for inst in reg.instances.values() {
        let st = status_of(inst.pid);
        if running_only && st != "running" {
            continue;
        }
        out.push(serde_json::json!({
            "name": inst.name,
            "port": inst.port,
            "dir": inst.dir,
            "opencode": inst.opencode,
            "pid": inst.pid,
            "status": st,
            "started_at": inst.started_at,
            "log": inst.log,
        }));
    }
    Ok(serde_json::json!({ "ok": true, "instances": out }))
}

fn stop(name: &str, remove: bool) -> Result<serde_json::Value, String> {
    let mut reg = load();
    let inst = reg.instances.get(name).cloned().ok_or_else(|| format!("instance `{name}` not found"))?;
    let mut stopped = false;
    if let Some(pid) = inst.pid {
        if is_process_alive(pid) {
            #[cfg(unix)]
            let sig = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
            #[cfg(unix)]
            let _ = sig;
            // Wait up to ~2s, then SIGKILL if still alive.
            for _ in 0..10 {
                if !is_process_alive(pid) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            if is_process_alive(pid) {
                #[cfg(unix)]
                {
                    let _ = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
                }
            }
            stopped = true;
        }
    }
    if remove {
        reg.instances.remove(name);
        save(&reg)?;
    }
    Ok(serde_json::json!({ "ok": true, "name": name, "stopped": stopped, "removed": remove }))
}

/// Dispatch for `teamx agents`. All mutations go through the registry lock so
/// concurrent spawn/list/stop are serialized (no read-modify-write races, no
/// double port allocation).
pub fn run(cmd: &AgentsCmd) -> Result<serde_json::Value, String> {
    with_registry_lock(|| match cmd {
        AgentsCmd::Spawn { name, dir, port, opencode, env, foreground } => {
            spawn(name, dir.as_deref(), *port, opencode.as_deref(), env, *foreground)
        }
        AgentsCmd::List { running } => list(*running),
        AgentsCmd::Stop { name, remove } => stop(name, *remove),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();

    /// Run a closure with a temp TEAMX_HOME (serialized; restores previous).
    fn with_tmp_home<F: FnOnce(&Path)>(f: F) {
        let lock = ENV_LOCK.get_or_init(|| std::sync::Mutex::new(()));
        let _guard = lock.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("teamx-agents-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let prev = std::env::var("TEAMX_HOME").ok();
        std::env::set_var("TEAMX_HOME", &tmp);
        f(&tmp);
        match prev {
            Some(v) => std::env::set_var("TEAMX_HOME", v),
            None => std::env::remove_var("TEAMX_HOME"),
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn registry_empty_load_is_default() {
        with_tmp_home(|_| {
            let r = load();
            assert!(r.instances.is_empty());
        });
    }

    #[test]
    fn registry_save_load_roundtrip() {
        with_tmp_home(|_| {
            let mut r = Registry::default();
            r.instances.insert(
                "a".into(),
                Instance {
                    name: "a".into(),
                    dir: "/tmp/a".into(),
                    port: 4200,
                    opencode: "opencode".into(),
                    pid: Some(123),
                    started_at: "now".into(),
                    log: None,
                },
            );
            save(&r).unwrap();
            let loaded = load();
            assert_eq!(loaded.instances.len(), 1);
            let inst = &loaded.instances["a"];
            assert_eq!(inst.port, 4200);
            assert_eq!(inst.pid, Some(123));
        });
    }

    #[test]
    fn pick_port_skips_used_ports() {
        // Bind 127.0.0.1 on some port, ensure pick_port(Some(that)) avoids it.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let used = listener.local_addr().unwrap().port();
        let picked = pick_port(Some(used));
        assert_ne!(picked, used, "must not return an already-bound port");
    }

    #[test]
    fn spawn_rejects_bad_names_before_launching() {
        with_tmp_home(|_| {
            // empty name
            assert!(spawn("", None, None, None, &[], false).is_err());
            // whitespace
            assert!(spawn("has space", None, None, None, &[], false).is_err());
            // slash
            assert!(spawn("a/b", None, None, None, &[], false).is_err());
        });
    }

    #[test]
    fn spawn_duplicate_name_rejected() {
        with_tmp_home(|_| {
            // Register a fake instance under the name, then spawn should reject.
            let mut r = Registry::default();
            r.instances.insert(
                "dup".into(),
                Instance {
                    name: "dup".into(),
                    dir: "/tmp/d".into(),
                    port: 1,
                    opencode: "x".into(),
                    pid: None,
                    started_at: "now".into(),
                    log: None,
                },
            );
            save(&r).unwrap();
            // spawn checks the existing registry *before* launching opencode, so
            // it must error here (never tries to exec a binary).
            let res = spawn("dup", None, None, Some("definitely-not-a-real-binary"), &[], false);
            assert!(res.is_err());
            assert!(res.unwrap_err().contains("already exists"));
        });
    }

    #[test]
    fn stop_unknown_instance_errors() {
        with_tmp_home(|_| {
            let res = stop("nope", false);
            assert!(res.is_err());
            assert!(res.unwrap_err().contains("not found"));
        });
    }

    #[test]
    fn status_of_reports_stopped_for_missing_pid() {
        // pid=None (no registered pid) is always stopped, no process probing.
        assert_eq!(status_of(None), "stopped");
    }
}
