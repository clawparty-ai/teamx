//! Git client operations for teamx.
//!
//! Implements `teamx git` commands: clone/pull/push/commit/list/create/delete.
//!
//! Transport: git repositories live on the teamx server as bare repos and are
//! exchanged as git bundles (base64) over the mTLS JSON-RPC channel. The local
//! side uses the system `git` binary — no libgit2 dependency.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

/// Where a cloned repo remembers its teamx origin: `.git/teamx-origin.json`.
const ORIGIN_FILE: &str = ".git/teamx-origin.json";

/// Client-side git error.
#[derive(Debug)]
pub struct GitError(pub String);

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for GitError {}

/// Local teamx origin metadata stored inside a cloned repo.
#[derive(serde::Serialize, serde::Deserialize)]
struct GitOrigin {
    server_url: String,
    team: String,
    repo: String,
    branch: String,
}

/// Run a local git command in `cwd`; returns stdout trimmed, or an error.
fn git_in(cwd: &Path, args: &[&str]) -> Result<String, GitError> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| GitError(format!("failed to run git: {e} (is git installed?)")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        return Err(GitError(format!("git {} failed: {}", args.join(" "), detail)));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Working directory for repo-local commands (pull/push/commit).
fn cwd_or(dir: Option<&str>) -> Result<PathBuf, GitError> {
    match dir {
        Some(d) => {
            let p = PathBuf::from(d);
            if !p.is_dir() {
                return Err(GitError(format!("directory `{d}` does not exist")));
            }
            Ok(p)
        }
        None => std::env::current_dir().map_err(|e| GitError(format!("cwd: {e}"))),
    }
}

/// Write teamx origin metadata into a cloned repo.
fn write_origin(dir: &Path, origin: &GitOrigin) -> Result<(), GitError> {
    let text = serde_json::to_string_pretty(origin)
        .map_err(|e| GitError(format!("serialize origin: {e}")))?;
    std::fs::write(dir.join(ORIGIN_FILE), text)
        .map_err(|e| GitError(format!("write {ORIGIN_FILE}: {e}")))
}

/// Read teamx origin metadata from a cloned repo.
fn read_origin(dir: &Path) -> Result<GitOrigin, GitError> {
    let path = dir.join(ORIGIN_FILE);
    let text = std::fs::read_to_string(&path).map_err(|_| {
        GitError(format!(
            "not a teamx repo (missing {ORIGIN_FILE}); clone it first with `teamx git clone`"
        ))
    })?;
    serde_json::from_str(&text).map_err(|e| GitError(format!("parse {ORIGIN_FILE}: {e}")))
}

/// Run a JSON-RPC call against the teamx server with mTLS and unwrap the
/// JSON-RPC envelope, returning the inner `data` object.
fn rpc(server_url: &str, method: &str, args: Value) -> Result<Value, GitError> {
    let resp = crate::tunnel_client::run_rpc(server_url, method, args)
        .map_err(|e| GitError(format!("rpc {method}: {e}")))?;
    // Server replies `{"ok": true, "data": {...}}` (or `{"ok": false, "error"}`).
    if resp.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(GitError(
            resp.get("error").and_then(Value::as_str).unwrap_or("rpc failed").to_string(),
        ));
    }
    Ok(resp.get("data").cloned().unwrap_or(resp))
}

/// Decode a base64 bundle into a temp file; returns the temp path.
fn write_bundle(b64: &str, prefix: &str) -> Result<PathBuf, GitError> {
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64)
        .map_err(|e| GitError(format!("bad bundle payload: {e}")))?;
    let tmp = std::env::temp_dir().join(format!("{prefix}-{}.bundle", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, &bytes).map_err(|e| GitError(format!("write bundle: {e}")))?;
    Ok(tmp)
}

/// Resolve a team id for git RPCs: explicit > single-team member > error.
fn resolve_team(server_url: &str, team: Option<&str>) -> Result<String, GitError> {
    if let Some(t) = team {
        if !t.is_empty() {
            return Ok(t.to_string());
        }
    }
    let resp = rpc(server_url, "team.list", json!({}))?;
    let teams = resp.get("teams").and_then(Value::as_array).cloned().unwrap_or_default();
    if teams.len() == 1 {
        return Ok(teams[0].get("team_id").and_then(Value::as_str).unwrap_or_default().to_string());
    }
    Err(GitError(format!(
        "cannot resolve team (member belongs to {} teams); pass --team <id>",
        teams.len()
    )))
}

