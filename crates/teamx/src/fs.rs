//! fs.rs — `teamx fs`: a small member-node file service for the console's
//! File browser.
//!
//! Binds 127.0.0.1 and exposes the workspace (the directory it is started
//! from) with four operations:
//!   GET  /api/list?path=<rel>            -> { path, entries: [{name,type,size,modified}] }
//!   GET  /api/read?path=<rel>[&max=..]   -> raw text preview (default cap 1 MiB)
//!   GET  /api/download?path=<rel>        -> raw file bytes (attachment)
//!   POST /api/upload?dir=<rel>&name=<n>  -> write request body to dir/name
//!
//! Security: loopback only (reachable via the teamx tunnel + per-user ACL);
//! every path must resolve inside the workspace root; `.teamx`/`.git` and
//! escaping paths are refused so credentials are never exposed.

use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use axum::{
    body::{to_bytes as body_to_bytes, Body},
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;

use crate::cli::FsCmd;

const MAX_UPLOAD: usize = 20 * 1024 * 1024;
const MAX_PREVIEW: usize = 1024 * 1024;
const DENIED_SEGMENTS: [&str; 2] = [".teamx", ".git"];

type S = Arc<FsState>;

struct FsState {
    root: PathBuf,
}

#[derive(Deserialize)]
struct Q {
    path: Option<String>,
    dir: Option<String>,
    name: Option<String>,
    #[serde(rename = "max")]
    max_: Option<usize>,
}

pub fn run(cmd: &FsCmd) -> Result<(), String> {
    crate::procwatch::spawn_parent_monitor();

    let root = std::env::current_dir().map_err(|e| format!("cwd: {e}"))?;
    let state: S = Arc::new(FsState { root: root.clone() });

    let app = Router::new()
        .route("/api/list", get(api_list))
        .route("/api/read", get(api_read))
        .route("/api/download", get(api_download))
        .route("/api/upload", axum::routing::post(api_upload))
        .with_state(state);

    let addr_str = format!("{}:{}", cmd.addr, cmd.port);
    let addr: SocketAddr = addr_str.parse().map_err(|e| format!("bad bind {addr_str}: {e}"))?;

    eprintln!("teamx fs: file service");
    eprintln!("  root: {}", root.display());
    eprintln!("  listen: http://{}", addr);
    eprintln!("  endpoints: /api/list /api/read /api/download /api/upload");
    eprintln!("  Ctrl+C to stop");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
    rt.block_on(async {
        axum_server::bind(addr)
            .serve(app.into_make_service())
            .await
            .map_err(|e| format!("server error: {e}"))
    })
}

/// Resolve a client-supplied relative path inside the root.
fn resolve(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel = if rel.is_empty() || rel == "/" { "." } else { rel.trim_start_matches('/') };
    let cand = root.join(rel);
    // Reject escaping (`..`) and hidden credential dirs.
    for c in cand.components() {
        match c {
            Component::ParentDir => return Err("path escapes workspace".to_string()),
            Component::Normal(s) => {
                let s = s.to_string_lossy();
                if DENIED_SEGMENTS.contains(&s.as_ref()) {
                    return Err("path not allowed (hidden/credential directory)".to_string());
                }
            }
            _ => {}
        }
    }
    if !cand.starts_with(root) {
        return Err("path escapes workspace".to_string());
    }
    // Resolve symlinks so a link inside the workspace cannot point outside it.
    let rootc = std::fs::canonicalize(root).map_err(|e| format!("root: {e}"))?;
    let canon = std::fs::canonicalize(&cand).map_err(|e| format!("resolve {}: {e}", cand.display()))?;
    if !canon.starts_with(&rootc) {
        return Err("path escapes workspace (symlink)".to_string());
    }
    Ok(canon)
}

fn entry(p: &Path) -> serde_json::Value {
    let md = std::fs::metadata(p).ok();
    let is_dir = md.as_ref().map(|m| m.is_dir()).unwrap_or(false);
    json!({
        "name": p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        "type": if is_dir { "directory" } else { "file" },
        "size": md.as_ref().map(|m| m.len()).unwrap_or(0),
        "modified": md.as_ref().and_then(|m| m.modified().ok()).map(|t| {
            t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
        }).unwrap_or(0),
    })
}

async fn api_list(State(state): State<S>, Query(q): Query<Q>) -> Response {
    let rel = q.path.unwrap_or_else(|| ".".into());
    let dir = match resolve(&state.root, &rel) {
        Ok(p) => p,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": e }))).into_response(),
    };
    if !dir.is_dir() {
        return (StatusCode::NOT_FOUND, Json(json!({ "ok": false, "error": "not a directory" }))).into_response();
    }
    let mut entries: Vec<serde_json::Value> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            if DENIED_SEGMENTS.contains(&name.as_str()) {
                continue;
            }
            entries.push(entry(&p));
        }
    }
    entries.sort_by(|a, b| {
        let ad = a["type"].as_str() == Some("directory");
        let bd = b["type"].as_str() == Some("directory");
        bd.cmp(&ad).then(a["name"].as_str().cmp(&b["name"].as_str()))
    });
    Json(json!({ "ok": true, "path": rel, "entries": entries })).into_response()
}

