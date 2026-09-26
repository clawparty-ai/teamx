//! console_repos.rs — "pull a team repository" for the member web console.
//!
//! The member console runs inside the workspace Pod; team code lives on the
//! tenant server, reached over native git-over-mTLS
//! (`https://<server>/git/<team>/<repo>`). This module:
//!
//! - lists the repos the calling member can read (server `git.repos` RPC),
//! - clones one, on demand, into `<workspace>/<repo>` using the system `git`
//!   and the invitation letter's client certificate,
//! - reports progress from git's own `--progress` stderr tail.
//!
//! Cloning an 11 GiB repo takes minutes, so a clone runs on a detached thread:
//! `POST /api/repos/clone` returns immediately and the browser polls
//! `GET /api/repos/clone/status` — no long-held HTTP request, no browser
//! timeout.
//!
//! Security: repo names are validated (no path separators, no `..`, no leading
//! `-`/`.`, no whitespace/control chars), so `root.join(name)` can never escape
//! `/workspace`; every git argument is an array element (no shell), with `--`
//! separating options from the URL and the destination.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::console_data::Ctx;

/// A clone job's in-memory state (also the polling payload).
#[derive(Clone, Debug)]
struct CloneJob {
    name: String,
    /// "running" | "done" | "error"
    state: String,
    started_ms: u64,
    finished_ms: Option<u64>,
    error: Option<String>,
    /// git stderr is redirected here so progress can be tailed while running.
    log: Option<String>,
}

fn jobs() -> &'static Mutex<HashMap<String, CloneJob>> {
    static JOBS: OnceLock<Mutex<HashMap<String, CloneJob>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Longest accepted repo name; server names are short, and the cap keeps a
/// hostile name from bloating the job key / log path.
const MAX_REPO_NAME: usize = 200;

/// Validate a repository name coming from the browser.
///
/// Reuses the server rule (`crate::git_service::validate_name`) for the core
/// rejections (empty, `/`, `\`, `..`, whitespace) and adds the console's own
/// guardrails: a length cap, no leading `-` (never an option-like argument) or
/// `.` (never a hidden entry such as `.teamx`), and no control characters.
pub fn validate_repo_name(name: &str) -> Result<(), String> {
    crate::git_service::validate_name(name)?;
    if name.len() > MAX_REPO_NAME {
        return Err(format!("repository name too long (>{MAX_REPO_NAME} chars)"));
    }
    if name.starts_with('-') || name.starts_with('.') {
        return Err(format!("invalid repository name `{name}`"));
    }
    if name.chars().any(char::is_control) {
        return Err(format!("invalid repository name `{name}`"));
    }
    Ok(())
}

/// Human-readable byte size (B / KB / MB / GB).
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    const GB: u64 = 1024 * 1024 * 1024;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// Resolve the clone destination `<root>/<name>` for a validated name.
pub fn target_dir(root: &Path, name: &str) -> Result<PathBuf, String> {
    validate_repo_name(name)?;
    let target = root.join(name);
    // Defensive: a separator-free name cannot escape, but assert the invariant
    // the whole feature relies on.
    if !target.starts_with(root) {
        return Err(format!("repository `{name}` resolves outside the workspace"));
    }
    Ok(target)
}

/// Build the argv for `git` (options first, then `clone -- <url> <dir>`).
///
/// `config` holds `(key, value)` pairs emitted as `-c key=value` (the mTLS
/// `http.<server>/.sslCert` … keys); passing them per-invocation avoids writing
/// the caller's global `~/.gitconfig`.
pub fn build_clone_args(url: &str, dir: &Path, config: &[(String, String)]) -> Vec<String> {
    let mut args: Vec<String> = Vec::with_capacity(config.len() * 2 + 5);
    for (k, v) in config {
        args.push("-c".to_string());
        args.push(format!("{k}={v}"));
    }
    args.push("clone".to_string());
    args.push("--progress".to_string());
    args.push("--".to_string());
    args.push(url.to_string());
    args.push(dir.to_string_lossy().to_string());
    args
}

/// The mTLS git config for the invitation letter's cert/key/CA.
pub fn mtls_config(base: &str, cert: &Path, key: &Path, ca: &Path) -> Vec<(String, String)> {
    let base = base.trim_end_matches('/');
    vec![
        (
            format!("http.{base}/.sslCert"),
            cert.to_string_lossy().to_string(),
        ),
        (
            format!("http.{base}/.sslKey"),
            key.to_string_lossy().to_string(),
        ),
        (
            format!("http.{base}/.sslCAInfo"),
            ca.to_string_lossy().to_string(),
        ),
    ]
}

/// Read the last `max` bytes of a file (git progress), normalizing `\r` to
/// `\n` so the browser can display it. Missing/unreadable files yield "".
pub fn read_tail(path: &Path, max: usize) -> String {
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(max as u64);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&buf).replace('\r', "\n")
}