/// `teamx git clone <repo> [--directory <dir>]`.
pub fn clone(server_url: &str, repo: &str, directory: Option<&str>, team: Option<&str>) -> Result<Value, GitError> {
    let team = resolve_team(server_url, team)?;
    let resp = rpc(server_url, "git.bundle", json!({ "team": team, "name": repo }))?;
    if resp.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(GitError(resp.to_string()));
    }
    let bundle_b64 = resp["bundle"].as_str().unwrap_or_default().to_string();
    let branch = resp["branch"].as_str().unwrap_or("main").to_string();
    let dir = PathBuf::from(directory.unwrap_or(repo));
    if dir.exists() {
        return Err(GitError(format!("directory `{}` already exists", dir.display())));
    }
    std::fs::create_dir_all(&dir).map_err(|e| GitError(format!("mkdir {}: {e}", dir.display())))?;

    if bundle_b64.is_empty() {
        // Empty remote repo: just init with the default branch.
        git_in(&dir, &["init", "-b", &branch])?;
    } else {
        let tmp = write_bundle(&bundle_b64, "clone")?;
        git_in(&dir, &["init", "-b", &branch])?;
        git_in(&dir, &["fetch", tmp.to_str().unwrap_or_default(), "+refs/heads/*:refs/remotes/teamx/*"])?;
        // Check out the default branch. `init -b` creates an unborn branch, so
        // `checkout -b <branch>` fails with "already on"; fall back to tracking
        // checkout of the remote ref.
        let target = format!("teamx/{branch}");
        if git_in(&dir, &["checkout", "-b", &branch, &target]).is_err() {
            let _ = git_in(&dir, &["checkout", &target]);
            let _ = git_in(&dir, &["branch", "-M", &branch]);
        }
        let _ = std::fs::remove_file(&tmp);
    }

    write_origin(&dir, &GitOrigin {
        server_url: server_url.to_string(),
        team: team.clone(),
        repo: repo.to_string(),
        branch: branch.clone(),
    })?;
    let _ = git_in(&dir, &["remote", "add", "teamx", &format!("teamx::{team}/{repo}")]);

    Ok(json!({
        "ok": true,
        "repo": repo,
        "branch": branch,
        "directory": dir.display().to_string(),
        "note": format!("cloned into {}; use `teamx git pull`/`push` in that directory", dir.display()),
    }))
}

/// `teamx git pull <repo> [--branch <branch>]`.
pub fn pull(
    server_url: &str,
    repo: &str,
    branch: Option<&str>,
    team: Option<&str>,
    dir: Option<&str>,
) -> Result<Value, GitError> {
    let cwd = cwd_or(dir)?;
    let origin = read_origin(&cwd)?;
    let team = team.unwrap_or(&origin.team).to_string();
    let branch = branch.map(str::to_string).unwrap_or(origin.branch.clone());

    let resp = rpc(server_url, "git.bundle", json!({ "team": team, "name": repo }))?;
    if resp.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(GitError(resp.to_string()));
    }
    let bundle_b64 = resp["bundle"].as_str().unwrap_or_default().to_string();
    if bundle_b64.is_empty() {
        return Ok(json!({ "ok": true, "repo": repo, "updated": false, "message": "remote has no commits" }));
    }
    let tmp = write_bundle(&bundle_b64, "pull")?;
    git_in(&cwd, &["fetch", tmp.to_str().unwrap_or_default(), "+refs/heads/*:refs/remotes/teamx/*"])?;
    let merge_ref = format!("teamx/{branch}");
    let merge = git_in(&cwd, &["merge", "--ff-only", &merge_ref]);
    let _ = std::fs::remove_file(&tmp);
    match merge {
        Ok(msg) => Ok(json!({
            "ok": true,
            "repo": repo,
            "branch": branch,
            "updated": true,
            "message": format!("pulled {branch}: {msg}"),
        })),
        Err(e) => Err(GitError(format!("merge failed (resolve conflicts, then commit): {e}"))),
    }
}

