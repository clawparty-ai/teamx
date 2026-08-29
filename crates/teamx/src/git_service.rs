//! Git service module for teamx server.
//!
//! Handles git repository operations and permissions on the server side.
//!
//! Repositories are stored as bare repos under `~/.teamx/repos/<team_id>/<name>.git`
//! and exchanged with clients as git bundles (base64 over RPC). The system `git`
//! binary does the heavy lifting — no libgit2 dependency.

use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db;

/// Repository storage root: `~/.teamx/repos`.
pub fn repos_root() -> PathBuf {
    crate::db::teamx_home().join("repos")
}

/// Validate a repository name: no path separators, no `..`, no whitespace.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name.contains(char::is_whitespace)
    {
        return Err(format!("invalid repository name `{name}`"));
    }
    Ok(())
}

/// On-disk bare repo directory for `(team_id, name)`.
pub fn repo_dir(team_id: &str, name: &str) -> PathBuf {
    repos_root().join(team_id).join(format!("{name}.git"))
}

/// Run a git command; returns stdout trimmed, or an error with stderr.
fn run_git(args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .output()
        .map_err(|e| format!("failed to run git: {e} (is git installed?)"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        return Err(format!("git {} failed: {}", args.join(" "), detail));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Initialize a bare repository on disk (idempotent).
pub fn init_bare_repo(team_id: &str, name: &str) -> Result<PathBuf, String> {
    validate_name(name)?;
    let dir = repo_dir(team_id, name);
    if !dir.exists() {
        std::fs::create_dir_all(dir.parent().unwrap_or(Path::new(".")))
            .map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
        run_git(&["init", "--bare", "--initial-branch=main", dir.to_str().unwrap_or_default()])?;
    }
    Ok(dir)
}

/// True when the bare repo has at least one ref (commit).
pub fn repo_has_commits(dir: &Path) -> bool {
    run_git(&["--git-dir", dir.to_str().unwrap_or_default(), "for-each-ref"])
        .map(|o| !o.trim().is_empty())
        .unwrap_or(false)
}

/// Default branch of a bare repo (from HEAD), falling back to `main`.
pub fn default_branch(dir: &Path) -> String {
    run_git(&["--git-dir", dir.to_str().unwrap_or_default(), "symbolic-ref", "--short", "HEAD"])
        .unwrap_or_else(|_| "main".to_string())
}

/// Create a full bundle (all refs) for the repo, base64-encoded.
/// Returns `(bundle_b64, default_branch)`. When the repo has no commits,
/// `bundle_b64` is empty and the caller should treat it as an empty repo.
pub fn create_bundle(team_id: &str, name: &str) -> Result<(String, String), String> {
    let dir = repo_dir(team_id, name);
    if !dir.exists() {
        return Err(format!("repository `{name}` not found"));
    }
    let branch = default_branch(&dir);
    if !repo_has_commits(&dir) {
        return Ok((String::new(), branch));
    }
    let tmp = std::env::temp_dir().join(format!("teamx-bundle-{}.bundle", Uuid::new_v4()));
    run_git(&[
        "--git-dir",
        dir.to_str().unwrap_or_default(),
        "bundle",
        "create",
        tmp.to_str().unwrap_or_default(),
        "--all",
    ])?;
    let bytes = std::fs::read(&tmp).map_err(|e| format!("read bundle: {e}"))?;
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
    let _ = std::fs::remove_file(&tmp);
    Ok((b64, branch))
}

/// Receive a git bundle file into the bare repo. Verifies the bundle, then
/// fetches every branch ref into the bare repo (non-fast-forward is rejected
/// by git).
pub fn receive_bundle(team_id: &str, name: &str, bundle_path: &Path) -> Result<(), String> {
    let dir = repo_dir(team_id, name);
    if !dir.exists() {
        return Err(format!("repository `{name}` not found"));
    }
    let dir_s = dir.to_str().unwrap_or_default();
    let bundle_s = bundle_path.to_str().unwrap_or_default();
    run_git(&["--git-dir", dir_s, "bundle", "verify", bundle_s])?;
    run_git(&["--git-dir", dir_s, "fetch", bundle_s, "+refs/heads/*:refs/heads/*"])?;
    Ok(())
}

/// Remove the on-disk bare repo directory.
pub fn remove_repo_dir(team_id: &str, name: &str) -> Result<(), String> {
    let dir = repo_dir(team_id, name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("remove {}: {e}", dir.display()))?;
    }
    Ok(())
}

/// Git repository information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitRepo {
    pub id: String,
    pub team_id: String,
    pub name: String,
    pub path: String,
    pub description: Option<String>,
    pub is_bare: bool,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Git repository permission
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitRepoPermission {
    pub id: i64,
    pub repo_id: String,
    pub member_id: String,
    pub permission: String,  // read, write, admin
    pub granted_by: String,
    pub granted_at: String,
}

/// Permission levels
pub const PERM_READ: &str = "read";
pub const PERM_WRITE: &str = "write";
pub const PERM_ADMIN: &str = "admin";

/// Create a new git repository (DB row + bare repo on disk).
pub fn create_repo(
    conn: &Connection,
    team_id: &str,
    name: &str,
    description: Option<&str>,
    created_by: &str,
) -> Result<GitRepo, String> {
    validate_name(name)?;
    // Physical bare repo first, so the DB row only exists when disk is ready.
    let disk_path = init_bare_repo(team_id, name)?;
    let id = Uuid::new_v4().to_string();
    let now = db::now();
    let path = disk_path.display().to_string();
    let is_bare = 1;

    conn.execute(
        "INSERT INTO git_repos (id, team_id, name, path, description, is_bare, created_by, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![id, team_id, name, path, description, is_bare, created_by, now, now],
    )
    .map_err(|e| {
        let _ = remove_repo_dir(team_id, name);
        format!("db error: {e}")
    })?;

    // Grant admin permission to the creator
    grant_permission(conn, &id, created_by, PERM_ADMIN, created_by)?;

    Ok(GitRepo {
        id,
        team_id: team_id.to_string(),
        name: name.to_string(),
        path,
        description: description.map(|s| s.to_string()),
        is_bare: is_bare == 1,
        created_by: created_by.to_string(),
        created_at: now.clone(),
        updated_at: now,
    })
}

/// Get a git repository by name
pub fn get_repo(
    conn: &Connection,
    team_id: &str,
    name: &str,
) -> Result<Option<GitRepo>, String> {
    let result = conn
        .query_row(
            "SELECT id, team_id, name, path, description, is_bare, created_by, created_at, updated_at
             FROM git_repos WHERE team_id = ?1 AND name = ?2",
            params![team_id, name],
            |row| {
                Ok(GitRepo {
                    id: row.get(0)?,
                    team_id: row.get(1)?,
                    name: row.get(2)?,
                    path: row.get(3)?,
                    description: row.get(4)?,
                    is_bare: row.get::<_, i64>(5)? == 1,
                    created_by: row.get(6)?,
                    created_at: row.get(7)?,
                    updated_at: row.get(8)?,
                })
            },
        )
        .optional()
        .map_err(|e| format!("db error: {e}"))?;

    Ok(result)
}

/// List all repositories for a team (owner/admin view; not wired to RPC yet).
#[allow(dead_code)]
pub fn list_repos(
    conn: &Connection,
    team_id: &str,
) -> Result<Vec<GitRepo>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, team_id, name, path, description, is_bare, created_by, created_at, updated_at
             FROM git_repos WHERE team_id = ?1 ORDER BY name",
        )
        .map_err(|e| format!("db error: {e}"))?;

    let repos = stmt
        .query_map(params![team_id], |row| {
            Ok(GitRepo {
                id: row.get(0)?,
                team_id: row.get(1)?,
                name: row.get(2)?,
                path: row.get(3)?,
                description: row.get(4)?,
                is_bare: row.get::<_, i64>(5)? == 1,
                created_by: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            })
        })
        .map_err(|e| format!("db error: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("db error: {e}"))?;

    Ok(repos)
}

/// Delete a git repository (DB row + bare repo on disk).
pub fn delete_repo(
    conn: &Connection,
    team_id: &str,
    name: &str,
) -> Result<(), String> {
    conn.execute(
        "DELETE FROM git_repos WHERE team_id = ?1 AND name = ?2",
        params![team_id, name],
    )
    .map_err(|e| format!("db error: {e}"))?;

    // Remove on-disk repo only after the DB row is gone.
    remove_repo_dir(team_id, name)?;

    Ok(())
}

/// Grant permission to a member for a repository
pub fn grant_permission(
    conn: &Connection,
    repo_id: &str,
    member_id: &str,
    permission: &str,
    granted_by: &str,
) -> Result<(), String> {
    let now = db::now();

    // Check if permission already exists
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM git_repo_permissions WHERE repo_id = ?1 AND member_id = ?2",
            params![repo_id, member_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("db error: {e}"))?;

    if let Some(id) = existing {
        // Update existing permission
        conn.execute(
            "UPDATE git_repo_permissions SET permission = ?1, granted_by = ?2, granted_at = ?3
             WHERE id = ?4",
            params![permission, granted_by, now, id],
        )
        .map_err(|e| format!("db error: {e}"))?;
    } else {
        // Insert new permission
        conn.execute(
            "INSERT INTO git_repo_permissions (repo_id, member_id, permission, granted_by, granted_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![repo_id, member_id, permission, granted_by, now],
        )
        .map_err(|e| format!("db error: {e}"))?;
    }

    Ok(())
}

/// Revoke permission from a member for a repository
#[allow(dead_code)]
pub fn revoke_permission(
    conn: &Connection,
    repo_id: &str,
    member_id: &str,
) -> Result<(), String> {
    conn.execute(
        "DELETE FROM git_repo_permissions WHERE repo_id = ?1 AND member_id = ?2",
        params![repo_id, member_id],
    )
    .map_err(|e| format!("db error: {e}"))?;

    Ok(())
}

/// List permissions for a repository
pub fn list_permissions(
    conn: &Connection,
    repo_id: &str,
) -> Result<Vec<GitRepoPermission>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, repo_id, member_id, permission, granted_by, granted_at
             FROM git_repo_permissions WHERE repo_id = ?1 ORDER BY granted_at",
        )
        .map_err(|e| format!("db error: {e}"))?;

    let perms = stmt
        .query_map(params![repo_id], |row| {
            Ok(GitRepoPermission {
                id: row.get(0)?,
                repo_id: row.get(1)?,
                member_id: row.get(2)?,
                permission: row.get(3)?,
                granted_by: row.get(4)?,
                granted_at: row.get(5)?,
            })
        })
        .map_err(|e| format!("db error: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("db error: {e}"))?;

    Ok(perms)
}

/// Directories never copied when seeding a repo from the current project.
const SKIP_DIRS: &[&str] = &[".git", ".teamx", "target", "node_modules", "vendor", "dist", ".build"];

/// Initialize the team's git repo with the contents of `source_dir` (the
/// project root the owner created the team from). Copies files into a temp
/// working copy, commits them, and pushes via bundle into the bare repo.
/// Returns the number of files seeded, or 0 if the source is empty.
pub fn seed_repo_from_dir(team_id: &str, name: &str, source_dir: &Path) -> Result<usize, String> {
    validate_name(name)?;
    let dir = repo_dir(team_id, name);
    if !dir.exists() {
        init_bare_repo(team_id, name)?;
    }

    // Prepare a temp working copy with the source files (excluding noise dirs).
    let work = std::env::temp_dir().join(format!("teamx-seed-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&work).map_err(|e| format!("mkdir {}: {e}", work.display()))?;
    let copied = copy_tree(source_dir, &work, 0);
    if copied == 0 {
        let _ = std::fs::remove_dir_all(&work);
        return Ok(0);
    }

    run_git(&["-C", work.to_str().unwrap_or_default(), "init", "-b", "main"])?;
    run_git(&["-C", work.to_str().unwrap_or_default(), "add", "-A"])?;
    // Best-effort identity; a commit needs user.name/email.
    run_git(&["-C", work.to_str().unwrap_or_default(), "-c", "user.name=teamx", "-c", "user.email=teamx@local", "commit", "-m", "initial import (teamx team create)"])?;

    let bundle = std::env::temp_dir().join(format!("teamx-seed-{}.bundle", Uuid::new_v4()));
    run_git(&["-C", work.to_str().unwrap_or_default(), "bundle", "create", bundle.to_str().unwrap_or_default(), "--all"])?;
    receive_bundle(team_id, name, &bundle)?;

    let _ = std::fs::remove_file(&bundle);
    let _ = std::fs::remove_dir_all(&work);
    Ok(copied)
}

/// Recursively copy `src` → `dst`, skipping SKIP_DIRS; returns file count.
fn copy_tree(src: &Path, dst: &Path, depth: usize) -> usize {
    let mut count = 0usize;
    let Ok(entries) = std::fs::read_dir(src) else { return 0 };
    for e in entries.flatten() {
        let from = e.path();
        let fname = e.file_name().to_string_lossy().to_string();
        if depth == 0 && SKIP_DIRS.contains(&fname.as_str()) {
            continue;
        }
        let to = dst.join(&fname);
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            if std::fs::create_dir_all(&to).is_ok() {
                count += copy_tree(&from, &to, depth + 1);
            }
        } else if e.file_type().map(|t| t.is_file()).unwrap_or(false) {
            if std::fs::copy(&from, &to).is_ok() {
                count += 1;
            }
        }
    }
    count
}

/// Convenience: sanitize a team/project name into a valid repo name
/// (lowercase, `-` for spaces, no path separators).
pub fn repo_name_from_team(name: &str) -> String {
    let s = name.trim().to_lowercase().replace(' ', "-");
    validate_name(&s).ok().map(|_| s.clone()).unwrap_or_else(|| format!("team-{}", Uuid::new_v4().simple()))
}

// ---------------------------------------------------------------------------
// Git Smart HTTP (standard git protocol over HTTPS/mTLS)
// ---------------------------------------------------------------------------
//
// These handlers implement the "smart" HTTP transport so any stock `git`
// client can `git clone https://server/git/<team>/<repo>` with mTLS client
// certs from the invitation letter. The server runs the standard plumbing
// commands (`git upload-pack` / `git receive-pack`) in --stateless-rpc mode.

use std::io::Write;
use std::process::{Command as ProcCommand, Stdio};

/// Result of a Smart HTTP RPC: raw bytes to send back plus a content type.
#[derive(Debug, Clone)]
pub struct SmartHttpResult {
    pub body: Vec<u8>,
    pub content_type: &'static str,
}

/// Handle `GET /git/<team>/<repo>/info/refs?service=...`.
/// `service` is `git-upload-pack` (fetch/clone) or `git-receive-pack` (push).
pub fn info_refs(team_id: &str, name: &str, service: &str) -> Result<SmartHttpResult, String> {
    validate_name(name)?;
    let dir = repo_dir(team_id, name);
    if !dir.exists() {
        return Err(format!("repository `{name}` not found"));
    }
    match service {
        "git-upload-pack" | "git-receive-pack" => {
            let plumbing = if service == "git-upload-pack" { "upload-pack" } else { "receive-pack" };
            let body = run_plumbing(&dir, plumbing, true)?;
            // Smart HTTP requires a service announcement preamble:
            //   `<len># service=<service>\n` followed by a flush-pkt (`0000`).
            // `git http-backend` normally prepends this; we do it ourselves.
            let announce = format!("# service={service}\n");
            let mut out = Vec::with_capacity(body.len() + 16);
            out.extend_from_slice(pkt_line(&announce).as_bytes());
            out.extend_from_slice(b"0000");
            out.extend_from_slice(&body);
            let content_type = if service == "git-upload-pack" {
                "application/x-git-upload-pack-advertisement"
            } else {
                "application/x-git-receive-pack-advertisement"
            };
            Ok(SmartHttpResult {
                body: out,
                content_type,
            })
        }
        other => Err(format!("unsupported service `{other}`")),
    }
}

/// Encode `s` as a pkt-line: 4-hex length + payload (length includes the 4
/// length bytes themselves).
fn pkt_line(s: &str) -> String {
    let len = s.len() + 4;
    format!("{:04x}{s}", len)
}

/// Handle `POST /git/<team>/<repo>/git-upload-pack` (clone/fetch/pull).
pub fn upload_pack(team_id: &str, name: &str, request_body: &[u8]) -> Result<SmartHttpResult, String> {
    validate_name(name)?;
    let dir = repo_dir(team_id, name);
    if !dir.exists() {
        return Err(format!("repository `{name}` not found"));
    }
    let body = run_plumbing_with_input(&dir, "upload-pack", request_body, false)?;
    Ok(SmartHttpResult {
        body,
        content_type: "application/x-git-upload-pack-result",
    })
}

/// Handle `POST /git/<team>/<repo>/git-receive-pack` (push).
pub fn receive_pack(team_id: &str, name: &str, request_body: &[u8]) -> Result<SmartHttpResult, String> {
    validate_name(name)?;
    let dir = repo_dir(team_id, name);
    if !dir.exists() {
        return Err(format!("repository `{name}` not found"));
    }
    let body = run_plumbing_with_input(&dir, "receive-pack", request_body, false)?;
    Ok(SmartHttpResult {
        body,
        content_type: "application/x-git-receive-pack-result",
    })
}

/// Run `git <plumbing> --stateless-rpc --advertise-refs <dir>` (info/refs).
fn run_plumbing(dir: &Path, plumbing: &str, advertise: bool) -> Result<Vec<u8>, String> {
    let dir_s = dir.to_str().unwrap_or_default();
    let mut cmd = ProcCommand::new("git");
    cmd.arg(plumbing).arg("--stateless-rpc");
    if advertise {
        cmd.arg("--advertise-refs");
    }
    cmd.arg(dir_s)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    run_proc(cmd, &[])
}

/// Run `git <plumbing> --stateless-rpc <dir>` feeding `input` on stdin.
fn run_plumbing_with_input(dir: &Path, plumbing: &str, input: &[u8], advertise: bool) -> Result<Vec<u8>, String> {
    let dir_s = dir.to_str().unwrap_or_default();
    let mut cmd = ProcCommand::new("git");
    cmd.arg(plumbing).arg("--stateless-rpc");
    if advertise {
        cmd.arg("--advertise-refs");
    }
    cmd.arg(dir_s)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    run_proc(cmd, input)
}

/// Spawn a git process, write stdin, capture stdout; surface stderr on failure.
fn run_proc(mut cmd: ProcCommand, input: &[u8]) -> Result<Vec<u8>, String> {
    let mut child = cmd.spawn().map_err(|e| format!("spawn git: {e} (is git installed?)"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input);
        // Close stdin so the child sees EOF and terminates.
        drop(stdin);
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("wait git: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!("git {} failed: {}", cmd.get_program().to_string_lossy(), err));
    }
    Ok(out.stdout)
}

/// Check if a member has permission for a repository
pub fn check_permission(
    conn: &Connection,
    repo_id: &str,
    member_id: &str,
    required_permission: &str,
) -> Result<bool, String> {
    let perm: Option<String> = conn
        .query_row(
            "SELECT permission FROM git_repo_permissions WHERE repo_id = ?1 AND member_id = ?2",
            params![repo_id, member_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("db error: {e}"))?;

    match perm {
        Some(p) => {
            // Check if the member has the required permission level
            match required_permission {
                PERM_READ => Ok(p == PERM_READ || p == PERM_WRITE || p == PERM_ADMIN),
                PERM_WRITE => Ok(p == PERM_WRITE || p == PERM_ADMIN),
                PERM_ADMIN => Ok(p == PERM_ADMIN),
                _ => Ok(false),
            }
        }
        None => Ok(false),
    }
}

/// Get the list of repos accessible to a member
pub fn list_accessible_repos(
    conn: &Connection,
    team_id: &str,
    member_id: &str,
) -> Result<Vec<GitRepo>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT r.id, r.team_id, r.name, r.path, r.description, r.is_bare, 
                    r.created_by, r.created_at, r.updated_at
             FROM git_repos r
             INNER JOIN git_repo_permissions p ON r.id = p.repo_id
             WHERE r.team_id = ?1 AND p.member_id = ?2
             ORDER BY r.name",
        )
        .map_err(|e| format!("db error: {e}"))?;

    let repos = stmt
        .query_map(params![team_id, member_id], |row| {
            Ok(GitRepo {
                id: row.get(0)?,
                team_id: row.get(1)?,
                name: row.get(2)?,
                path: row.get(3)?,
                description: row.get(4)?,
                is_bare: row.get::<_, i64>(5)? == 1,
                created_by: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            })
        })
        .map_err(|e| format!("db error: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("db error: {e}"))?;

    Ok(repos)
}
