//! console.rs — `teamx console`: member web console (local, view-only).
//!
//! Runs from a workspace and serves a browser console branded "Teamx Console"
//! over HTTPS on 127.0.0.1 with a random per-launch token (same UX as
//! `teamx ui`). Data comes from the same CLI/RPC calls the workspace would
//! make; the console never writes team state. The only file it writes is the
//! cloud-node list (`.teamx/console.json`).
//!
//! Cloud panel: entries point at local ports the user has already `tunnel
//! forward`ed (headless opencode / ttyd). The console proxies them same-origin
//! (`/api/cloud/{id}/...`) so iframes work under the HTTPS console and, for
//! terminal apps like ttyd, relays the WebSocket upgrade.

use std::sync::Arc;

use axum::{
    body::{to_bytes as body_to_bytes, Body, Bytes},
    extract::{
        ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::{header, HeaderMap, Method, Request, Response, StatusCode},
    response::{sse::{Event as SseEvent, KeepAlive, Sse}, Html, IntoResponse},
    routing::{delete, get, post},
    Json, Router,
};
use axum_server::tls_rustls::RustlsConfig;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;

use crate::cli::ConsoleCmd;
use crate::console_data::{
    config_path, discussion_action, fetch_me, fetch_panel, load_config, resolve_mode, save_config, slugify,
    valid_node_url, Ctx, Mode, RunOut,
};
use crate::console_html::{
    login_html, not_found_html, page_html, print_discussion_html, unauthorized_html,
};
use crate::pki;

/// Cookie name holding the auth token.
const COOKIE: &str = "teamx_console_token";

/// Entry point. Blocks until the server stops.
pub fn run(cmd: &ConsoleCmd) -> Result<(), String> {
    // When launched by the opencode plugin, exit (and clean up tunnel
    // forwards) as soon as opencode goes away.
    crate::procwatch::spawn_parent_monitor();

    let home = crate::db::teamx_home();
    // 允许外部固定 token（云 member Pod 由平台下发 → 平台可据此生成可点击链接）；
    // 未设置时按原逻辑随机生成（本地用法不变）。
    let token = std::env::var("TEAMX_CONSOLE_TOKEN")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| crate::util::gen_access_token(16));

    // HTTPS with the workspace's instance server cert (same as `teamx ui`).
    let pk = pki::ensure_pki(&home)?;
    let server_cert_pem = std::fs::read(&pk.server_cert).map_err(|e| format!("read server cert: {e}"))?;
    let server_key_pem = std::fs::read(&pk.server_key).map_err(|e| format!("read server key: {e}"))?;
    let cert_chain = rustls_pemfile::certs(&mut server_cert_pem.as_slice())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("parse server cert chain: {e}"))?;
    let key_der = rustls_pemfile::private_key(&mut server_key_pem.as_slice())
        .map_err(|e| format!("parse server key: {e}"))?
        .ok_or_else(|| "no private key in server.key".to_string())?;
    let tls_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key_der)
        .map_err(|e| format!("tls config: {e}"))?;

    // Data mode: network (server URL discovered) or local (workspace DB).
    let mode = resolve_mode(cmd.server_url.as_deref());
    let cfg_path = config_path(&home, &cmd.config);
    let session = cmd.session.clone().or_else(|| {
        crate::console_data::pick_local_session(&home)
            .or_else(|| std::env::var("TEAMX_SESSION").ok().filter(|s| !s.is_empty()))
    });
    let ctx = Ctx {
        mode,
        session,
        home: home.clone(),
        config_path: cfg_path.clone(),
    };

    let state = Arc::new(StateA {
        token: token.clone(),
        ctx: ctx.clone(),
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/login", get(login_page))
        // Chrome-free thread view opened in a new tab, then printed to PDF by
        // the browser ("Download PDF" in the discussion toolbar).
        .route("/print/discussion", get(print_discussion_page))
        .route("/api/panels/{id}", get(api_panel))
        .route("/api/stream", get(api_stream))
        .route("/api/discussion/{action}", post(api_discussion))
        .route("/api/me", get(api_me))
        .route("/api/file", get(api_file))
        .route("/api/repos", get(api_repos))
        .route("/api/repos/clone", post(api_repos_clone))
        .route("/api/repos/clone/status", get(api_repos_clone_status))
        .route("/api/cloud", get(api_cloud_get).post(api_cloud_post))
        .route(
            "/api/cloud/{id}",
            delete(api_cloud_delete).get(api_cloud_proxy_root),
        )
        .route("/api/cloud/{id}/", get(api_cloud_proxy_root))
        .route("/api/cloud/{id}/ws", get(api_cloud_ws))
        .route("/api/cloud/{id}/{*path}", get(api_cloud_proxy_rest))
        .route("/api/tunnels/forward", post(api_tunnel_forward))
        .fallback(not_found_page)
        .with_state(state);

    let bind_str = if cmd.addr.contains(':') {
        format!("[{}]:{}", cmd.addr, cmd.port)
    } else {
        format!("{}:{}", cmd.addr, cmd.port)
    };
    let addr: std::net::SocketAddr = bind_str
        .parse()
        .map_err(|e| format!("invalid bind address {bind_str}: {e}"))?;

    match &ctx.mode {
        Mode::Network(url) => eprintln!("teamx console: member web console (network mode, server {url})"),
        Mode::Local => eprintln!("teamx console: member web console (project/local mode)"),
    }
    eprintln!("  open: https://{}/?token={}", addr, token);
    eprintln!("  workspace: {}", home.display());
    eprintln!("  (self-signed cert; your browser will warn about it once)");
    eprintln!("  Ctrl+C to stop");

    let config = RustlsConfig::from_config(Arc::new(tls_config));
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
    rt.block_on(async {
        axum_server::bind_rustls(addr, config)
            .serve(app.into_make_service())
            .await
            .map_err(|e| format!("server error: {e}"))
    })
}

type S = Arc<StateA>;

struct StateA {
    token: String,
    ctx: Ctx,
}

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

fn authorized(state: &S, cookies: Option<&str>) -> bool {
    match cookies {
        Some(c) => c.split(';').any(|kv| {
            let mut it = kv.trim().splitn(2, '=');
            it.next() == Some(COOKIE) && it.next() == Some(&state.token)
        }),
        None => false,
    }
}

async fn index(State(state): State<S>, headers: HeaderMap, request: Request<Body>) -> axum::response::Response {
    // ?token=... sets the cookie and redirects to /.
    let query = request.uri().query().unwrap_or("").to_string();
    if let Some(t) = query_token(&query) {
        if t == state.token {
            let mut resp = axum::response::Redirect::to("/").into_response();
            let val = format!("{COOKIE}={}; HttpOnly; SameSite=Strict; Path=/", state.token);
            resp.headers_mut().insert(header::SET_COOKIE, val.parse().unwrap());
            return resp;
        }
    }
    if authorized(&state, headers.get(header::COOKIE).and_then(|v| v.to_str().ok())) {
        return Html(page_html()).into_response();
    }
    axum::response::Redirect::to("/login").into_response()
}

fn query_token(query: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let mut it = kv.splitn(2, '=');
        if it.next() == Some("token") {
            it.next().map(|v| v.replace("+", " ").to_string())
        } else {
            None
        }
    })
}