/// `teamx git push <repo> [--branch <branch>]`.
pub fn push(
    server_url: &str,
    repo: &str,
    branch: Option<&str>,
    team: Option<&str>,
    dir: Option<&str>,
) -> Result<Value, GitError> {
    let cwd = cwd_or(dir)?;
    let origin = read_origin(&cwd)?;
    let team = team.unwrap_or(&origin.team).to_string();
    let branch = branch.map(str::to_string).unwrap_or(origin.branch.clone());

    // Create a bundle of the local branch refs.
    let tmp = std::env::temp_dir().join(format!("teamx-push-local-{}.bundle", uuid::Uuid::new_v4()));
    git_in(&cwd, &["bundle", "create", tmp.to_str().unwrap_or_default(), "--all"])?;
    let bytes = std::fs::read(&tmp).map_err(|e| GitError(format!("read bundle: {e}")))?;
    let bundle_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
    let _ = std::fs::remove_file(&tmp);

    let resp = rpc(server_url, "git.receive", json!({ "team": team, "name": repo, "bundle": bundle_b64 }))?;
    if resp.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(GitError(format!(
            "push rejected: {}",
            resp.get("error").and_then(Value::as_str).unwrap_or(&resp.to_string())
        )));
    }
    Ok(json!({ "ok": true, "repo": repo, "branch": branch, "message": "pushed" }))
}

/// `teamx git commit -m <msg>` — local commit (no network).
pub fn commit(message: &str, dir: Option<&str>) -> Result<Value, GitError> {
    let cwd = cwd_or(dir)?;
    let _ = read_origin(&cwd)?;
    git_in(&cwd, &["add", "-A"])?;
    let out = git_in(&cwd, &["commit", "-m", message])?;
    Ok(json!({ "ok": true, "message": out }))
}

/// `teamx git commit-push -m <msg>` — commit then push.
pub fn commit_push(
    server_url: &str,
    message: &str,
    repo: Option<&str>,
    branch: Option<&str>,
    team: Option<&str>,
    dir: Option<&str>,
) -> Result<Value, GitError> {
    let cwd = cwd_or(dir)?;
    let origin = read_origin(&cwd)?;
    let repo = repo.unwrap_or(&origin.repo).to_string();
    let commit_msg = commit(message, Some(cwd.to_str().unwrap_or_default()))?.get("message").cloned();
    let push_msg = push(server_url, &repo, branch, team, Some(cwd.to_str().unwrap_or_default()))?
        .get("message").cloned();
    Ok(json!({
        "ok": true,
        "commit": commit_msg,
        "push": push_msg,
        "repo": repo,
    }))
}

/// `teamx git list [--team <id>]`.
pub fn list(server_url: &str, team: Option<&str>) -> Result<Value, GitError> {
    let team = resolve_team(server_url, team)?;
    let resp = rpc(server_url, "git.repos", json!({ "team": team }))?;
    if resp.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(GitError(resp.to_string()));
    }
    Ok(resp)
}

/// `teamx git create <name> [--description <desc>]`.
pub fn create(server_url: &str, name: &str, description: Option<&str>, team: Option<&str>) -> Result<Value, GitError> {
    let team = resolve_team(server_url, team)?;
    let mut args = json!({ "team": team, "name": name });
    if let Some(d) = description {
        args["description"] = json!(d);
    }
    let resp = rpc(server_url, "git.create", args)?;
    if resp.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(GitError(resp.to_string()));
    }
    Ok(resp)
}

/// `teamx git delete <name>`.
pub fn delete(server_url: &str, name: &str, team: Option<&str>) -> Result<Value, GitError> {
    let team = resolve_team(server_url, team)?;
    let resp = rpc(server_url, "git.delete", json!({ "team": team, "name": name }))?;
    if resp.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(GitError(resp.to_string()));
    }
    Ok(resp)
}

/// `teamx git grant <name> <member_id> [--permission <read|write|admin>]`.
pub fn grant(
    server_url: &str,
    name: &str,
    member_id: &str,
    permission: &str,
    team: Option<&str>,
) -> Result<Value, GitError> {
    let team = resolve_team(server_url, team)?;
    let resp = rpc(
        server_url,
        "git.grant",
        json!({ "team": team, "name": name, "member_id": member_id, "permission": permission }),
    )?;
    if resp.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(GitError(resp.to_string()));
    }
    Ok(resp)
}

/// `teamx git permissions <name>`.
pub fn permissions(server_url: &str, name: &str, team: Option<&str>) -> Result<Value, GitError> {
    let team = resolve_team(server_url, team)?;
    let resp = rpc(server_url, "git.permissions", json!({ "team": team, "name": name }))?;
    if resp.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(GitError(resp.to_string()));
    }
    Ok(resp)
}