/// Run a `git` clone (argv array), redirecting stderr to `log_path` so progress
/// can be tailed. Never prompts. Returns git's stderr tail on failure.
pub fn run_clone_process(args: &[String], log_path: &Path) -> Result<(), String> {
    let log = std::fs::File::create(log_path).map_err(|e| format!("create clone log: {e}"))?;
    let status = Command::new("git")
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .status()
        .map_err(|e| format!("failed to run git: {e} (is git installed?)"))?;
    if status.success() {
        return Ok(());
    }
    let tail = read_tail(log_path, 1200);
    Err(format!(
        "git clone exited {}: {}",
        status.code().unwrap_or(-1),
        tail.trim()
    ))
}

fn dir_nonempty(p: &Path) -> bool {
    std::fs::read_dir(p)
        .map(|mut it| it.next().is_some())
        .unwrap_or(false)
}

/// Serialize a job for the browser (includes the live progress tail).
fn job_json(job: &CloneJob) -> Value {
    let elapsed = job
        .finished_ms
        .unwrap_or_else(now_ms)
        .saturating_sub(job.started_ms);
    let tail = job
        .log
        .as_deref()
        .map(|p| read_tail(Path::new(p), 900))
        .unwrap_or_default();
    json!({
        "ok": true,
        "name": job.name,
        "state": job.state,
        "started_ms": job.started_ms,
        "finished_ms": job.finished_ms,
        "elapsed_ms": elapsed,
        "error": job.error,
        "tail": tail.trim_end(),
    })
}

/// `GET /api/repos` payload: the accessible repos with a local-presence flag.
pub fn list_repos(ctx: &Ctx) -> Result<Value, String> {
    let root = crate::console_data::workspace_root(ctx);
    let data = crate::console_data::repos_data(ctx)?;
    let team = data
        .get("team")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let repos = data
        .get("repos")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let out: Vec<Value> = repos
        .iter()
        .map(|r| {
            let name = r.get("name").and_then(Value::as_str).unwrap_or_default();
            let bytes = r.get("bytes").and_then(Value::as_u64).unwrap_or(0);
            json!({
                "name": name,
                "bytes": bytes,
                "size_text": format_size(bytes),
                "description": r.get("description").cloned().unwrap_or(Value::Null),
                "updated_at": r.get("updated_at").cloned().unwrap_or(Value::Null),
                "local": root.join(name).is_dir(),
            })
        })
        .collect();
    Ok(json!({ "ok": true, "team": team, "repos": out }))
}

/// `POST /api/repos/clone` — validate, authorize against the server's repo
/// list, then start a detached clone. Returns the initial (running) job.
pub fn start_clone(ctx: &Ctx, name: &str) -> Result<Value, String> {
    validate_repo_name(name)?;
    let root = crate::console_data::workspace_root(ctx);
    let server = ctx
        .server_url()
        .ok_or_else(|| "team repositories require network mode (no server URL)".to_string())?
        .to_string();

    // Reuse the server's authorized list: this both authorizes the repo and
    // yields the team id needed to build the clone URL (member identity = the
    // invitation letter, not the owner session).
    let data = crate::console_data::repos_data(ctx)?;
    let team = data
        .get("team")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "server did not report the team id".to_string())?
        .to_string();
    let repos = data
        .get("repos")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    repos
        .iter()
        .find(|r| r.get("name").and_then(Value::as_str) == Some(name))
        .ok_or_else(|| format!("repository `{name}` not found (or you lack read access)"))?;

    // Never clobber user data: the target must be absent or an empty directory.
    let target = target_dir(&root, name)?;
    if target.exists() {
        if !target.is_dir() {
            return Err(format!(
                "`{}` exists and is not a directory; not touching it",
                target.display()
            ));
        }
        if dir_nonempty(&target) {
            return Err(format!(
                "`{}` already exists and is not empty; not touching it",
                target.display()
            ));
        }
    }

    // Idempotent start: a running clone for this name is returned as-is.
    if let Some(existing) = jobs().lock().unwrap().get(name) {
        if existing.state == "running" {
            return Ok(job_json(existing));
        }
    }

    let log_path = std::env::temp_dir().join(format!(
        "teamx-clone-{}-{}.log",
        crate::console_data::slugify(name),
        uuid::Uuid::new_v4()
    ));
    let job = CloneJob {
        name: name.to_string(),
        state: "running".to_string(),
        started_ms: now_ms(),
        finished_ms: None,
        error: None,
        log: Some(log_path.to_string_lossy().to_string()),
    };
    jobs().lock().unwrap().insert(name.to_string(), job.clone());
    let name_owned = name.to_string();
    std::thread::spawn(move || run_clone(name_owned, server, team, root, log_path));
    Ok(job_json(&job))
}

/// `GET /api/repos/clone/status?name=` — current job state (or `idle`).
pub fn clone_status(name: &str) -> Result<Value, String> {
    validate_repo_name(name)?;
    let guard = jobs().lock().unwrap();
    match guard.get(name) {
        Some(job) => Ok(job_json(job)),
        None => Ok(json!({ "ok": true, "name": name, "state": "idle" })),
    }
}