async fn login_page() -> Html<String> {
    Html(login_html())
}

async fn not_found_page() -> (StatusCode, Html<String>) {
    (StatusCode::NOT_FOUND, Html(not_found_html()))
}

/// GET /print/discussion?id=<id> — the print/PDF view of one thread.
///
/// A page, not an API: unauthenticated requests bounce to `/login` like `/`
/// does, rather than getting the bare 401 `guard` hands to `/api/*`. The page
/// then fetches the thread through `/api/panels/discussion` with the cookie, so
/// no thread content is ever embedded in the served HTML.
async fn print_discussion_page(
    State(state): State<S>,
    headers: HeaderMap,
) -> axum::response::Response {
    if authorized(&state, headers.get(header::COOKIE).and_then(|v| v.to_str().ok())) {
        return Html(print_discussion_html()).into_response();
    }
    axum::response::Redirect::to("/login").into_response()
}

#[allow(clippy::result_large_err)]
fn guard(state: &S, headers: &HeaderMap) -> Result<(), axum::response::Response> {
    if authorized(state, headers.get(header::COOKIE).and_then(|v| v.to_str().ok())) {
        Ok(())
    } else {
        Err((StatusCode::UNAUTHORIZED, Html(unauthorized_html())).into_response())
    }
}

fn err_json(code: StatusCode, msg: String) -> axum::response::Response {
    (code, Json(json!({ "ok": false, "error": msg }))).into_response()
}

// ---------------------------------------------------------------------------
// Panels
// ---------------------------------------------------------------------------

async fn api_panel(
    State(state): State<S>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    if let Err(resp) = guard(&state, &headers) {
        return resp;
    }
    let ctx = state.ctx.clone();
    let id = id.clone();
    let query_id = params.get("id").cloned();
    let out: RunOut = tokio::task::spawn_blocking(move || fetch_panel(&ctx, &id, query_id.as_deref()))
        .await
        .unwrap_or_else(|_| RunOut {
            ok: false,
            mode: "",
            cmd: "panel".to_string(),
            error: Some("panel task panicked".to_string()),
            data: None,
            elapsed_ms: 0,
        });
    (StatusCode::OK, Json(out)).into_response()
}

/// GET /api/stream — Server-Sent Events relay of the team's ledger events.
///
/// Network mode: the console polls the serve `/rpc events` every ~2s (using the
/// workspace member's mTLS identity) and forwards new events to the browser as
/// SSE, so Discussion / Events views update without page polling. The initial
/// poll only snapshots the current seq (no history replay). Local mode emits
/// only heartbeats (no live channel).
/// A single `bytes=` range from the request. Multi-range requests are ignored
/// (the whole file is served) — `<video>` only ever asks for one range.
#[derive(Clone, Copy)]
enum ByteRange {
    /// `bytes=<start>-`
    From(u64),
    /// `bytes=-<suffix>` (last N bytes)
    Suffix(u64),
    /// `bytes=<start>-<end>` (inclusive)
    Span(u64, u64),
}

impl ByteRange {
    /// Resolve to an inclusive `(start, end)` for a file of `len` bytes.
    fn resolve(self, len: u64) -> Result<(u64, u64), FileErr> {
        if len == 0 {
            return Err(FileErr::Unsatisfiable(0));
        }
        let last = len - 1;
        let (start, end) = match self {
            ByteRange::From(s) => (s, last),
            ByteRange::Suffix(n) => (len.saturating_sub(n), last),
            ByteRange::Span(s, e) => (s, e.min(last)),
        };
        if start > end || start >= len {
            return Err(FileErr::Unsatisfiable(len));
        }
        Ok((start, end))
    }
}