async fn api_read(State(state): State<S>, Query(q): Query<Q>) -> Response {
    let rel = q.path.unwrap_or_default();
    let file = match resolve(&state.root, &rel) {
        Ok(p) => p,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": e }))).into_response(),
    };
    if !file.is_file() {
        return (StatusCode::NOT_FOUND, Json(json!({ "ok": false, "error": "not a file" }))).into_response();
    }
    let max = q.max_.unwrap_or(MAX_PREVIEW);
    let data = match std::fs::read(&file) {
        Ok(mut d) => {
            let truncated = d.len() > max;
            d.truncate(max);
            (d, truncated)
        }
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "ok": false, "error": e.to_string() }))).into_response(),
    };
    let (bytes, truncated) = data;
    let text = String::from_utf8_lossy(&bytes).to_string();
    Json(json!({ "ok": true, "path": rel, "truncated": truncated, "content": text })).into_response()
}

async fn api_download(State(state): State<S>, Query(q): Query<Q>) -> Response {
    let rel = q.path.unwrap_or_default();
    let file = match resolve(&state.root, &rel) {
        Ok(p) => p,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": e }))).into_response(),
    };
    if !file.is_file() {
        return (StatusCode::NOT_FOUND, Json(json!({ "ok": false, "error": "not a file" }))).into_response();
    }
    let bytes = match std::fs::read(&file) {
        Ok(b) => b,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "ok": false, "error": e.to_string() }))).into_response(),
    };
    let name = file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "download".into());
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\"").as_str()),
        ],
        bytes,
    )
        .into_response()
}

async fn api_upload(
    State(state): State<S>,
    Query(q): Query<Q>,
    body: Body,
) -> Response {
    let dir_rel = q.dir.unwrap_or_else(|| ".".into());
    let name = match q.name {
        Some(n) if !n.is_empty() && !n.contains('/') && !n.contains("..") => n,
        _ => return (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": "bad name" }))).into_response(),
    };
    let dir = match resolve(&state.root, &dir_rel) {
        Ok(p) => p,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": e }))).into_response(),
    };
    if !dir.is_dir() {
        return (StatusCode::NOT_FOUND, Json(json!({ "ok": false, "error": "dir not found" }))).into_response();
    }
    if DENIED_SEGMENTS.contains(&name.as_str()) {
        return (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": "name not allowed" }))).into_response();
    }
    let bytes = match body_to_bytes(body, MAX_UPLOAD).await {
        Ok(b) => b,
        Err(_) => return (StatusCode::PAYLOAD_TOO_LARGE, Json(json!({ "ok": false, "error": "upload too large" }))).into_response(),
    };
    let target = dir.join(&name);
    if let Err(e) = std::fs::write(&target, &bytes) {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "ok": false, "error": e.to_string() }))).into_response();
    }
    Json(json!({ "ok": true, "path": dir.join(&name).display().to_string(), "size": bytes.len() })).into_response()
}