/// Detached worker: clone, then record the terminal state.
fn run_clone(name: String, server: String, team: String, root: PathBuf, log_path: PathBuf) {
    let target = root.join(&name);
    let result = clone_via_git(&server, &team, &name, &target, &log_path);
    // Record the outcome under the lock, then release it before any cleanup.
    {
        let mut guard = jobs().lock().unwrap();
        if let Some(job) = guard.get_mut(&name) {
            job.finished_ms = Some(now_ms());
            match &result {
                Ok(()) => {
                    job.state = "done".to_string();
                    job.error = None;
                }
                Err(e) => {
                    job.state = "error".to_string();
                    job.error = Some(e.clone());
                }
            }
        }
    }
    if result.is_err() && target.join(".git").exists() {
        // A pre-existing non-empty directory was refused up-front, so any
        // `.git` here is the partial clone we just made — safe to remove so a
        // retry is possible.
        let _ = std::fs::remove_dir_all(&target);
    }
}

/// Build the mTLS clone command from the workspace letter and run it.
fn clone_via_git(
    server: &str,
    team: &str,
    repo: &str,
    target: &Path,
    log: &Path,
) -> Result<(), String> {
    let mat = crate::git_client::find_letter(server).map_err(|e| e.to_string())?;
    let base = server.trim_end_matches('/');
    let url = format!("{base}/git/{team}/{repo}");
    let cfg = mtls_config(base, &mat.cert, &mat.key, &mat.ca);
    let args = build_clone_args(&url, target, &cfg);
    run_clone_process(&args, log)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_escaping_and_flag_like_names() {
        for bad in [
            "",
            ".",
            "..",
            "../x",
            "a/b",
            "a\\b",
            "a b",
            "-x",
            "--upload-pack",
            ".teamx",
            "a\nb",
        ] {
            assert!(validate_repo_name(bad).is_err(), "{bad:?} should be rejected");
        }
        for good in ["teamx", "repo-1", "a.b", "A_B", "r2d2"] {
            assert!(validate_repo_name(good).is_ok(), "{good:?} should be accepted");
        }
    }

    #[test]
    fn target_dir_stays_under_root() {
        let root = PathBuf::from("/workspace");
        assert_eq!(target_dir(&root, "repo").unwrap(), root.join("repo"));
        assert!(target_dir(&root, "../etc").is_err());
        assert!(target_dir(&root, "a/b").is_err());
        assert!(target_dir(&root, "/etc/passwd").is_err());
    }

    #[test]
    fn clone_args_are_an_argument_array_with_separators() {
        let args = build_clone_args(
            "https://s:8888/git/T/R",
            Path::new("/workspace/R"),
            &mtls_config(
                "https://s:8888",
                Path::new("/l/client.crt"),
                Path::new("/l/client.key"),
                Path::new("/l/ca.crt"),
            ),
        );
        assert_eq!(args[0], "-c");
        for key in [
            "http.https://s:8888/.sslCert=/l/client.crt",
            "http.https://s:8888/.sslKey=/l/client.key",
            "http.https://s:8888/.sslCAInfo=/l/ca.crt",
        ] {
            assert!(args.contains(&key.to_string()), "missing {key}");
        }
        let clone = args.iter().position(|a| a == "clone").unwrap();
        assert_eq!(args[clone + 1], "--progress");
        assert_eq!(args[clone + 2], "--");
        assert_eq!(args[clone + 3], "https://s:8888/git/T/R");
        assert_eq!(args[clone + 4], "/workspace/R");
        // No shell metacharacters ever reach a single argument.
        assert!(!args.iter().any(|a| a.contains(';') || a.contains('|')));
    }

    #[test]
    fn size_formatting_is_human_readable() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(1024 * 1024), "1.0 MB");
        assert_eq!(format_size(11 * 1024 * 1024 * 1024), "11.0 GB");
    }

    #[test]
    fn clone_from_local_bare_repo() {
        // End-to-end through `build_clone_args` + `run_clone_process` with a
        // local `file://` bare repo (no mTLS, no network): verifies the command
        // shape and the process wiring.
        if Command::new("git").arg("--version").output().is_err() {
            return; // git not installed -> skip
        }
        let root = std::env::temp_dir().join(format!(
            "teamx-console-repo-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let bare = root.join("origin.git");
        let work = root.join("work");
        let dst = root.join("clone");
        std::fs::create_dir_all(&root).unwrap();
        let run = |cwd: &Path, args: &[&str]| {
            let out = Command::new("git")
                .current_dir(cwd)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&root, &["init", "--bare", "-b", "main", bare.to_str().unwrap()]);
        run(&root, &["clone", bare.to_str().unwrap(), work.to_str().unwrap()]);
        std::fs::write(work.join("hello.txt"), "hi").unwrap();
        run(&work, &["add", "-A"]);
        run(
            &work,
            &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-m", "init"],
        );
        run(&work, &["push", "origin", "main"]);

        let url = format!("file://{}", bare.display());
        let args = build_clone_args(&url, &dst, &[]);
        let log = root.join("clone.log");
        run_clone_process(&args, &log).unwrap();
        assert!(dst.join("hello.txt").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }
}