fn parse_range(v: &str) -> Option<ByteRange> {
    let spec = v.strip_prefix("bytes=")?.trim();
    if spec.contains(',') {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let (a, b) = (a.trim(), b.trim());
    if a.is_empty() {
        return b.parse::<u64>().ok().map(ByteRange::Suffix);
    }
    let start = a.parse::<u64>().ok()?;
    if b.is_empty() {
        return Some(ByteRange::From(start));
    }
    b.parse::<u64>().ok().map(|e| ByteRange::Span(start, e))
}

enum FileErr {
    /// Range cannot be satisfied → 416 with `Content-Range: bytes */<len>`.
    Unsatisfiable(u64),
    Other(String),
}

struct FileOut {
    bytes: Vec<u8>,
    name: String,
    len: u64,
    start: u64,
    end: u64,
    partial: bool,
}

/// GET /api/file?path=<rel> — serve one workspace file.
///
/// Backs every inline preview in the File panel (`<iframe>` for PDF/HTML,
/// `<img>` for images, `<video>` for video) plus the "download original"
/// links. Supports single-range `Range: bytes=` requests so a `<video>` can
/// seek without the whole file being read into memory.
async fn api_file(
    State(state): State<S>,
    headers: HeaderMap,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    if let Err(resp) = guard(&state, &headers) {
        return resp;
    }
    let Some(rel) = params.get("path").cloned().filter(|s| !s.is_empty()) else {
        return err_json(StatusCode::BAD_REQUEST, "file: `path` is required".to_string());
    };
    let wanted = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_range);
    let ctx = state.ctx.clone();
    let out = tokio::task::spawn_blocking(move || -> Result<FileOut, FileErr> {
        let root = crate::console_data::workspace_root(&ctx);
        let file = crate::fs::resolve_with(&root, &rel, crate::fs::PathPolicy::Console)
            .map_err(FileErr::Other)?;
        if !file.is_file() {
            return Err(FileErr::Other(format!("`{rel}` is not a file")));
        }
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "download".to_string());
        let len = std::fs::metadata(&file)
            .map_err(|e| FileErr::Other(e.to_string()))?
            .len();
        let (start, end) = match wanted {
            Some(r) => r.resolve(len)?,
            None => (0, len.saturating_sub(1)),
        };
        let n = if len == 0 { 0 } else { end - start + 1 };
        let mut bytes = vec![0u8; n as usize];
        if n > 0 {
            use std::io::{Read, Seek, SeekFrom};
            let mut f = std::fs::File::open(&file).map_err(|e| FileErr::Other(e.to_string()))?;
            f.seek(SeekFrom::Start(start))
                .map_err(|e| FileErr::Other(e.to_string()))?;
            f.read_exact(&mut bytes)
                .map_err(|e| FileErr::Other(e.to_string()))?;
        }
        Ok(FileOut {
            bytes,
            name,
            len,
            start,
            end,
            partial: wanted.is_some(),
        })
    })
    .await;
    match out {
        Ok(Ok(f)) => {
            let mut resp = Response::new(Body::from(f.bytes));
            if f.partial {
                *resp.status_mut() = StatusCode::PARTIAL_CONTENT;
            }
            let h = resp.headers_mut();
            h.insert(
                header::CONTENT_TYPE,
                content_type_for(&f.name).parse().unwrap(),
            );
            h.insert(header::ACCEPT_RANGES, "bytes".parse().unwrap());
            // Script-capable documents (HTML / SVG) are sandboxed so a workspace
            // file cannot script the console's own origin. See `inline_csp`.
            if let Some(csp) = inline_csp(&f.name) {
                h.insert(
                    axum::http::HeaderName::from_static("content-security-policy"),
                    csp.parse().unwrap(),
                );
            }
            let disp = format!(
                "{}; filename=\"{}\"",
                if is_inline_type(&f.name) { "inline" } else { "attachment" },
                f.name.replace('"', "")
            );
            h.insert(header::CONTENT_DISPOSITION, disp.parse().unwrap());
            if f.partial {
                h.insert(
                    header::CONTENT_RANGE,
                    format!("bytes {}-{}/{}", f.start, f.end, f.len).parse().unwrap(),
                );
            }
            resp
        }
        Ok(Err(FileErr::Unsatisfiable(len))) => {
            let mut resp = err_json(
                StatusCode::RANGE_NOT_SATISFIABLE,
                "requested range is not satisfiable".to_string(),
            );
            resp.headers_mut().insert(
                header::CONTENT_RANGE,
                format!("bytes */{len}").parse().unwrap(),
            );
            resp
        }
        Ok(Err(FileErr::Other(e))) => err_json(StatusCode::BAD_REQUEST, e),
        Err(_) => err_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            "file task panicked".to_string(),
        ),
    }
}

/// Whether a file may be rendered **inline** by the browser (rather than
/// downloaded). Widened to cover images, video and HTML/ SVG, which are all
/// sandboxed by `inline_csp()` — see its docs for why that is what keeps this
/// list XSS-safe.
fn is_inline_type(name: &str) -> bool {
    matches!(
        content_type_for(name),
        "application/pdf"
            | "image/png"
            | "image/jpeg"
            | "image/gif"
            | "image/webp"
            | "image/bmp"
            | "image/x-icon"
            | "image/avif"
            | "image/svg+xml"
            | "video/mp4"
            | "video/webm"
            | "video/quicktime"
            | "video/ogg"
            | "text/plain; charset=utf-8"
            | "text/csv; charset=utf-8"
            | "text/html; charset=utf-8"
            | "application/json"
    )
}

/// `Content-Security-Policy` for inline responses that can carry script:
/// HTML and SVG. Both are rendered by the browser as documents when navigated
/// to directly, so a workspace file could otherwise run script in the console's
/// origin and drive its authenticated `/api/*` routes.
///
/// `sandbox` (with no `allow-same-origin`) puts the document in an opaque
/// origin: scripts may still run for HTML previews, but they cannot read the
/// console cookie (which is also `HttpOnly`) nor read back same-origin
/// responses. SVG gets no `allow-scripts` at all — its preview path is
/// `<img>`, where scripts never run anyway.
///
/// Returns `None` for every other type (nothing to sandbox).
fn inline_csp(name: &str) -> Option<&'static str> {
    match content_type_for(name) {
        "text/html; charset=utf-8" => Some("sandbox allow-scripts"),
        "image/svg+xml" => Some("sandbox"),
        _ => None,
    }
}

fn content_type_for(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "txt" | "md" | "markdown" => "text/plain; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "json" => "application/json",
        "html" | "htm" => "text/html; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "avif" => "image/avif",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "ogv" => "video/ogg",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

async fn api_stream(State(state): State<S>, headers: HeaderMap) -> axum::response::Response {
    if let Err(resp) = guard(&state, &headers) {
        return resp;
    }
    let ctx = state.ctx.clone();
    use futures_util::stream;
    use std::convert::Infallible;
    use std::time::Duration;

    // State: (ctx, last_seq, pending events queue). We drain `pending` first;
    // when empty, poll the serve for events after `last_seq`. Synchronous RPC
    // must run on a blocking thread (never inside async).
    let stream = stream::unfold((ctx, 0i64, Vec::<serde_json::Value>::new()), |(ctx, last, mut pending)| async move {
        // Helper: run the sync poll_team_events on a blocking task.
        async fn poll(ctx: &crate::console_data::Ctx, after: i64) -> Result<(Vec<serde_json::Value>, i64), String> {
            let ctx = ctx.clone();
            tokio::task::spawn_blocking(move || crate::console_data::poll_team_events(&ctx, after))
                .await
                .map_err(|e| e.to_string())?
        }
        // No `loop` here: stream::unfold re-invokes this future with the new
        // state after every `Some`, so this body already runs once per SSE
        // poll (a `loop` whose every path returns would be flagged
        // never_loop and is dead structure).
        if let Some(row) = pending.pop() {
            let seq = row.get("seq").and_then(|v| v.as_i64()).unwrap_or(0);
            let ev = SseEvent::default().event("event").json_data(row).unwrap_or_else(|_| SseEvent::default());
            return Some((ev, (ctx, seq.max(last), pending)));
        }
        if last == 0 {
            // first pass: snapshot latest seq only (no history replay)
            if let Ok((_hist, max)) = poll(&ctx, 0).await {
                return Some((heartbeat(), (ctx, max, pending)));
            }
        }
        tokio::time::sleep(Duration::from_millis(1500)).await;
        match poll(&ctx, last).await {
            Ok((rows, max)) => {
                if rows.is_empty() {
                    return Some((heartbeat(), (ctx, last, pending)));
                }
                let new_last = max;
                for r in rows.into_iter().rev() {
                    pending.push(r);
                }
                Some((heartbeat(), (ctx, new_last, pending)))
            }
            Err(e) => {
                eprintln!("teamx console: sse poll error: {e}");
                Some((heartbeat(), (ctx, last, pending)))
            }
        }
    });

    fn heartbeat() -> SseEvent {
        SseEvent::default().event("heartbeat").data("{}")
    }

    Sse::new(stream.filter_map(|o| async move { Some(Ok::<_, Infallible>(o)) }))
        .keep_alive(KeepAlive::default().interval(Duration::from_secs(20)))
        .into_response()
}

/// POST /api/discussion/{action} — create/say/reply/close a discussion thread.
#[derive(Deserialize)]
struct DiscussionBody {
    id: Option<String>,
    message_id: Option<String>,
    title: Option<String>,
    body: Option<String>,
    quotes: Option<serde_json::Value>,
}

async fn api_discussion(
    State(state): State<S>,
    headers: HeaderMap,
    axum::extract::Path(action): axum::extract::Path<String>,
    Json(payload): Json<DiscussionBody>,
) -> axum::response::Response {
    if let Err(resp) = guard(&state, &headers) {
        return resp;
    }
    let ctx = state.ctx.clone();
    let mut args = serde_json::Map::new();
    if let Some(v) = payload.id {
        args.insert("id".into(), json!(v));
    }
    if let Some(v) = payload.message_id {
        args.insert("message_id".into(), json!(v));
    }
    if let Some(v) = payload.title {
        args.insert("title".into(), json!(v));
    }
    if let Some(v) = payload.body {
        args.insert("body".into(), json!(v));
    }
    if let Some(v) = payload.quotes {
        args.insert("quotes".into(), v);
    }
    let args = serde_json::Value::Object(args);
    let out: RunOut = tokio::task::spawn_blocking(move || {
        let is_net = ctx.server_url().is_some();
        match discussion_action(&ctx, &action, &args) {
            Ok(data) => RunOut {
                ok: true,
                mode: if is_net { "rpc" } else { "cli" },
                cmd: format!("teamx discussion {action}"),
                error: None,
                data: Some(data),
                elapsed_ms: 0,
            },
            Err(e) => RunOut {
                ok: false,
                mode: if is_net { "rpc" } else { "cli" },
                cmd: format!("teamx discussion {action}"),
                error: Some(e),
                data: None,
                elapsed_ms: 0,
            },
        }
    })
    .await
    .unwrap_or_else(|_| RunOut {
        ok: false,
        mode: "",
        cmd: "discussion".to_string(),
        error: Some("discussion task panicked".to_string()),
        data: None,
        elapsed_ms: 0,
    });
    (StatusCode::OK, Json(out)).into_response()
}
async fn api_me(State(state): State<S>, headers: HeaderMap) -> axum::response::Response {
    if let Err(resp) = guard(&state, &headers) {
        return resp;
    }
    let ctx = state.ctx.clone();
    let out: RunOut = tokio::task::spawn_blocking(move || fetch_me(&ctx))
        .await
        .unwrap_or_else(|_| RunOut {
            ok: false,
            mode: "rpc",
            cmd: "teamx whoami".to_string(),
            error: Some("whoami task panicked".to_string()),
            data: None,
            elapsed_ms: 0,
        });
    (StatusCode::OK, Json(out)).into_response()
}

// ---------------------------------------------------------------------------
// Team repositories ("pull repo"): list + background clone + status polling
// ---------------------------------------------------------------------------

/// GET /api/repos — the repos the calling member can read, with a size and a
/// local-presence flag.
async fn api_repos(State(state): State<S>, headers: HeaderMap) -> axum::response::Response {
    if let Err(resp) = guard(&state, &headers) {
        return resp;
    }
    let ctx = state.ctx.clone();
    let out = tokio::task::spawn_blocking(move || crate::console_repos::list_repos(&ctx)).await;
    match out {
        Ok(Ok(v)) => (StatusCode::OK, Json(v)).into_response(),
        Ok(Err(e)) => err_json(StatusCode::BAD_GATEWAY, e),
        Err(_) => err_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            "repos task panicked".to_string(),
        ),
    }
}

#[derive(Deserialize)]
struct RepoCloneReq {
    /// Accepts `{name}` (or `{repo}` for convenience).
    name: Option<String>,
    repo: Option<String>,
}

/// POST /api/repos/clone `{name}` — start cloning `<name>` into the workspace.
///
/// The clone itself runs on a detached thread (an 11 GiB repo takes minutes);
/// this returns the initial `running` job immediately. Poll
/// `/api/repos/clone/status?name=` for progress/failure.
async fn api_repos_clone(
    State(state): State<S>,
    headers: HeaderMap,
    Json(req): Json<RepoCloneReq>,
) -> axum::response::Response {
    if let Err(resp) = guard(&state, &headers) {
        return resp;
    }
    let name = req.name.or(req.repo).unwrap_or_default().trim().to_string();
    if name.is_empty() {
        return err_json(StatusCode::BAD_REQUEST, "`name` is required".to_string());
    }
    let ctx = state.ctx.clone();
    let out =
        tokio::task::spawn_blocking(move || crate::console_repos::start_clone(&ctx, &name)).await;
    match out {
        Ok(Ok(v)) => (StatusCode::OK, Json(v)).into_response(),
        Ok(Err(e)) => err_json(StatusCode::BAD_REQUEST, e),
        Err(_) => err_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            "clone task panicked".to_string(),
        ),
    }
}

/// GET /api/repos/clone/status?name= — in-flight/finished clone state.
async fn api_repos_clone_status(
    State(state): State<S>,
    headers: HeaderMap,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    if let Err(resp) = guard(&state, &headers) {
        return resp;
    }
    let name = params.get("name").cloned().unwrap_or_default();
    if name.is_empty() {
        return err_json(StatusCode::BAD_REQUEST, "`name` is required".to_string());
    }
    match crate::console_repos::clone_status(&name) {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => err_json(StatusCode::BAD_REQUEST, e),
    }
}

// ---------------------------------------------------------------------------
// Cloud node list (.teamx/console.json)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CloudAdd {
    name: String,
    url: String,
}

async fn api_cloud_get(State(state): State<S>, headers: HeaderMap) -> axum::response::Response {
    if let Err(resp) = guard(&state, &headers) {
        return resp;
    }
    let cfg = load_config(&state.ctx.config_path);
    (StatusCode::OK, Json(cfg)).into_response()
}

async fn api_cloud_post(
    State(state): State<S>,
    headers: HeaderMap,
    Json(add): Json<CloudAdd>,
) -> axum::response::Response {
    if let Err(resp) = guard(&state, &headers) {
        return resp;
    }
    let name = add.name.trim().to_string();
    let url = add.url.trim().to_string();
    if name.is_empty() || url.is_empty() {
        return err_json(StatusCode::BAD_REQUEST, "name and url required".to_string());
    }
    if !valid_node_url(&url) {
        return err_json(
            StatusCode::BAD_REQUEST,
            "url must be a loopback address (http://127.0.0.1:<port> or http://localhost:<port>)"
                .to_string(),
        );
    }
    let path = state.ctx.config_path.clone();
    let mut cfg = load_config(&path);
    let id = slugify(&name);
    let kind = if name.to_ascii_lowercase().contains("ttyd") {
        "ttyd"
    } else if url.contains(":4199") || name.to_ascii_lowercase().contains("headless") {
        "headless"
    } else {
        "web"
    };
    // Upsert by id.
    if let Some(existing) = cfg.nodes.iter_mut().find(|n| n.id == id) {
        existing.name = name;
        existing.url = url;
        existing.kind = kind.to_string();
    } else {
        cfg.nodes.push(crate::console_data::CloudNode {
            id,
            name,
            kind: kind.to_string(),
            url,
        });
    }
    if let Err(e) = save_config(&path, &cfg) {
        return err_json(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    (StatusCode::OK, Json(cfg)).into_response()
}

async fn api_cloud_delete(
    State(state): State<S>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> axum::response::Response {
    if let Err(resp) = guard(&state, &headers) {
        return resp;
    }
    let path = state.ctx.config_path.clone();
    let mut cfg = load_config(&path);
    cfg.nodes.retain(|n| n.id != id);
    if let Err(e) = save_config(&path, &cfg) {
        return err_json(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    (StatusCode::OK, Json(cfg)).into_response()
}

// ---------------------------------------------------------------------------
// Tunnel auto-forward ("View"/"Open" from the Teams members list)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct TunFwdReq {
    name: String,
}

/// `POST /api/tunnels/forward` — start a local `teamx tunnel forward` for a
/// teammate's tunnel (idempotent) and register a loopback cloud node so the
/// Teams view can iframe it through `/api/cloud/{node_id}/`.
async fn api_tunnel_forward(
    State(state): State<S>,
    headers: HeaderMap,
    Json(req): Json<TunFwdReq>,
) -> axum::response::Response {
    if let Err(resp) = guard(&state, &headers) {
        return resp;
    }
    let name = req.name.trim().to_string();
    if name.is_empty() {
        return err_json(StatusCode::BAD_REQUEST, "tunnel name required".to_string());
    }
    let ctx = state.ctx.clone();
    let res = tokio::task::spawn_blocking(move || start_tunnel_forward(&ctx, &name))
        .await
        .unwrap_or_else(|_| Err("forward task panicked".to_string()));
    match res {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => err_json(StatusCode::BAD_GATEWAY, e),
    }
}

/// This session's spawned forwards: tunnel name -> local port. Reuse is only
/// trusted from here — never from the on-disk node list, whose recorded ports
/// may have been reused by a different tunnel in a later session.
static SESSION_FORWARDS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, u16>>,
> = std::sync::OnceLock::new();

fn session_forwards() -> &'static std::sync::Mutex<std::collections::HashMap<String, u16>> {
    SESSION_FORWARDS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Spawn (or reuse from THIS session) a local forward for `name`; returns JSON
/// describing the listening port and the node record.
fn start_tunnel_forward(ctx: &Ctx, name: &str) -> Result<serde_json::Value, String> {
    use std::net::TcpListener;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    // TLS probe (may run over a far-away tunnel; keep the timeout generous).
    let probe = |port: u16| -> bool {
        let home = ctx.home.clone();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok();
        rt.map(|r| r.block_on(is_tls_server(&home, "127.0.0.1", port)))
            .unwrap_or(false)
    };

    // Reuse only a forward THIS session spawned and that is still listening.
    let reused_port = session_forwards()
        .lock()
        .unwrap()
        .get(name)
        .copied()
        .filter(|&p| port_open(p));

    let port = match reused_port {
        Some(p) => p,
        None => {
            // Pick a free local port (probe by binding, then release).
            let mut port = 0u16;
            for candidate in 19001u16..19400 {
                if TcpListener::bind(("127.0.0.1", candidate)).is_ok() {
                    port = candidate;
                    break;
                }
            }
            if port == 0 {
                return Err("no free local port in 19001-19399".to_string());
            }
            port
        }
    };

    if reused_port.is_some() {
        let is_tls = probe(port);
        return finish_forward(ctx, name, port, is_tls, true);
    }

    // Spawn `teamx tunnel forward <name> --local-port <port> --session <s>`.
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let session = ctx.session_or("console").to_string();
    let mut cmd = Command::new(&exe);
    cmd.args([
        "tunnel",
        "forward",
        name,
        "--local-port",
        &port.to_string(),
        "--session",
        &session,
    ])
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    if let Some(url) = ctx.server_url() {
        cmd.env("TEAMX_SERVER_URL", url);
    }
    let mut child = cmd.spawn().map_err(|e| format!("spawn tunnel forward: {e}"))?;
    // Track it so the parent-death watcher can clean it up with us.
    crate::procwatch::register_child(child.id());

    // Wait until the local port actually accepts connections (~4s max).
    let mut up = false;
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(100));
        if port_open(port) {
            up = true;
            break;
        }
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
    }
    if !up {
        let _ = child.kill();
        return Err(format!(
            "tunnel forward for `{name}` did not come up (is the binary current? is the tunnel exposed?)"
        ));
    }
    // Detach: the child outlives this call; keep stdout/stderr empty so it
    // never blocks. (A dropped Child on Unix does not terminate the process.)
    drop(child);

    // Remember it for reuse within this session (authoritative).
    session_forwards().lock().unwrap().insert(name.to_string(), port);

    let is_tls = probe(port);
    finish_forward(ctx, name, port, is_tls, false)
}

/// Compute/record the `tun-*` node for a running forward and return the JSON.
fn finish_forward(
    ctx: &Ctx,
    name: &str,
    port: u16,
    is_tls: bool,
    already: bool,
) -> Result<serde_json::Value, String> {
    let node_id = format!("tun-{}", crate::console_data::slugify(name));
    let kind = if name.contains("ttyd") {
        "ttyd"
    } else if name.contains("opencode") {
        "headless"
    } else {
        "web"
    };
    let scheme = if is_tls { "https" } else { "http" };
    let url = format!("{scheme}://127.0.0.1:{port}");
    let mut cfg = crate::console_data::load_config(&ctx.config_path);
    if let Some(n) = cfg.nodes.iter_mut().find(|n| n.id == node_id) {
        n.name = name.to_string();
        n.kind = kind.to_string();
        n.url = url.clone();
    } else {
        cfg.nodes.push(crate::console_data::CloudNode {
            id: node_id.clone(),
            name: name.to_string(),
            kind: kind.to_string(),
            url: url.clone(),
        });
    }
    crate::console_data::save_config(&ctx.config_path, &cfg)
        .map_err(|e| format!("save cloud node: {e}"))?;
    Ok(serde_json::json!({
        "ok": true, "already": already, "name": name,
        "local_port": port, "node_id": node_id, "url": url,
    }))
}

fn port_open(port: u16) -> bool {
    use std::net::{SocketAddr, TcpStream};
    use std::time::Duration;
    TcpStream::connect_timeout(
        &SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(250),
    )
    .is_ok()
}

// ---------------------------------------------------------------------------
// Cloud reverse proxy (same-origin iframe + WebSocket relay)
// ---------------------------------------------------------------------------

/// Parse a node URL into (host, port, tls). Loopback only; https is allowed so
/// e.g. a tunnelled `teamx ui` (self-signed) can be embedded same-origin.
fn target_addr(url: &str) -> Result<(String, u16, bool), String> {
    let trimmed = url.trim();
    let (tls, rest) = if let Some(r) = trimmed.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = trimmed.strip_prefix("http://") {
        (false, r)
    } else {
        return Err("cloud node url must start with http:// or https://".to_string());
    };
    let hostport = rest.split('/').next().unwrap_or("");
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (
            h.to_string(),
            p.parse::<u16>().map_err(|_| format!("bad port in {url}"))?,
        ),
        None => (hostport.to_string(), if tls { 443 } else { 80 }),
    };
    if host != "127.0.0.1" && !host.eq_ignore_ascii_case("localhost") {
        return Err(format!("cloud node url must be loopback, got `{host}`"));
    }
    Ok((host, port, tls))
}

/// Strip the `/api/cloud/{id}` prefix from a request path; the remainder is
/// the target path (leading slash removed, root becomes empty).
fn tail_after(id: &str, path: &str) -> String {
    let marker = format!("/api/cloud/{id}");
    match path.strip_prefix(&marker) {
        Some(t) => t.trim_start_matches('/').to_string(),
        None => String::new(),
    }
}

async fn api_cloud_proxy_root(
    State(state): State<S>,
    headers: HeaderMap,
    Path(id): Path<String>,
    request: Request<Body>,
) -> axum::response::Response {
    api_cloud_http(state, headers, id, String::new(), request).await
}

async fn api_cloud_proxy_rest(
    State(state): State<S>,
    headers: HeaderMap,
    Path((id, _)): Path<(String, String)>,
    request: Request<Body>,
) -> axum::response::Response {
    let full = request.uri().path().to_string();
    let rest = tail_after(&id, &full);
    api_cloud_http(state, headers, id, rest, request).await
}

async fn api_cloud_http(
    state: S,
    headers: HeaderMap,
    id: String,
    rest: String,
    request: Request<Body>,
) -> axum::response::Response {
    if let Err(resp) = guard(&state, &headers) {
        return resp;
    }
    let cfg = load_config(&state.ctx.config_path);
    let node = match cfg.nodes.iter().find(|n| n.id == id) {
        Some(n) => n.clone(),
        None => return err_json(StatusCode::NOT_FOUND, format!("cloud node `{id}` not found")),
    };
    let (host, port, tls) = match target_addr(&node.url) {
        Ok(a) => a,
        Err(e) => return err_json(StatusCode::BAD_REQUEST, e),
    };
    http_proxy(rest, host, port, tls, &state.ctx.home, request).await
}

/// WebSocket relay for terminal apps (ttyd) embedded under
/// `/api/cloud/{id}/ws`.
async fn api_cloud_ws(
    State(state): State<S>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
) -> axum::response::Response {
    if let Err(resp) = guard(&state, &headers) {
        return resp;
    }
    let cfg = load_config(&state.ctx.config_path);
    let node = match cfg.nodes.iter().find(|n| n.id == id) {
        Some(n) => n.clone(),
        None => return err_json(StatusCode::NOT_FOUND, format!("cloud node `{id}` not found")),
    };
    let (host, port, tls) = match target_addr(&node.url) {
        Ok(a) => a,
        Err(e) => return err_json(StatusCode::BAD_REQUEST, e),
    };
    if tls {
        return err_json(
            StatusCode::BAD_REQUEST,
            "WebSocket relay for https cloud nodes is not supported yet".to_string(),
        );
    }
    ws.on_upgrade(move |socket| ws_relay(socket, host, port, "/ws".to_string()))
        .into_response()
}

fn proxy_path(rest: &str, query: Option<&str>) -> String {
    let p = if rest.is_empty() { "/".to_string() } else { format!("/{rest}") };
    match query {
        Some(q) if !q.is_empty() => format!("{p}?{q}"),
        _ => p,
    }
}

async fn http_proxy(
    rest: String,
    host: String,
    port: u16,
    tls: bool,
    home: &std::path::Path,
    request: Request<Body>,
) -> axum::response::Response {
    let method = request.method().clone();
    let path = proxy_path(&rest, request.uri().query());
    let body = match body_to_bytes(request.into_body(), 8 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return err_json(StatusCode::BAD_REQUEST, "request body too large".to_string()),
    };
    let raw = if tls {
        match tls_http_roundtrip(home, &host, port, &method, &path, &body).await {
            Ok(b) => b,
            Err(e) => return err_json(StatusCode::BAD_GATEWAY, format!("proxy to {host}:{port}: {e}")),
        }
    } else {
        // Plain HTTP first; if the reply is not a valid HTTP response the
        // service is probably a TLS one (e.g. a tunnelled `teamx ui`) whose
        // port was recorded before the TLS probe succeeded — retry over TLS.
        match plain_http_roundtrip(&host, port, &method, &path, &body).await {
            Ok(b) if b.starts_with(b"HTTP/") => b,
            _ => match tls_http_roundtrip(home, &host, port, &method, &path, &body).await {
                Ok(b) => b,
                Err(e) => return err_json(
                    StatusCode::BAD_GATEWAY,
                    format!("proxy to {host}:{port} (http then https): {e}"),
                ),
            },
        }
    };
    split_response(raw)
}

/// Plain-TCP HTTP/1.1 exchange against the loopback target.
async fn plain_http_roundtrip(
    host: &str,
    port: u16,
    method: &Method,
    path: &str,
    body: &Bytes,
) -> Result<Vec<u8>, String> {
    use tokio::net::TcpStream;
    let addr = format!("{host}:{port}");
    let tcp = TcpStream::connect(&addr)
        .await
        .map_err(|e| format!("connect {addr}: {e}"))?;
    http_exchange(tcp, host, method, path, body).await
}

/// TLS HTTP/1.1 exchange against a loopback https service (self-signed by the
/// workspace's teamx CA, so we trust the CA from the imported letters).
async fn tls_http_roundtrip(
    home: &std::path::Path,
    host: &str,
    port: u16,
    method: &Method,
    path: &str,
    body: &Bytes,
) -> Result<Vec<u8>, String> {
    use tokio::net::TcpStream;
    use tokio_rustls::TlsConnector;

    let addr = format!("{host}:{port}");
    let tcp = TcpStream::connect(&addr)
        .await
        .map_err(|e| format!("connect {addr}: {e}"))?;
    let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|e| format!("invalid server name {host}: {e}"))?;
    let connector = TlsConnector::from(loopback_tls_client(home)?);
    let tls = connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| format!("tls handshake with {host}:{port}: {e}"))?;
    http_exchange(tls, host, method, path, body).await
}

/// Build a rustls client that trusts the workspace's letter CAs (used for
/// loopback https services such as the tunnelled `teamx ui`).
fn loopback_tls_client(home: &std::path::Path) -> Result<std::sync::Arc<rustls::ClientConfig>, String> {
    use rustls::ClientConfig;
    let mut roots = rustls::RootCertStore::empty();
    let letters = home.join("letters");
    if !letters.is_dir() {
        return Err("no workspace letters to trust the https service".to_string());
    }
    let mut found = 0u32;
    if let Ok(entries) = std::fs::read_dir(&letters) {
        for e in entries.flatten() {
            let ca_path = e.path().join("ca.crt");
            if !ca_path.is_file() {
                continue;
            }
            if let Ok(pem) = std::fs::read(&ca_path) {
                let mut pem = pem.as_slice();
                for cert in rustls_pemfile::certs(&mut pem).flatten() {
                    if roots.add(cert).is_ok() {
                        found += 1;
                    }
                }
            }
        }
    }
    if found == 0 {
        return Err("no CA found in workspace letters to verify the https service".to_string());
    }
    Ok(std::sync::Arc::new(
        ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    ))
}

/// Best-effort probe: does the loopback port speak TLS (https)?
async fn is_tls_server(home: &std::path::Path, host: &str, port: u16) -> bool {
    use tokio_rustls::TlsConnector;
    let Ok(config) = loopback_tls_client(home) else { return false };
    let Ok(tcp) = tokio::net::TcpStream::connect(format!("{host}:{port}")).await else {
        return false;
    };
    let Ok(name) = rustls::pki_types::ServerName::try_from(host.to_string()) else {
        return false;
    };
    let connector = TlsConnector::from(config);
    // Through a tunnel the remote TLS server may be far away; give the probe
    // enough time so a https service is not mis-recorded as http.
    tokio::time::timeout(std::time::Duration::from_millis(2500), connector.connect(name, tcp))
        .await
        .map(|r| r.is_ok())
        .unwrap_or(false)
}

/// Percent-encode a request path for the outgoing HTTP request line (spaces,
/// unicode, etc.), leaving the query string untouched (appended separately).
fn pct_path(path: &str) -> String {
    let mut out = String::new();
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// One HTTP/1.1 exchange over a connected (plain or TLS) stream, with
/// `Connection: close`. Reads the full head, then the body according to
/// Content-Length when present (falling back to reading until EOF with a
/// short grace period, since some local servers ignore `Connection: close`).
async fn http_exchange<S>(
    mut io: S,
    host: &str,
    method: &Method,
    path: &str,
    body: &Bytes,
) -> Result<Vec<u8>, String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let target = match path.split_once('?') {
        Some((p, q)) => format!("{}?{q}", pct_path(p)),
        None => pct_path(path),
    };
    let head = match method.as_str() {
        "POST" | "PUT" | "PATCH" => format!(
            "{} {target} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            method,
            body.len()
        ),
        _ => format!("GET {target} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"),
    };
    io.write_all(head.as_bytes()).await.map_err(|e| format!("write: {e}"))?;
    if !body.is_empty() && method != Method::GET {
        io.write_all(body).await.map_err(|e| format!("write body: {e}"))?;
    }
    // Response head (up to \r\n\r\n).
    let mut raw = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let res = tokio::time::timeout(std::time::Duration::from_secs(8), io.read(&mut byte)).await;
        match res {
            Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
            Ok(Ok(_)) => {
                raw.push(byte[0]);
                if raw.len() >= 4 && &raw[raw.len() - 4..] == b"\r\n\r\n" {
                    break;
                }
                if raw.len() > 65_536 {
                    return Err("response head too large".to_string());
                }
            }
        }
    }
    let head_text = String::from_utf8_lossy(&raw);
    let content_len: Option<usize> = head_text
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            if k.trim().eq_ignore_ascii_case("content-length") {
                v.trim().parse().ok()
            } else {
                None
            }
        });
    if let Some(n) = content_len {
        let mut rest = vec![0u8; n];
        let mut read = 0;
        while read < n {
            let got = io.read(&mut rest[read..]).await.map_err(|e| format!("read body: {e}"))?;
            if got == 0 {
                break;
            }
            read += got;
        }
        rest.truncate(read);
        raw.extend_from_slice(&rest);
    } else {
        // No Content-Length: read until EOF with a short grace period.
        let mut rest = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(3), io.read(&mut buf)).await {
                Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
                Ok(Ok(n)) => rest.extend_from_slice(&buf[..n]),
            }
        }
        raw.extend_from_slice(&rest);
    }
    Ok(raw)
}

/// Parse a raw HTTP response into an axum Response with XFO/CSP removed.
fn split_response(raw: Vec<u8>) -> axum::response::Response {
    let text = String::from_utf8_lossy(&raw);
    let (head, body_bytes) = match text.find("\r\n\r\n") {
        Some(i) => (text[..i].to_string(), raw[i + 4..].to_vec()),
        None => (text.lines().next().unwrap_or("HTTP/1.1 502").to_string(), raw.clone()),
    };
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(502);
    let mut builder = Response::builder().status(status);
    for line in head.lines().skip(1) {
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim().to_string();
            // Hop-by-hop + iframe blockers are stripped.
            let strip = matches!(
                k.as_str(),
                "connection"
                    | "keep-alive"
                    | "transfer-encoding"
                    | "content-length"
                    | "upgrade"
                    | "x-frame-options"
                    | "content-security-policy"
                    | "frame-ancestors"
            );
            if !strip && !k.is_empty() {
                if let Ok(name) = header::HeaderName::from_bytes(k.as_bytes()) {
                    if let Ok(val) = header::HeaderValue::from_str(&v) {
                        builder = builder.header(name, val);
                    }
                }
            }
        }
    }
    builder
        .body(Body::from(body_bytes))
        .unwrap_or_else(|_| Response::new(Body::from("proxy error")))
}

/// Relay a browser WebSocket to the loopback target's WebSocket endpoint.
async fn ws_relay(socket: WebSocket, host: String, port: u16, path: String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    let connect = TcpStream::connect(format!("{host}:{port}")).await;
    let mut tcp = match connect {
        Ok(t) => t,
        Err(_) => return,
    };
    // Handshake against the target.
    let key = "teamsocconsole0000deadbeef==";
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
    );
    if tcp.write_all(req.as_bytes()).await.is_err() {
        return;
    }
    // Read until the end of the target's response head.
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if tcp.read(&mut byte).await.is_err() {
            return;
        }
        head.push(byte[0]);
        if head.len() >= 4 && &head[head.len() - 4..] == b"\r\n\r\n" {
            break;
        }
        if head.len() > 8192 {
            return;
        }
    }
    if !head.starts_with(b"HTTP/1.1 101") {
        return;
    }
    let (mut r, mut w) = tcp.into_split();
    let (mut sink, mut stream) = socket.split();

    // target TCP -> client WS
    let up_task = tokio::spawn(async move {
        let mut buf = [0u8; 8192];
        loop {
            let n = match r.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            if sink.send(WsMessage::Binary(buf[..n].to_vec().into())).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });
    // client WS -> target TCP
    while let Some(msg) = stream.next().await {
        match msg {
            Ok(WsMessage::Binary(b)) => {
                if w.write_all(&b).await.is_err() {
                    break;
                }
            }
            Ok(WsMessage::Text(t)) => {
                let s: &str = t.as_ref();
                if w.write_all(s.as_bytes()).await.is_err() {
                    break;
                }
            }
            Ok(WsMessage::Close(_)) | Err(_) => break,
            Ok(WsMessage::Ping(_)) | Ok(WsMessage::Pong(_)) => {}
        }
    }
    up_task.abort();
}
