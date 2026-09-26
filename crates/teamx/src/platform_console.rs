//! 平台控制台（design/014 §5/§6.5）：**平台面** web UI + API。
//!
//! 分层（与另外两个 console 区分）：
//! - **platform console**（本模块）：平台面 —— 登录 / 租户 / 集群与用量元数据；
//! - admin-console（`teamx ui`）：租户内 lead 看板；
//! - user-console（`teamx console`）：成员自助。
//!
//! 认证：
//! - **launch token**：本地运维（仅绑 127.0.0.1，启动时打印一次），适合平台主机上的 owner；
//! - **GitHub OAuth**：配置 `TEAMX_GITHUB_CLIENT_ID/SECRET` 后启用，登录者绑定为平台管理员
//!   （`users.id` 为「人」的权威键，D8）。
//!
//! 租户内成员操作（invite / provision）走 **service 车道**（§6.5）：平台以
//! `service:platform-console` 身份、持**租户 CA 签发**的服务证书调租户 server 的 RPC，
//! 方法白名单 fail-closed（见 `serve::platform_console_method_allowed`）。

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, delete, get, post},
    Json, Router,
};
use axum::body::Body;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{FromRequestParts, Request};
use axum::extract::ConnectInfo;
use axum_server::tls_rustls::RustlsConfig;
use serde_json::{json, Value};

use crate::cli::PlatformConsoleCmd;

// ---------------------------------------------------------------------------
// 登录防爆破（design/019 SD-13）
// ---------------------------------------------------------------------------

/// 双维度登录保护：**per-IP** 与 **per-账号** 分别计数。
///
/// - 软阈值（默认 15 分钟 ≥5 次失败）→ **指数退避**（1s、2s、4s…上限 60s），期间登录返回 429；
/// - 硬阈值（默认 1 小时 ≥10 次失败）→ **锁定 15 分钟**（IP 与账号同时锁）；
/// - 登录成功 → 清零该维度计数。
///
/// 阈值可用环境变量覆盖：`TEAMX_LOGIN_SOFT/HARD/WINDOW_SECS/LOCK_SECS`（运维可调）。
/// 内存态：单进程控制台足够；重启即复位（可接受——爆破者也会被重启打断）。
pub(crate) struct LoginGuard {
    soft: usize,
    hard: usize,
    window: std::time::Duration,
    hard_window: std::time::Duration,
    lock: std::time::Duration,
    /// key（`ip:…` / `user:…`）→ 失败时间戳
    fails: std::collections::HashMap<String, Vec<std::time::Instant>>,
    /// key → 解锁时刻
    locked: std::collections::HashMap<String, std::time::Instant>,
}

impl LoginGuard {
    pub(crate) fn new() -> Self {
        let n = |k: &str, d: u64| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
        Self {
            soft: n("TEAMX_LOGIN_SOFT", 5) as usize,
            hard: n("TEAMX_LOGIN_HARD", 10) as usize,
            window: std::time::Duration::from_secs(n("TEAMX_LOGIN_WINDOW_SECS", 15 * 60)),
            hard_window: std::time::Duration::from_secs(n("TEAMX_LOGIN_WINDOW_SECS", 15 * 60) * 4),
            lock: std::time::Duration::from_secs(n("TEAMX_LOGIN_LOCK_SECS", 15 * 60)),
            fails: std::collections::HashMap::new(),
            locked: std::collections::HashMap::new(),
        }
    }

    fn prune(&mut self, now: std::time::Instant) {
        let w = self.hard_window;
        self.fails.retain(|_, v| {
            v.retain(|t| now.duration_since(*t) < w);
            !v.is_empty()
        });
        self.locked.retain(|_, until| *until > now);
    }

    /// 是否被拦（返回需等待秒数）。
    pub(crate) fn blocked_for(&self, keys: &[String], now: std::time::Instant) -> Option<u64> {
        let debug_on = std::env::var("TEAMX_LOGIN_DEBUG").is_ok();
        let mut worst: Option<u64> = None;
        for k in keys {
            if let Some(until) = self.locked.get(k) {
                if *until > now {
                    // 向上取整（Retry-After 是秒；截断会让剩余 0.9s 变成 0 → 提前解除）
                    let secs = until.duration_since(now).as_secs() + 1;
                    worst = Some(worst.map_or(secs, |w: u64| w.max(secs)));
                }
            }
            if let Some(v) = self.fails.get(k) {
                let recent: Vec<std::time::Instant> =
                    v.iter().filter(|t| now.duration_since(**t) < self.window).copied().collect();
                if recent.len() >= self.soft {
                    // 退避 = 从**最后一次失败**起算的截止时间（不是常量，否则失败留在窗口内就永不解除）
                    let last = recent.iter().max().copied().unwrap_or(now);
                    let backoff = backoff_secs(recent.len() - self.soft);
                    let until = last + std::time::Duration::from_secs(backoff);
                    if until > now {
                        let remain = until.duration_since(now).as_secs() + 1;
                        worst = Some(worst.map_or(remain, |w: u64| w.max(remain)));
                    }
                }
            }
        }
        if debug_on {
            let counts: Vec<(String, usize)> = keys
                .iter()
                .map(|k| (k.clone(), self.fails.get(k).map(|v| v.len()).unwrap_or(0)))
                .collect();
            eprintln!("[lg] blocked_for -> {worst:?} | soft={} fails={counts:?}", self.soft);
        }
        worst
    }

    /// 记一次失败；返回 `(需等待秒数, 是否刚触发锁定)`。
    pub(crate) fn fail(&mut self, keys: &[String], now: std::time::Instant) -> (u64, bool) {
        let debug_on = std::env::var("TEAMX_LOGIN_DEBUG").is_ok();
        self.prune(now);
        let mut locked_now = false;
        let mut worst = 0u64;
        for k in keys {
            let e = self.fails.entry(k.clone()).or_default();
            e.push(now);
            let in_window = e.iter().filter(|t| now.duration_since(**t) < self.window).count();
            if in_window >= self.soft {
                worst = worst.max(backoff_secs(in_window - self.soft));
            }
            if e.len() >= self.hard {
                self.locked.insert(k.clone(), now + self.lock);
                locked_now = true;
                worst = worst.max(self.lock.as_secs());
            }
        }
        if debug_on {
            eprintln!("[lg] fail -> wait={worst} locked={locked_now}");
        }
        (worst, locked_now)
    }

    /// 登录成功 → 清零。
    pub(crate) fn ok(&mut self, keys: &[String]) {
        for k in keys {
            self.fails.remove(k);
            self.locked.remove(k);
        }
    }
}

/// 指数退避秒数：**首步 2s**，其后翻倍（2/4/8/16/32），上限 60s。
/// `over` = 窗口内失败数 − 软阈值。
fn backoff_secs(over: usize) -> u64 {
    let e = (over + 1).min(6) as u32;
    (1u64 << e).min(60)
}

/// 客户端 IP：优先 `X-Forwarded-For` 首段（反向代理场景），否则取对端地址。
fn client_ip(headers: &HeaderMap, peer: Option<std::net::SocketAddr>) -> String {
    if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        if let Some(first) = xff.split(',').next() {
            let t = first.trim();
            if !t.is_empty() {
                return t.to_string();
            }
        }
    }
    peer.map(|p| p.ip().to_string()).unwrap_or_else(|| "unknown".to_string())
}

/// 把服务端错误文案映射成**稳定错误码**（design/019 SD-14）：
/// 服务端只保证 `code` 稳定，**文案由前端按语言映射**；`error` 原文保留作兜底与排障。
pub(crate) fn err_code(msg: &str) -> &'static str {
    let m = msg.to_lowercase();
    let has = |k: &str| m.contains(k);
    if has("too_many_attempts") || has("too many attempts") {
        "too_many_attempts"
    } else if has("invalid credentials") {
        "invalid_credentials"
    } else if has("unauthorized") || has("bad token") {
        "unauthorized"
    } else if has("github oauth not configured") {
        "github_not_configured"
    } else if has("not your tenant") {
        "not_your_tenant"
    } else if has("not a member") {
        "not_member"
    } else if has("only the owner") || has("owner only") {
        "owner_only"
    } else if has("quota") {
        "quota_exceeded"
    } else if has("not found") || has("no such") {
        "not_found"
    } else if has("cannot reach")
        || has("port-forward")
        || has("exited immediately")
        || has("tenant-side unavailable")
        || has("unreachable")
    {
        "tenant_unreachable"
    } else if has("already") || has("exists") {
        "already_exists"
    } else if has("cooldown") {
        "cooldown"
    } else if has("disabled") {
        "disabled"
    } else {
        "request_failed"
    }
}

/// 统一错误响应体：`{ok:false, code, error}`（`code` 稳定、`error` 为英文兜底）。
pub(crate) fn err_body(msg: &str) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": false, "code": err_code(msg), "error": msg }))
}

/// 是否被拦（含等待秒数）。
fn guard_blocked(ctx: &Arc<Ctx>, keys: &[String], now: std::time::Instant) -> Option<u64> {
    ctx.login_guard.lock().ok().and_then(|g| g.blocked_for(keys, now))
}
/// 记一次失败 → `(需等待秒数, 是否锁定)`。
fn guard_fail(ctx: &Arc<Ctx>, keys: &[String], now: std::time::Instant) -> (u64, bool) {
    ctx.login_guard.lock().map(|mut g| g.fail(keys, now)).unwrap_or((0, false))
}
/// 登录成功 → 清零。
fn guard_ok(ctx: &Arc<Ctx>, keys: &[String]) {
    if let Ok(mut g) = ctx.login_guard.lock() {
        g.ok(keys);
    }
}
/// 429 + `Retry-After`（防爆破统一响应；文案由前端按 error code 渲染，SD-14）。
fn too_many(secs: u64) -> Response {
    let mut r = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({"ok": false, "code": "too_many_attempts", "error": "too_many_attempts", "retry_after": secs})),
    )
        .into_response();
    if let Ok(v) = axum::http::HeaderValue::from_str(&secs.max(1).to_string()) {
        r.headers_mut().insert(axum::http::header::RETRY_AFTER, v);
    }
    r
}
/// 失败/锁定写平台审计（NFR-9：审计独立于租户台账）。
fn audit_blocked(ctx: &Arc<Ctx>, ip: &str, account: &str, locked: bool, wait: u64) {
    let action = if locked { "platform_user.login_locked" } else { "platform_user.login_failed" };
    let detail = format!("account={account} {ip} retry_after={wait}s");
    let _ = with_conn(ctx, |c| crate::platform::audit(c, "anonymous", action, Some(&detail)));
}

/// 平台控制台状态：平台 DB 句柄 + launch token + K8s 访问参数 + OAuth state 表。
#[derive(Clone)]
struct Ctx {
    db_path: std::path::PathBuf,
    launch_token: String,
    kubeconfig: Option<String>,
    context: Option<String>,
    /// 登录防爆破（design/019 SD-13）：双维度（IP + 账号）计数、指数退避、锁定。
    login_guard: std::sync::Arc<std::sync::Mutex<LoginGuard>>,
    /// OAuth state（CSRF）：state → 发起时间；单次有效，10 分钟过期。
    /// 内存态（单进程控制台；重启后未完成的登录作废，需重走 start）。
    oauth_states: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>>,
    /// 平台控制台登录页路径：`teamx platform-console` 为 `/`，`teamx site` 为 `/console`。
    /// 未登录访问 `/c/<member16>` 时 303 到这里。
    console_page: String,
    /// 成员 console 反代目标覆盖（仅测试/排障）。`None` = 走 `kubectl port-forward`
    /// 到成员的 ClusterIP Service；`Some("https://127.0.0.1:PORT")` = 直连该 loopback。
    console_target_override: Option<String>,
}

/// 启动平台控制台（HTTPS，绑 127.0.0.1 或显式地址）。
pub fn run(cmd: &PlatformConsoleCmd) -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    rt.block_on(run_async(cmd))
}

async fn run_async(cmd: &PlatformConsoleCmd) -> Result<(), String> {
    let db_path = crate::db::default_db_path();
    {
        let conn = crate::db::open(&db_path).map_err(|e| format!("open db: {e}"))?;
        crate::db::migrate(&conn).map_err(|e| format!("migrate: {e}"))?;
    }
    let launch_token = crate::util::gen_access_token(16);
    // kubeconfig/context：CLI flag 优先，其次环境变量（TEAMX_KUBECONFIG / TEAMX_K8S_CONTEXT）。
    // 环境变量回退是必需的：systemd 部署没有 CLI flag，而 k3s 自带的 kubectl 是 k3s
    // 包装器，缺省读 /etc/rancher/k3s/k3s.yaml（普通用户权限不足）—— 真机踩到。
    let flag_kc = cmd.kubeconfig.as_ref().map(|p| p.to_string_lossy().to_string());
    let kubeconfig = flag_kc.or_else(|| {
        std::env::var("TEAMX_KUBECONFIG").ok().filter(|s| !s.trim().is_empty())
    });
    let context = cmd
        .context
        .clone()
        .or_else(|| std::env::var("TEAMX_K8S_CONTEXT").ok().filter(|s| !s.trim().is_empty()));
    let ctx = Arc::new(Ctx {
        db_path,
        launch_token: launch_token.clone(),
        kubeconfig,
        context,
        login_guard: std::sync::Arc::new(std::sync::Mutex::new(LoginGuard::new())),
        oauth_states: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        console_page: "/".to_string(),
        console_target_override: None,
    });

    let app = build_router(ctx.clone(), true);
    serve_https(app, &cmd.addr, cmd.port, launch_token, None, None, None).await
}

/// 组装路由：控制台全部 `/api/*` + 可选 `/`（`root_console=true` 时 `/` 是控制台首页）。
///
/// `teamx site` 用 `root_console=false`，把 `/` 让给官网、控制台首页挂到 `/console`；
/// 控制台前端用的是**绝对路径** `/api/...`，所以嵌套与否都不影响它自己工作（Login 即站内相对路径）。
fn build_router(ctx: Arc<Ctx>, root_console: bool) -> Router {
    let base = Router::new();
    let base = if root_console { base.route("/", get(index)) } else { base };
    base
        .route("/c/{member16}", any(api_console_proxy))
        .route("/c/{member16}/", any(api_console_proxy))
        .route("/c/{member16}/{*rest}", any(api_console_proxy))
        .route("/api/login", post(api_login))
        .route("/api/login/password", post(api_login_password))
        .route("/api/logout", post(api_logout))
        .route("/api/me", get(api_me))
        .route("/api/tenants", get(api_tenants).post(api_tenant_create))
        .route("/api/discovery/directory", get(api_dir))
        .route("/api/discovery/teams/{team}", get(api_dir_team))
        .route("/api/discovery/teams/{team}/apply", post(api_dir_apply))
        .route("/api/discovery/teams/{team}/settings", post(api_dir_settings))
        .route("/api/discovery/mine", get(api_dir_mine))
        .route("/api/discovery/requests/{id}/withdraw", post(api_dir_withdraw))
        .route("/api/discovery/pending", get(api_dir_pending))
        .route("/api/discovery/requests/{id}/decide", post(api_dir_decide))
        .route("/api/my/teams", get(api_my_teams).post(api_my_teams_create))
        .route("/api/my/teams/{team}/roster", get(api_my_roster))
        .route("/api/my/team-console/{team}", get(api_my_team_console))
        .route("/api/my/teams/{team}/invite", post(api_my_invite))
        .route("/api/my/teams/{team}/leave", post(api_my_leave))
        .route("/api/my/letters", get(api_my_letters))
        .route("/api/my/credit", get(api_my_credit))
        .route("/api/my/credit/{team}", get(api_my_credit_team))
        .route("/api/my/credit/{team}/refresh", post(api_my_credit_refresh))
        .route("/api/my/tcoin", get(api_my_tcoin))
        .route("/api/my/tcoin/{team}", get(api_my_tcoin_team))
        .route("/api/my/tcoin/{team}/settle", post(api_my_tcoin_settle))
        .route("/api/my/teams/{team}/bootstrap", post(api_my_teams_bootstrap))
        .route("/api/tenants/{team}", delete(api_tenant_destroy))
        .route("/api/tenants/{team}/invite", post(api_tenant_invite))
        .route("/api/tenants/{team}/provision", post(api_tenant_provision))
        .route("/api/tenants/{team}/reissue-members", post(api_tenant_reissue))
        .route("/api/tokens", get(api_tokens))
        .route("/api/admins", get(api_admins))
        .route("/api/audit", get(api_audit))
        .route("/api/oauth/start", get(api_oauth_start))
        .route("/api/oauth/callback", get(api_oauth_callback))
        .with_state(ctx)
}

/// 启动 HTTPS 服务：`cert/key` 给定则用外部证书（Let's Encrypt 等），
/// 否则回退平台自签 instance 证书。可选在 `http_port` 上并行起一个 **HTTP→HTTPS 301** 跳转监听。
#[allow(clippy::too_many_arguments)]
async fn serve_https(
    app: Router,
    addr: &str,
    port: u16,
    launch_token: String,
    cert: Option<std::path::PathBuf>,
    key: Option<std::path::PathBuf>,
    http_port: Option<u16>,
) -> Result<(), String> {
    let (cert_pem, key_pem, self_signed) = match (cert, key) {
        (Some(c), Some(k)) => (
            std::fs::read(&c).map_err(|e| format!("read cert {}: {e}", c.display()))?,
            std::fs::read(&k).map_err(|e| format!("read key {}: {e}", k.display()))?,
            false,
        ),
        (None, None) => {
            // 平台自身 instance 证书（与另外两个 console 同源）
            let home = crate::db::teamx_home();
            let pk = crate::pki::ensure_pki(&home).map_err(|e| format!("pki: {e}"))?;
            (
                std::fs::read(&pk.server_cert).map_err(|e| format!("read cert: {e}"))?,
                std::fs::read(&pk.server_key).map_err(|e| format!("read key: {e}"))?,
                true,
            )
        }
        _ => return Err("--tls-cert 与 --tls-key 必须成对提供".to_string()),
    };
    let config = RustlsConfig::from_pem(cert_pem, key_pem)
        .await
        .map_err(|e| format!("tls config: {e}"))?;
    let sock: std::net::SocketAddr = format!("{addr}:{port}")
        .parse()
        .map_err(|e| format!("bad bind addr: {e}"))?;
    eprintln!("teamx site (website + console)");
    eprintln!("  site   : https://{sock}/");
    eprintln!("  console: https://{sock}/console?token={launch_token}");
    if self_signed {
        eprintln!("  (self-signed cert; your browser will warn about it once)");
    }
    eprintln!(
        "  GitHub OAuth: {}",
        if github_configured() { "enabled" } else { "disabled (set TEAMX_GITHUB_CLIENT_ID/SECRET)" }
    );

    // HTTP → HTTPS 301（design/019 SD-12「强制 HTTPS」）
    if let Some(hp) = http_port {
        let host = sock.ip().to_string();
        let target_port = sock.port();
        let haddr: std::net::SocketAddr = format!("{host}:{hp}")
            .parse()
            .map_err(|e| format!("bad http bind addr: {e}"))?;
        let redirect = Router::new().fallback(move |req: axum::extract::Request| {
            let path = req.uri().path_and_query().map(|p| p.as_str().to_string()).unwrap_or_else(|| "/".to_string());
            let host = req
                .headers()
                .get(axum::http::header::HOST)
                .and_then(|v| v.to_str().ok())
                .map(|h| h.split(':').next().unwrap_or("").to_string())
                .unwrap_or_else(|| "localhost".to_string());
            async move {
                let url = format!("https://{host}:{target_port}{path}");
                (StatusCode::MOVED_PERMANENTLY, [(axum::http::header::LOCATION, url)])
            }
        });
        let rhost = haddr.ip().to_string();
        tokio::spawn(async move {
            if let Ok(l) = tokio::net::TcpListener::bind(haddr).await {
                eprintln!("  http   : http://{rhost}:{hp}/ → 301 https");
                let _ = axum::serve(l, redirect).await;
            } else {
                eprintln!("  http   : 绑定 {haddr} 失败（301 跳转未启用）");
            }
        });
    }

    axum_server::bind_rustls(sock, config)
        .serve(app.into_make_service_with_connect_info::<std::net::SocketAddr>())
        .await
        .map_err(|e| format!("serve: {e}"))
}

/// 官网静态资源（`/assets/{file}`）：只从内嵌表取，天然免疫路径穿越。
async fn site_asset(axum::extract::Path(file): axum::extract::Path<String>) -> Response {
    match crate::site::asset(&file) {
        Some((bytes, ct)) => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, ct),
             (axum::http::header::CACHE_CONTROL, "public, max-age=86400")],
            bytes,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

/// 官网首页（design/019）。
async fn site_index() -> Response {
    html_no_store(crate::site::INDEX_HTML)
}

/// 控制台首页（挂在 `/console`，供 `teamx site` 使用）。
async fn console_index() -> Response {
    html_no_store(INDEX_HTML)
}

/// 启动 `teamx site`（design/019）：一个进程同时服务官网 `/` 与控制台 `/console`。
///
/// **强制 HTTPS（SD-12）**：提供 `--tls-cert/--tls-key` 时用外部证书（Let's Encrypt 等）
/// 并允许对外监听；**未提供证书时只监听 127.0.0.1**（自签证书，仅供本机预览，绝不明文对外）。
pub fn run_site(cmd: &crate::cli::SiteCmd) -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    rt.block_on(run_site_async(cmd))
}

async fn run_site_async(cmd: &crate::cli::SiteCmd) -> Result<(), String> {
    // 证书：CLI flag 优先，其次环境变量（部署时便于走密钥管理，避免写进命令行）
    let flag = |v: &Option<std::path::PathBuf>, k: &str| {
        v.clone().or_else(|| std::env::var(k).ok().filter(|s| !s.trim().is_empty()).map(std::path::PathBuf::from))
    };
    let cert = flag(&cmd.tls_cert, "TEAMX_TLS_CERT");
    let key = flag(&cmd.tls_key, "TEAMX_TLS_KEY");
    let has_cert = cert.is_some() && key.is_some();

    // 未配证书 → 强制回环（SD-12）
    let mut addr = cmd.addr.clone();
    if !has_cert && addr != "127.0.0.1" && addr != "localhost" && addr != "::1" {
        eprintln!("teamx: 未提供 --tls-cert/--tls-key，强制只监听 127.0.0.1（对外必须配证书）");
        addr = "127.0.0.1".to_string();
    }

    let db_path = crate::db::default_db_path();
    {
        let conn = crate::db::open(&db_path).map_err(|e| format!("open db: {e}"))?;
        crate::db::migrate(&conn).map_err(|e| format!("migrate: {e}"))?;
    }
    let launch_token = crate::util::gen_access_token(16);
    let flag_kc = cmd.kubeconfig.as_ref().map(|p| p.to_string_lossy().to_string());
    let kubeconfig = flag_kc.or_else(|| std::env::var("TEAMX_KUBECONFIG").ok().filter(|s| !s.trim().is_empty()));
    let context = cmd
        .context
        .clone()
        .or_else(|| std::env::var("TEAMX_K8S_CONTEXT").ok().filter(|s| !s.trim().is_empty()));
    let ctx = Arc::new(Ctx {
        db_path,
        launch_token: launch_token.clone(),
        kubeconfig,
        context,
        login_guard: std::sync::Arc::new(std::sync::Mutex::new(LoginGuard::new())),
        oauth_states: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        console_page: "/console".to_string(),
        console_target_override: None,
    });

    let app = build_router(ctx, false)
        .route("/", get(site_index))
        .route("/assets/{file}", get(site_asset))
        .route("/console", get(console_index));

    serve_https(app, &addr, cmd.port, launch_token, cert, key, cmd.http_port).await
}

// ---------------------------------------------------------------------------
// 认证
// ---------------------------------------------------------------------------

/// **所有** `teamx_platform_token` cookie 值（保持出现顺序，去重）。
///
/// 真机踩到两回：
/// ① HTTP/2 下浏览器（Firefox）会把 cookie 拆成多个 `cookie` 字段 —— 只读第一个会丢 token；
/// ② 浏览器可能**同时存在多个同名 cookie**（例如旧版本代码用别的 Path 写过，或先经 `?token=`
///    登录过再改密码登录）—— 若取到的第一个是**失效**的那个，用户就会"登录成功却停在登录页"。
/// 因此这里返回**全部**候选值，由鉴权逻辑逐个尝试，任一有效即通过。
fn cookie_tokens(headers: &HeaderMap) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in headers.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()) {
        for kv in raw.split(';').map(str::trim) {
            if let Some(t) = kv.strip_prefix("teamx_platform_token=") {
                if !t.is_empty() && !out.iter().any(|x| x == t) {
                    out.push(t.to_string());
                }
            }
        }
    }
    out
}

/// 兼容旧调用：取第一个候选值（鉴权请用 `cookie_tokens` 逐个尝试）。
fn cookie_token(headers: &HeaderMap) -> Option<String> {
    cookie_tokens(headers).into_iter().next()
}

/// 平台面鉴权（fail-closed）：cookie 是 launch token（运维）**或**有效的用户会话（Phase 1）。
/// 先比 launch token（无 DB 开销），再验会话（查库 + 过期/禁用检查）。
#[allow(clippy::result_large_err)] // Response 作为错误值直接返回，避免多层包装
fn guard(ctx: &Ctx, headers: &HeaderMap) -> Result<(), Response> {
    for t in cookie_tokens(headers) {
        if t == ctx.launch_token {
            return Ok(());
        }
        if with_conn(ctx, |c| crate::platform::verify_session(c, &t)).is_ok() {
            return Ok(());
        }
    }
    Err(unauth())
}

fn unauth() -> Response {
    (StatusCode::UNAUTHORIZED, err_body("unauthorized")).into_response()
}

/// 会话对应的平台用户（None = 运维 launch-token 或未登录）。
fn session_user(ctx: &Ctx, headers: &HeaderMap) -> Option<(String, String)> {
    // 逐个候选值尝试：任一能验成会话即认该用户（多 cookie 场景下不被失效 cookie 挡住）
    for t in cookie_tokens(headers) {
        if t == ctx.launch_token {
            continue;
        }
        if let Ok(v) = with_conn(ctx, |c| crate::platform::verify_session(c, &t)) {
            let uid = v["user_id"].as_str().unwrap_or("").to_string();
            if !uid.is_empty() {
                return Some((uid, v["display_name"].as_str().unwrap_or("").to_string()));
            }
        }
    }
    None
}

/// 当前身份（展示 + 审计用）：运维 launch token 或已登录用户。
fn identity(ctx: &Ctx, headers: &HeaderMap) -> String {
    if let Some((uid, name)) = session_user(ctx, headers) {
        return format!("user:{name}({uid})");
    }
    match cookie_token(headers) {
        Some(t) if t == ctx.launch_token => "operator(launch-token)".to_string(),
        Some(t) => with_conn(ctx, |c| crate::platform::verify_session(c, &t))
            .map(|v| format!("user:{}", v["display_name"].as_str().unwrap_or("?")))
            .unwrap_or_else(|_| "unknown".to_string()),
        None => "anonymous".to_string(),
    }
}

fn with_conn<T>(ctx: &Ctx, f: impl FnOnce(&mut rusqlite::Connection) -> Result<T, String>) -> Result<T, String> {
    let mut conn = crate::db::open(&ctx.db_path).map_err(|e| format!("open db: {e}"))?;
    crate::db::migrate(&conn).map_err(|e| format!("migrate: {e}"))?;
    f(&mut conn)
}

/// HTML 响应统一 **no-store**：控制台随版本频繁变化，缓存旧页会与新接口不匹配
/// （表现：登录后仍停在登录页 / 按钮无反应）。
fn html_no_store(body: &'static str) -> Response {
    (
        StatusCode::OK,
        [
            (axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (axum::http::header::CACHE_CONTROL, "no-store, no-cache, must-revalidate"),
        ],
        body,
    )
        .into_response()
}

async fn index() -> Response {
    html_no_store(INDEX_HTML)
}

async fn api_login(
    State(ctx): State<Arc<Ctx>>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    Json(body): Json<Value>,
) -> Response {
    let ip = client_ip(&headers, Some(peer));
    let token = body.get("token").and_then(Value::as_str).unwrap_or("");
    let ipkey = vec![format!("ip:{ip}")];
    let now = std::time::Instant::now();
    if let Some(secs) = guard_blocked(&ctx, &ipkey, now) {
        return too_many(secs);
    }
    if token != ctx.launch_token {
        // launch token 爆破同样计数（只按 IP，无账号维度）
        let (wait, locked) = guard_fail(&ctx, &ipkey, now);
        audit_blocked(&ctx, &format!("ip:{ip}"), "launch", locked, wait);
        if locked || wait > 0 {
            return too_many(wait.max(1));
        }
        return (StatusCode::UNAUTHORIZED, err_body("bad token")).into_response();
    }
    guard_ok(&ctx, &ipkey);
    let mut resp = Json(json!({"ok": true})).into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        format!("teamx_platform_token={}; Path=/; HttpOnly; Secure; SameSite=Strict", ctx.launch_token)
            .parse()
            .unwrap(),
    );
    resp
}

/// 密码登录（Phase 1）：成功发会话 cookie（7 天），失败统一 401（防枚举，不写库）。
async fn api_login_password(
    State(ctx): State<Arc<Ctx>>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    Json(body): Json<Value>,
) -> Response {
    let name = body.get("name").and_then(Value::as_str).unwrap_or("");
    let password = body.get("password").and_then(Value::as_str).unwrap_or("");
    // 防爆破（SD-13）：**双维度** —— IP 与账号分别计数，任一超限即拦
    let ip = client_ip(&headers, Some(peer));
    let keys = vec![
        format!("ip:{ip}"),
        format!("user:{}", name.trim().to_lowercase()),
    ];
    let now = std::time::Instant::now();
    if let Some(secs) = guard_blocked(&ctx, &keys, now) {
        return too_many(secs);
    }
    let user = match with_conn(&ctx, |c| crate::platform::authenticate(c, name, password)) {
        Ok(u) => u,
        Err(_) => {
            let (wait, locked) = guard_fail(&ctx, &keys, now);
            audit_blocked(&ctx, &format!("ip:{ip}"), name, locked, wait);
            if locked || wait > 0 {
                return too_many(wait.max(1));
            }
            return (StatusCode::UNAUTHORIZED, err_body("invalid credentials"))
                .into_response();
        }
    };
    guard_ok(&ctx, &keys);
    let uid = user["user_id"].as_str().unwrap_or("").to_string();
    let uname = user["display_name"].as_str().unwrap_or("").to_string();
    let sess = match with_conn(&ctx, |c| crate::platform::issue_session(c, &uid)) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, err_body(&e))
                .into_response()
        }
    };
    let token = sess["token"].as_str().unwrap_or("").to_string();
    let _ = with_conn(&ctx, |c| {
        crate::platform::audit(c, &format!("user:{uname}"), "platform_user.login", None)
    });
    let mut resp = Json(json!({"ok": true, "display_name": uname})).into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        format!("teamx_platform_token={token}; Path=/; HttpOnly; Secure; SameSite=Strict")
            .parse()
            .unwrap(),
    );
    resp
}

/// 登出：吊销会话（若是）+ 清 cookie（幂等）。
async fn api_logout(State(ctx): State<Arc<Ctx>>, headers: HeaderMap) -> Response {
    for t in cookie_tokens(&headers) {
        if t != ctx.launch_token {
            let _ = with_conn(&ctx, |c| crate::platform::revoke_session(c, &t));
        }
    }
    let mut resp = Json(json!({"ok": true})).into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        "teamx_platform_token=; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=0"
            .parse()
            .unwrap(),
    );
    resp
}

async fn api_me(State(ctx): State<Arc<Ctx>>, headers: HeaderMap) -> Response {
    if let Err(r) = guard(&ctx, &headers) {
        return r;
    }
    let admins = with_conn(&ctx, |c| crate::platform::list_admins(c)).unwrap_or_default();
    let who = identity(&ctx, &headers);
    Json(json!({ "ok": true, "who": who, "admins": admins, "github_oauth": github_configured() })).into_response()
}

// ---------------------------------------------------------------------------
// 租户（平台面真实操作）
// ---------------------------------------------------------------------------

async fn api_tenants(State(ctx): State<Arc<Ctx>>, headers: HeaderMap) -> Response {
    if let Err(r) = guard(&ctx, &headers) {
        return r;
    }
    let mine = session_user(&ctx, &headers).map(|(uid, _)| uid);
    match with_conn(&ctx, |c| {
        let all = crate::provision::tenant_list(c)?;
        match mine {
            Some(uid) => {
                let owned = crate::platform::owned_teams(c, &uid).map_err(|e| e.to_string())?;
                Ok::<_, String>(
                    all.into_iter()
                        .filter(|t| {
                            t.get("team_id")
                                .and_then(Value::as_str)
                                .map(|id| owned.contains(&id.to_string()))
                                .unwrap_or(false)
                        })
                        .collect(),
                )
            }
            None => Ok(all),
        }
    }) {
        Ok(ts) => Json(json!({ "ok": true, "tenants": ts })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, err_body(&e)).into_response(),
    }
}

/// 归属门：用户会话只能动名下租户；运维（无会话用户）放行。返回 Err(Response) 即拒。
#[allow(clippy::result_large_err)] // 与 guard 同：Response 作错误值直接返回
fn own_guard(ctx: &Ctx, headers: &HeaderMap, team: &str) -> Result<(), Response> {
    if let Some((uid, _)) = session_user(ctx, headers) {
        let ok =
            with_conn(ctx, |c| crate::platform::tenant_owned_by(c, team, &uid)).unwrap_or(false);
        if !ok {
            return Err((
                StatusCode::FORBIDDEN,
                err_body("not your tenant"),
            )
                .into_response());
        }
    }
    Ok(())
}

async fn api_tenant_create(State(ctx): State<Arc<Ctx>>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    if let Err(r) = guard(&ctx, &headers) {
        return r;
    }
    // 用户自助走 /api/my/teams（配额 + 归属）；本入口仅运维。
    if session_user(&ctx, &headers).is_some() {
        return (
            StatusCode::FORBIDDEN,
            err_body("use POST /api/my/teams for self-service"),
        )
            .into_response();
    }
    let team = body.get("team").and_then(Value::as_str).unwrap_or("").to_string();
    let image = body.get("server_image").and_then(Value::as_str).map(str::to_string);
    if team.is_empty() {
        return (StatusCode::BAD_REQUEST, err_body("team is required")).into_response();
    }
    let (kc, cx) = (ctx.kubeconfig.clone(), ctx.context.clone());
    match with_conn(&ctx, |c| crate::provision::create_tenant(c, &team, image.as_deref(), kc.as_deref(), cx.as_deref())) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, err_body(&e)).into_response(),
    }
}

async fn api_tenant_destroy(State(ctx): State<Arc<Ctx>>, headers: HeaderMap, Path(team): Path<String>) -> Response {
    if let Err(r) = guard(&ctx, &headers) {
        return r;
    }
    if let Err(r) = own_guard(&ctx, &headers, &team) {
        return r;
    }
    let (kc, cx) = (ctx.kubeconfig.clone(), ctx.context.clone());
    match with_conn(&ctx, |c| crate::provision::destroy_tenant(c, &team, kc.as_deref(), cx.as_deref())) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, err_body(&e)).into_response(),
    }
}

/// 经 **service 车道**（§6.5）在租户内邀请成员。
async fn api_tenant_invite(State(ctx): State<Arc<Ctx>>, headers: HeaderMap, Path(team): Path<String>, Json(body): Json<Value>) -> Response {
    if let Err(r) = guard(&ctx, &headers) {
        return r;
    }
    if let Err(r) = own_guard(&ctx, &headers, &team) {
        return r;
    }
    let role_desc = body.get("role_desc").and_then(Value::as_str).unwrap_or("contributor").to_string();
    let name_hint = body.get("name_hint").and_then(Value::as_str).map(str::to_string);
    service_call(&ctx, &team, "team.invite", json!({
        "role_desc": role_desc, "name_hint": name_hint, "team": team,
    }))
    .await
}

/// 经 service 车道触发该租户的成员 provision。
async fn api_tenant_provision(State(ctx): State<Arc<Ctx>>, headers: HeaderMap, Path(team): Path<String>, Json(body): Json<Value>) -> Response {
    if let Err(r) = guard(&ctx, &headers) {
        return r;
    }
    if let Err(r) = own_guard(&ctx, &headers, &team) {
        return r;
    }
    let member = body.get("member").and_then(Value::as_str).unwrap_or("").to_string();
    if member.is_empty() {
        return (StatusCode::BAD_REQUEST, err_body("member is required")).into_response();
    }
    service_call(&ctx, &team, "node.provision", json!({
        "member": member, "team": team,
        "provider": body.get("provider").cloned().unwrap_or(json!("k8e")),
        "region": body.get("region").cloned().unwrap_or(json!("us-west-2")),
        "os_user": body.get("os_user").cloned().unwrap_or(Value::Null),
    }))
    .await
}

/// 成员证书换发（design/014 §8 ①②③）：平台面直接执行（非经 service 车道 ——
/// 换发发生在**成员切换到租户 server 之前**，此时租户 server 可能尚不可达）。
async fn api_tenant_reissue(State(ctx): State<Arc<Ctx>>, headers: HeaderMap, Path(team): Path<String>) -> Response {
    if let Err(r) = guard(&ctx, &headers) {
        return r;
    }
    if let Err(r) = own_guard(&ctx, &headers, &team) {
        return r;
    }
    let kubeconfig = ctx.kubeconfig.clone();
    let context = ctx.context.clone();
    let t = team.clone();
    let out = with_conn(&ctx, |c| {
        crate::provision::reissue_member_certs(c, &t, kubeconfig.as_deref(), context.as_deref())
    });
    match out {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, err_body(&e)).into_response(),
    }
}

// ---------------------------------------------------------------------------
// 团队发现与加入（design/015）：全部团队 / 详情 / 申请 / 审批
// ---------------------------------------------------------------------------

/// 要求登录用户会话；返回 (user_id, display_name)。
#[allow(clippy::result_large_err)]
fn require_user(ctx: &Ctx, headers: &HeaderMap) -> Result<(String, String), Response> {
    session_user(ctx, headers).ok_or_else(|| {
        (StatusCode::FORBIDDEN, err_body("user session required"))
            .into_response()
    })
}

/// 「全部团队」名录（SD-12：须登录；SD-19/SD-25：搜索/排序/筛选/分页）。
async fn api_dir(
    State(ctx): State<Arc<Ctx>>,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    if let Err(r) = guard(&ctx, &headers) { return r; }
    if let Err(r) = require_user(&ctx, &headers) { return r; }
    let query = q.get("query").cloned();
    let sort = q.get("sort").cloned();
    let joinable = q.get("joinable").map(|v| v == "1" || v == "true").unwrap_or(false);
    let page = q.get("page").and_then(|v| v.parse::<i64>().ok()).unwrap_or(1);
    let per_page = q.get("per_page").and_then(|v| v.parse::<i64>().ok()).unwrap_or(20);
    let (uid, _) = match require_user(&ctx, &headers) { Ok(v) => v, Err(r) => return r };
    match with_conn(&ctx, |c| {
        let mut d = crate::discovery::directory(c, query.as_deref(), sort.as_deref(), joinable, page, per_page)?;
        // 列表也要带**访问者关系**（否则列表标"可加入"、详情却没按钮 —— 用户看到自相矛盾）
        let owned = crate::platform::owned_teams(c, &uid).map_err(|e| e.to_string())?;
        let joined: Vec<String> = crate::discovery::joined_teams(c, &uid)
            .map_err(|e| e.to_string())?
            .iter()
            .filter_map(|j| j["team_id"].as_str().map(str::to_string))
            .collect();
        if let Some(items) = d["items"].as_array_mut() {
            for it in items.iter_mut() {
                let id = it["team_id"].as_str().unwrap_or("").to_string();
                let rel = if owned.contains(&id) { "owner" } else if joined.contains(&id) { "member" } else { "none" };
                it["relation"] = json!(rel);
                it["can_apply"] = json!(it["joinable"] == json!(true) && rel == "none");
            }
        }
        Ok::<_, String>(d)
    }) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, err_body(&e)).into_response(),
    }
}

/// 详情（SD-20：private 非成员 → 403）。
///
/// **诚实降级**：租户侧字段（goal 标题/状态）需要租户 server 端点（开放项 O-5 未定，
/// 控制台与集群同机时靠 port-forward）。端点不可达时返回平台元数据 + `tenant_meta: null`
/// 并说明原因，**不假装有**。
async fn api_dir_team(
    State(ctx): State<Arc<Ctx>>,
    headers: HeaderMap,
    Path(team): Path<String>,
) -> Response {
    if let Err(r) = guard(&ctx, &headers) { return r; }
    let (uid, _) = match require_user(&ctx, &headers) { Ok(v) => v, Err(r) => return r };
    let detail = with_conn(&ctx, |c| {
        if !crate::discovery::visible_to(c, &team, Some(&uid)).map_err(|e| e.to_string())? {
            return Err("team not found".to_string());
        }
        let s = crate::discovery::settings(c, &team).map_err(|e| e.to_string())?
            .ok_or_else(|| "team not found".to_string())?;
        let owner: String = c
            .query_row("SELECT COALESCE(user_id,'') FROM tenant_owners WHERE team_id = ?1", rusqlite::params![team], |r| r.get(0))
            .unwrap_or_default();
        let owner_name: String = c
            .query_row("SELECT display_name FROM platform_users WHERE id = ?1", rusqlite::params![owner], |r| r.get(0))
            .unwrap_or_default();
        let member_count: i64 = c
            .query_row("SELECT COUNT(*) FROM members WHERE team_id = ?1 AND state NOT IN ('left','denied')", rusqlite::params![team], |r| r.get(0))
            .unwrap_or(0);
        Ok(json!({
            "team_id": team,
            "name": s["name"], "public_summary": s["public_summary"],
            "state": s["state"], "joinable": s["join_policy"] == json!("open_request"),
            "owner_name": owner_name, "member_count": member_count,
            // credit **公开分档**（design/018 SD-5/SD-16）：数据不足/未刷新 → null
            "credit_grade": crate::credit::public_grade(c, &team),
        }))
    });
    let base = match detail {
        Ok(v) => v,
        Err(_) => return (StatusCode::NOT_FOUND, err_body("team not found")).into_response(),
    };
    // 租户侧字段：**端点已配置**时经 service 车道取白名单（SD-15 租户侧裁剪）；
    // 未配置或不可达时如实说明（不假装有）。
    let mut out = base;
    // 访问者关系（design/015：owner/member 不该看到"申请加入"）
    let relation = with_conn(&ctx, |c| crate::discovery::viewer_relation(c, &team, &uid))
        .unwrap_or("none");
    out["relation"] = json!(relation);
    out["can_apply"] = json!(out["joinable"] == json!(true) && relation == "none");
    let (kc, cx) = (ctx.kubeconfig.clone(), ctx.context.clone());
    let meta = with_conn(&ctx, |c| {
        crate::provision::tenant_service_post_via_endpoint(
            c, &team, kc.as_deref(), cx.as_deref(), "public.team_meta",
            json!({ "team": team }),
        )
    });
    match meta {
        Ok(m) => {
            out["tenant_meta"] = m;
            out["tenant_meta_note"] = json!("from tenant server (whitelisted fields only)");
        }
        Err(e) => {
            out["tenant_meta"] = Value::Null;
            out["tenant_meta_note"] = json!(format!("tenant-side unavailable: {e}"));
        }
    }
    Json(json!({ "ok": true, "team": out })).into_response()
}

/// 申请加入（SD-11）。
async fn api_dir_apply(
    State(ctx): State<Arc<Ctx>>, headers: HeaderMap, Path(team): Path<String>, Json(body): Json<Value>,
) -> Response {
    if let Err(r) = guard(&ctx, &headers) { return r; }
    let (uid, _) = match require_user(&ctx, &headers) { Ok(v) => v, Err(r) => return r };
    let msg = body.get("message").and_then(Value::as_str).map(str::to_string);
    match with_conn(&ctx, |c| crate::discovery::request_join(c, &team, &uid, msg.as_deref())) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, err_body(&e)).into_response(),
    }
}

/// owner 改发现设置（SD-8/SD-16/SD-15）。
async fn api_dir_settings(
    State(ctx): State<Arc<Ctx>>, headers: HeaderMap, Path(team): Path<String>, Json(body): Json<Value>,
) -> Response {
    if let Err(r) = guard(&ctx, &headers) { return r; }
    let (uid, _) = match require_user(&ctx, &headers) { Ok(v) => v, Err(r) => return r };
    let out = with_conn(&ctx, |c| {
        if !crate::platform::tenant_owned_by(c, &team, &uid).map_err(|e| e.to_string())? {
            return Err("only the team owner may change these settings".to_string());
        }
        if let Some(v) = body.get("visibility").and_then(Value::as_str) {
            crate::discovery::set_visibility(c, &team, v)?;
        }
        if let Some(p) = body.get("join_policy").and_then(Value::as_str) {
            crate::discovery::set_join_policy(c, &team, p)?;
        }
        if let Some(s) = body.get("summary").and_then(Value::as_str) {
            crate::discovery::set_public_summary(c, &team, s)?;
        }
        crate::discovery::settings(c, &team)?.ok_or_else(|| "team not found".to_string())
    });
    // k8s label 投影（SD-24：平台库为准 → 异步语义；这里同步试一次，失败由对账补偿）
    let (kc, cx) = (ctx.kubeconfig.clone(), ctx.context.clone());
    let projection = if out.is_ok() {
        with_conn(&ctx, |c| {
            crate::discovery::project_visibility(c, &team, kc.as_deref(), cx.as_deref())
        })
        .unwrap_or_else(|e| json!({ "ok": false, "reason": e }))
    } else {
        Value::Null
    };
    match out {
        Ok(v) => Json(json!({ "ok": true, "settings": v, "projection": projection })).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, err_body(&e)).into_response(),
    }
}

/// 「我的申请」（SD-26）。
async fn api_dir_mine(State(ctx): State<Arc<Ctx>>, headers: HeaderMap) -> Response {
    if let Err(r) = guard(&ctx, &headers) { return r; }
    let (uid, _) = match require_user(&ctx, &headers) { Ok(v) => v, Err(r) => return r };
    match with_conn(&ctx, |c| crate::discovery::my_requests(c, &uid)) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, err_body(&e)).into_response(),
    }
}

async fn api_dir_withdraw(
    State(ctx): State<Arc<Ctx>>, headers: HeaderMap, Path(id): Path<String>,
) -> Response {
    if let Err(r) = guard(&ctx, &headers) { return r; }
    let (uid, _) = match require_user(&ctx, &headers) { Ok(v) => v, Err(r) => return r };
    match with_conn(&ctx, |c| crate::discovery::withdraw_request(c, &id, &uid)) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, err_body(&e)).into_response(),
    }
}

/// owner 待审列表。
async fn api_dir_pending(State(ctx): State<Arc<Ctx>>, headers: HeaderMap) -> Response {
    if let Err(r) = guard(&ctx, &headers) { return r; }
    let (uid, _) = match require_user(&ctx, &headers) { Ok(v) => v, Err(r) => return r };
    match with_conn(&ctx, |c| crate::discovery::pending_for_owner(c, &uid)) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, err_body(&e)).into_response(),
    }
}

/// owner 审批（approve/reject；批准后的 seat+邀请函+pod 见后续提交）。
async fn api_dir_decide(
    State(ctx): State<Arc<Ctx>>, headers: HeaderMap, Path(id): Path<String>, Json(body): Json<Value>,
) -> Response {
    if let Err(r) = guard(&ctx, &headers) { return r; }
    let (uid, _) = match require_user(&ctx, &headers) { Ok(v) => v, Err(r) => return r };
    let approve = body.get("approve").and_then(Value::as_bool).unwrap_or(false);
    let (kc, cx) = (ctx.kubeconfig.clone(), ctx.context.clone());
    let out = with_conn(&ctx, |c| {
        let d = crate::discovery::decide_request(c, &id, &uid, approve)?;
        if !approve {
            return Ok(d);
        }
        // 批准后的执行链（SD-11/17/23）：租户侧建座位+邀请函 → 平台侧拉 member pod。
        // 任一步失败都**如实记录**（不假装成功），且不回滚座位（SD-23）。
        let team = d["team_id"].as_str().unwrap_or("").to_string();
        let applicant = d["applicant_user_id"].as_str().unwrap_or("").to_string();
        let name = d["applicant_name"].as_str().unwrap_or("").to_string();
        match crate::provision::materialize_member_seat(
            c, &team, &applicant, &name, kc.as_deref(), cx.as_deref(),
        ) {
            Ok(m) => {
                crate::discovery::record_delivery(
                    c, &id,
                    m["member_id"].as_str().unwrap_or(""),
                    m["letter"].as_str().unwrap_or(""),
                    m["pod_state"].as_str().unwrap_or(""),
                    m["pod_detail"].as_str().unwrap_or(""),
                )?;
                let mut d2 = d.clone();
                d2["delivery"] = m;
                Ok(d2)
            }
            Err(e) => {
                crate::discovery::record_delivery(c, &id, "", "", "failed", &e)?;
                let mut d2 = d.clone();
                d2["delivery"] = json!({ "ok": false, "pod_state": "failed", "pod_detail": e });
                Ok(d2)
            }
        }
    });
    match out {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, err_body(&e)).into_response(),
    }
}

/// 用户自助 team Phase 2：只看名下租户（含配额用量）。
async fn api_my_teams(State(ctx): State<Arc<Ctx>>, headers: HeaderMap) -> Response {
    if let Err(r) = guard(&ctx, &headers) {
        return r;
    }
    let Some((uid, _)) = session_user(&ctx, &headers) else {
        // 运维走 /api/tenants（全量）；my/* 是用户面。
        return (
            StatusCode::FORBIDDEN,
            err_body("user session required"),
        )
            .into_response();
    };
    let (kc, cx) = (ctx.kubeconfig.clone(), ctx.context.clone());
    match with_conn(&ctx, |c| {
        crate::provision::my_teams(c, &uid, kc.as_deref(), cx.as_deref())
    }) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, err_body(&e)).into_response(),
    }
}

// ---------------------------------------------------------------------------
// credit（design/018 SD-16）：团队信用实页；对外只给分档，成员/owner 看分数+构成
// ---------------------------------------------------------------------------

/// 我的团队 credit：每个团队的分档 + **精确分与构成明细**（调用方是成员/owner，SD-5）。
/// 同时返回该团队是否已物化（未刷新 → 提示"待刷新"）。
async fn api_my_credit(State(ctx): State<Arc<Ctx>>, headers: HeaderMap) -> Response {
    if let Err(r) = guard(&ctx, &headers) {
        return r;
    }
    let Some((uid, _)) = session_user(&ctx, &headers) else {
        return (
            StatusCode::FORBIDDEN,
            err_body("user session required"),
        )
            .into_response();
    };
    let (kc, cx) = (ctx.kubeconfig.clone(), ctx.context.clone());
    match with_conn(&ctx, |c| {
        let mine = crate::provision::my_teams(c, &uid, kc.as_deref(), cx.as_deref())?;
        let mut teams: Vec<Value> = Vec::new();
        for group in ["owned", "joined"] {
            let relation = if group == "owned" { "owner" } else { "member" };
            for t in mine.get(group).and_then(Value::as_array).cloned().unwrap_or_default() {
                let tid = t["team_id"].as_str().unwrap_or("").to_string();
                if tid.is_empty() {
                    continue;
                }
                let snap = crate::credit::snapshot(c, &tid)?;
                teams.push(json!({
                    "team_id": tid,
                    "team_name": t["team_name"].clone(),
                    "relation": relation,
                    "credit": snap,
                    "grade": snap.as_ref().map(|v| v["grade"].clone()).unwrap_or(Value::Null),
                    "insufficient": snap.as_ref().map(|v| v["insufficient"].clone()).unwrap_or(json!(true)),
                    "awaiting_refresh": snap.is_none(),
                }));
            }
        }
        Ok(json!({
            "ok": true, "teams": teams,
            "model_version": crate::credit::MODEL_VERSION,
            "window_days": crate::credit::WINDOW_DAYS,
            "min_events": crate::credit::MIN_EVENTS,
            "baseline": crate::credit::BASELINE,
        }))
    }) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, err_body(&e)).into_response(),
    }
}

/// 单团队 credit 明细（成员/owner 可见精确分 + 构成）；非成员 400。
async fn api_my_credit_team(
    State(ctx): State<Arc<Ctx>>,
    headers: HeaderMap,
    Path(team): Path<String>,
) -> Response {
    if let Err(r) = guard(&ctx, &headers) {
        return r;
    }
    let Some((uid, _)) = session_user(&ctx, &headers) else {
        return (
            StatusCode::FORBIDDEN,
            err_body("user session required"),
        )
            .into_response();
    };
    let (kc, cx) = (ctx.kubeconfig.clone(), ctx.context.clone());
    match with_conn(&ctx, |c| {
        let mine = crate::provision::my_teams(c, &uid, kc.as_deref(), cx.as_deref())?;
        let mut relation = "";
        for (g, r) in [("owned", "owner"), ("joined", "member")] {
            if mine
                .get(g)
                .and_then(Value::as_array)
                .map(|xs| xs.iter().any(|x| x["team_id"].as_str() == Some(team.as_str())))
                .unwrap_or(false)
            {
                relation = r;
            }
        }
        if relation.is_empty() {
            return Err("not a member of this team".to_string());
        }
        let mut v = crate::provision::credit_view(c, &team)?;
        v["relation"] = json!(relation);
        Ok(v)
    }) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, err_body(&e)).into_response(),
    }
}

/// 刷新 credit 快照（owner-only；经自动通道拉租户侧行为计数）。
async fn api_my_credit_refresh(
    State(ctx): State<Arc<Ctx>>,
    headers: HeaderMap,
    Path(team): Path<String>,
) -> Response {
    if let Err(r) = guard(&ctx, &headers) {
        return r;
    }
    let Some((uid, _)) = session_user(&ctx, &headers) else {
        return (
            StatusCode::FORBIDDEN,
            err_body("user session required"),
        )
            .into_response();
    };
    let (kc, cx) = (ctx.kubeconfig.clone(), ctx.context.clone());
    match with_conn(&ctx, |c| {
        let owned = crate::platform::owned_teams(c, &uid).unwrap_or_default();
        if !owned.iter().any(|t| t == &team) {
            return Err("only the owner can refresh this team's credit".to_string());
        }
        crate::provision::refresh_credit(c, &team, kc.as_deref(), cx.as_deref())
    }) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, err_body(&e)).into_response(),
    }
}

// ---------------------------------------------------------------------------
// TCoin（design/017 SD-15）：余额 + 流水实页；成员看自己 / owner 看全租户
// ---------------------------------------------------------------------------

/// 我的 TCoin 概览：我参与的每个团队的**平台余额**、**我自己的分配余额**、**累计消耗**。
async fn api_my_tcoin(State(ctx): State<Arc<Ctx>>, headers: HeaderMap) -> Response {
    if let Err(r) = guard(&ctx, &headers) {
        return r;
    }
    let Some((uid, _)) = session_user(&ctx, &headers) else {
        return (
            StatusCode::FORBIDDEN,
            err_body("user session required"),
        )
            .into_response();
    };
    let (kc, cx) = (ctx.kubeconfig.clone(), ctx.context.clone());
    match with_conn(&ctx, |c| {
        let mine = crate::provision::my_teams(c, &uid, kc.as_deref(), cx.as_deref())?;
        let mut teams: Vec<Value> = Vec::new();
        for group in ["owned", "joined"] {
            let relation = if group == "owned" { "owner" } else { "member" };
            for t in mine.get(group).and_then(Value::as_array).cloned().unwrap_or_default() {
                let tid = t["team_id"].as_str().unwrap_or("").to_string();
                if tid.is_empty() {
                    continue;
                }
                let mid = t["member_id"].as_str().unwrap_or("").to_string();
                let balance = crate::tcoin::balance(c, &tid, "tenant")?;
                let my_balance = if mid.is_empty() {
                    Value::Null
                } else {
                    json!(crate::tcoin::balance(c, &tid, &format!("member:{mid}"))?)
                };
                teams.push(json!({
                    "team_id": tid,
                    "team_name": t["team_name"].clone(),
                    "relation": relation,
                    "member_id": mid,
                    "balance": balance,
                    "my_balance": my_balance,
                    "consumed_total": crate::tcoin::consumed_total(c, &tid)?,
                    "warning": if balance < 0 { "余额为负（欠费，v1 不阻断）" }
                               else if balance == 0 { "余额为 0" } else { "" },
                }));
            }
        }
        // 全局流水（跨我的团队），供实页顶部「最近动态」
        let mut recent: Vec<Value> = Vec::new();
        for t in &teams {
            let tid = t["team_id"].as_str().unwrap_or("");
            for mut e in crate::tcoin::entries(c, tid, None, 20)? {
                e["team_id"] = json!(tid);
                e["team_name"] = t["team_name"].clone();
                recent.push(e);
            }
        }
        recent.sort_by(|a, b| b["seq"].as_i64().cmp(&a["seq"].as_i64()));
        recent.truncate(30);
        Ok(json!({ "ok": true, "rate": crate::tcoin::TCOIN_PER_USD, "teams": teams, "recent": recent }))
    }) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, err_body(&e)).into_response(),
    }
}

/// 单团队 TCoin 明细：平台总账流水 + 成员余额 + 租户侧未结算用量（经自动通道）。
async fn api_my_tcoin_team(
    State(ctx): State<Arc<Ctx>>,
    headers: HeaderMap,
    Path(team): Path<String>,
) -> Response {
    if let Err(r) = guard(&ctx, &headers) {
        return r;
    }
    let Some((uid, _)) = session_user(&ctx, &headers) else {
        return (
            StatusCode::FORBIDDEN,
            err_body("user session required"),
        )
            .into_response();
    };
    let (kc, cx) = (ctx.kubeconfig.clone(), ctx.context.clone());
    match with_conn(&ctx, |c| {
        // 归属校验：只能看自己参与的团队（成员看自己 / owner 看全租户 SD-15）
        let mine = crate::provision::my_teams(c, &uid, kc.as_deref(), cx.as_deref())?;
        let mut relation = "";
        for (g, r) in [("owned", "owner"), ("joined", "member")] {
            if mine
                .get(g)
                .and_then(Value::as_array)
                .map(|xs| xs.iter().any(|x| x["team_id"].as_str() == Some(team.as_str())))
                .unwrap_or(false)
            {
                relation = r;
            }
        }
        if relation.is_empty() {
            return Err("not a member of this team".to_string());
        }
        let mut ov = crate::provision::tcoin_overview(c, &team, kc.as_deref(), cx.as_deref())?;
        ov["relation"] = json!(relation);
        ov["consumed_total"] = json!(crate::tcoin::consumed_total(c, &team)?);
        ov["rate"] = json!(crate::tcoin::TCOIN_PER_USD);
        // owner：附各成员分配余额
        if relation == "owner" {
            let mut alloc: Vec<Value> = Vec::new();
            let mut stmt = c
                .prepare(
                    "SELECT account, COALESCE(SUM(delta),0) bal FROM tcoin_entries
                     WHERE team_id = ?1 AND account LIKE 'member:%' GROUP BY account ORDER BY bal DESC",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(rusqlite::params![team], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
                })
                .map_err(|e| e.to_string())?;
            for row in rows {
                let (acct, bal) = row.map_err(|e| e.to_string())?;
                alloc.push(json!({ "member_id": acct.trim_start_matches("member:"), "balance": bal }));
            }
            ov["allocations"] = json!(alloc);
        }
        Ok(ov)
    }) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, err_body(&e)).into_response(),
    }
}

/// owner 结算：拉取租户侧未结算消耗，写入平台总账（两级账本对账动作 SD-14）。
async fn api_my_tcoin_settle(
    State(ctx): State<Arc<Ctx>>,
    headers: HeaderMap,
    Path(team): Path<String>,
) -> Response {
    if let Err(r) = guard(&ctx, &headers) {
        return r;
    }
    let Some((uid, _)) = session_user(&ctx, &headers) else {
        return (
            StatusCode::FORBIDDEN,
            err_body("user session required"),
        )
            .into_response();
    };
    let (kc, cx) = (ctx.kubeconfig.clone(), ctx.context.clone());
    match with_conn(&ctx, |c| {
        // owner-only
        let owned = crate::platform::owned_teams(c, &uid).unwrap_or_default();
        if !owned.iter().any(|t| t == &team) {
            return Err("only the owner can settle this team".to_string());
        }
        crate::provision::settle_team_tcoin(c, &team, kc.as_deref(), cx.as_deref())
    }) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, err_body(&e)).into_response(),
    }
}

/// **我的**云 member console 地址（design/021 §J）：点开即进入**云上 member Pod 里的 `teamx console`**
/// （本地 `teamx console` 是另一回事）。链接改为**平台反代路径** `/c/<member16>`：
/// 浏览器 URL 里**不含 token**；平台会话鉴权后由平台在上游请求注入 console token。
async fn api_my_team_console(
    State(ctx): State<Arc<Ctx>>,
    headers: HeaderMap,
    Path(team): Path<String>,
) -> Response {
    if let Err(r) = guard(&ctx, &headers) {
        return r;
    }
    let Some((uid, _)) = session_user(&ctx, &headers) else {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"ok": false, "error": "user session required"})),
        )
            .into_response();
    };
    match with_conn(&ctx, |c| {
        let seat = my_seat_id(c, &uid, &team)?;
        let Some(seat) = seat else {
            return Err("no member seat for this user in this team".to_string());
        };
        let short = crate::k8s::short_id(&seat);
        Ok(json!({
            "ok": true, "team_id": team, "member_id": seat,
            "url": format!("/c/{short}"),
            "kind": "cloud-member-console",
        }))
    }) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": e}))).into_response(),
    }
}

/// 从 `/c/<member16>...` 路径取出 `member16`（非本前缀 → None）。
fn member16_from_path(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/c/")?;
    let seg = rest.split('/').next()?;
    if seg.is_empty() {
        None
    } else {
        Some(seg.to_string())
    }
}

fn is_ws_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false)
}

/// 在**当前用户自己的席位**里按 `member16` 匹配 `(team_id, member_id)`。
///
/// 只放开本人席位（owner 的 `tenants.owner_member_id` + 已批准申请的 `team_join_requests.member_id`）：
/// console token 是确定性的、等于该成员的完整身份，跨成员放开等同越权。
fn resolve_seat(
    conn: &rusqlite::Connection,
    uid: &str,
    m16: &str,
) -> Result<Option<(String, String)>, String> {
    let mut candidates: Vec<(String, String)> = Vec::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT t.team_id, t.owner_member_id FROM tenants t
                 JOIN tenant_owners o ON o.team_id = t.team_id
                 WHERE o.user_id = ?1 AND t.owner_member_id <> ''",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params![uid], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            candidates.push(row.map_err(|e| e.to_string())?);
        }
    }
    {
        let mut stmt = conn
            .prepare(
                "SELECT team_id, COALESCE(member_id,'') FROM team_join_requests
                 WHERE user_id = ?1 AND status = 'approved'",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params![uid], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (t, m) = row.map_err(|e| e.to_string())?;
            if !m.is_empty() {
                candidates.push((t, m));
            }
        }
    }
    Ok(candidates
        .into_iter()
        .find(|(_, m)| crate::console_proxy::member16(m) == m16))
}

/// 解析成员 console 的上游目标：测试覆盖优先，否则经 port-forward 拨号 ClusterIP。
async fn resolve_console_target(
    ctx: &Arc<Ctx>,
    team_id: &str,
    member_id: &str,
) -> Result<crate::console_proxy::Target, String> {
    if let Some(base) = &ctx.console_target_override {
        return crate::console_proxy::parse_target(base);
    }
    let (kc, cx) = (ctx.kubeconfig.clone(), ctx.context.clone());
    let (t, m) = (team_id.to_string(), member_id.to_string());
    let port = tokio::task::spawn_blocking(move || {
        crate::console_proxy::ensure_forward(&t, &m, kc.as_deref(), cx.as_deref())
    })
    .await
    .map_err(|e| format!("console forward task panicked: {e}"))??;
    Ok(crate::console_proxy::Target {
        host: "127.0.0.1".to_string(),
        port,
        tls: true,
    })
}

/// `GET|POST|... /c/{member16}[/...]` —— 平台控制台反代成员 console（design/021 §J）。
///
/// - 鉴权 = **平台会话**：未登录 303 跳平台登录页（与控制台一致）。
/// - 只反代**本人席位**的 console（见 `resolve_seat`）。
/// - console token 只注入上游 `Cookie`；浏览器 URL/页面/响应头里都不出现
///   （`Set-Cookie` 在上游响应里去丢弃，见 `console_proxy`）。
/// - **含 WebSocket upgrade**（ttyd 终端等）：显式解析 `WebSocketUpgrade` 后走 `serve_ws`。
async fn api_console_proxy(
    State(ctx): State<Arc<Ctx>>,
    headers: HeaderMap,
    request: Request<Body>,
) -> Response {
    let path = request.uri().path().to_string();
    let Some(m16) = member16_from_path(&path) else {
        return (StatusCode::NOT_FOUND, err_body("not found")).into_response();
    };
    let Some((uid, _)) = session_user(&ctx, &headers) else {
        // 页面导航要的是登录页，不是裸 401 JSON。
        let mut resp = axum::response::Redirect::to(&ctx.console_page).into_response();
        *resp.status_mut() = StatusCode::SEE_OTHER;
        return resp;
    };
    let seat = with_conn(&ctx, |c| resolve_seat(c, &uid, &m16));
    let Some((team_id, member_id)) = seat.ok().flatten() else {
        return (StatusCode::NOT_FOUND, err_body("no such console")).into_response();
    };
    let token = crate::k8s::console_token_for(&member_id);
    let target = match resolve_console_target(&ctx, &team_id, &member_id).await {
        Ok(t) => t,
        Err(e) => return (StatusCode::BAD_GATEWAY, err_body(&e)).into_response(),
    };
    let rest = crate::console_proxy::strip_prefix(&m16, &path);
    let query = request.uri().query().map(str::to_string);
    let up_path = crate::console_proxy::upstream_path(&rest, query.as_deref());
    if is_ws_upgrade(&headers) {
        let (mut parts, _body) = request.into_parts();
        return match WebSocketUpgrade::from_request_parts(&mut parts, &ctx).await {
            Ok(ws) => crate::console_proxy::serve_ws(target, up_path, token, ws).await,
            Err(_) => (StatusCode::BAD_REQUEST, err_body("bad websocket upgrade")).into_response(),
        };
    }
    let method = request.method().clone();
    crate::console_proxy::serve_http(target, m16, method, up_path, headers, request, token).await
}

/// 当前用户在某团队的**席位 id**（owner 看 `tenants.owner_member_id`；成员看已批准申请的 `member_id`）。
fn my_seat_id(conn: &rusqlite::Connection, uid: &str, team: &str) -> Result<Option<String>, String> {
    if crate::platform::tenant_owned_by(conn, team, uid).unwrap_or(false) {
        let seat: Option<String> = conn
            .query_row(
                "SELECT COALESCE(owner_member_id,'') FROM tenants WHERE team_id = ?1",
                rusqlite::params![team],
                |r| r.get(0),
            )
            .ok();
        if let Some(s) = seat.filter(|s| !s.is_empty()) {
            return Ok(Some(s));
        }
    }
    let seat: Option<String> = conn
        .query_row(
            "SELECT COALESCE(member_id,'') FROM team_join_requests
             WHERE team_id = ?1 AND user_id = ?2 AND status = 'approved'
             ORDER BY created_at DESC LIMIT 1",
            rusqlite::params![team, uid],
            |r| r.get(0),
        )
        .ok();
    Ok(seat.filter(|s| !s.is_empty()))
}

/// 我的席位显示名（平台侧权威；租户 roster 不返回 member_id，只能用名字配）。
fn my_seat_name(conn: &rusqlite::Connection, uid: &str, team: &str) -> Option<String> {
    let seat = my_seat_id(conn, uid, team).ok().flatten()?;
    conn.query_row(
        "SELECT display_name FROM members WHERE id = ?1",
        rusqlite::params![seat],
        |r| r.get::<_, String>(0),
    )
    .ok()
}

/// 成员名册（design/016 SD-12）：owner 只读，租户侧裁剪（无凭据）。
async fn api_my_roster(State(ctx): State<Arc<Ctx>>, headers: HeaderMap, Path(team): Path<String>) -> Response {
    if let Err(r) = guard(&ctx, &headers) { return r; }
    let (uid, _) = match require_user(&ctx, &headers) { Ok(v) => v, Err(r) => return r };
    let (kc, cx) = (ctx.kubeconfig.clone(), ctx.context.clone());
    let out = with_conn(&ctx, |c| {
        if !crate::platform::tenant_owned_by(c, &team, &uid).map_err(|e| e.to_string())? {
            return Err("only the team owner may view the roster".to_string());
        }
        let mut v = crate::provision::tenant_service_post_via_endpoint(
            c, &team, kc.as_deref(), cx.as_deref(), "team.roster", json!({ "team": team }),
        )?;
        // 标出"哪一行是我" → 该行渲染「Teamx Console」链接（云 member 的 console）
        let mine = my_seat_id(c, &uid, &team)?;
        let my_name = my_seat_name(c, &uid, &team);
        if let Some(rows) = v["members"].as_array_mut() {
            for r in rows.iter_mut() {
                let by_id = mine
                    .as_deref()
                    .map(|m| !m.is_empty() && r["member_id"].as_str() == Some(m))
                    .unwrap_or(false);
                let by_name = my_name
                    .as_deref()
                    .map(|n| !n.is_empty() && r["display_name"].as_str() == Some(n))
                    .unwrap_or(false);
                r["is_me"] = json!(by_id || by_name);
            }
        }
        Ok::<Value, String>(v)
    });
    match out {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, err_body(&e)).into_response(),
    }
}

/// owner 邀请成员（design/016 SD-12）：service 车道，返回邀请函供复制。
async fn api_my_invite(State(ctx): State<Arc<Ctx>>, headers: HeaderMap, Path(team): Path<String>, Json(body): Json<Value>) -> Response {
    if let Err(r) = guard(&ctx, &headers) { return r; }
    let (uid, _) = match require_user(&ctx, &headers) { Ok(v) => v, Err(r) => return r };
    let role_desc = body.get("role_desc").and_then(Value::as_str).unwrap_or("contributor: 成员").to_string();
    let (kc, cx) = (ctx.kubeconfig.clone(), ctx.context.clone());
    let out = with_conn(&ctx, |c| {
        if !crate::platform::tenant_owned_by(c, &team, &uid).map_err(|e| e.to_string())? {
            return Err("only the team owner may invite".to_string());
        }
        crate::provision::tenant_service_post_via_endpoint(
            c, &team, kc.as_deref(), cx.as_deref(), "team.invite",
            json!({ "team": team, "role_desc": role_desc, "auto_approve": false }),
        )
    });
    match out {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, err_body(&e)).into_response(),
    }
}

/// 离开团队（design/016 SD-11 / ADR-0002）：双侧真释放。
async fn api_my_leave(State(ctx): State<Arc<Ctx>>, headers: HeaderMap, Path(team): Path<String>) -> Response {
    if let Err(r) = guard(&ctx, &headers) { return r; }
    let (uid, _) = match require_user(&ctx, &headers) { Ok(v) => v, Err(r) => return r };
    let (kc, cx) = (ctx.kubeconfig.clone(), ctx.context.clone());
    match with_conn(&ctx, |c| crate::provision::leave_team(c, &team, &uid, kc.as_deref(), cx.as_deref())) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, err_body(&e)).into_response(),
    }
}

/// Owner 邀请函取件页（design/016 SD-10）。
async fn api_my_letters(State(ctx): State<Arc<Ctx>>, headers: HeaderMap) -> Response {
    if let Err(r) = guard(&ctx, &headers) { return r; }
    let (uid, _) = match require_user(&ctx, &headers) { Ok(v) => v, Err(r) => return r };
    match with_conn(&ctx, |c| crate::provision::my_letters(c, &uid)) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, err_body(&e)).into_response(),
    }
}

/// 用户自助建 team（O-1(a) 全新开始入口）：配额 → 空壳 team → 租户 infra → 标记归属。
/// server 侧 team 自举是第二步（server Pod 启动要 ~1 分钟），见 bootstrap 路由。
async fn api_my_teams_create(State(ctx): State<Arc<Ctx>>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    if let Err(r) = guard(&ctx, &headers) {
        return r;
    }
    let Some((uid, _)) = session_user(&ctx, &headers) else {
        return (
            StatusCode::FORBIDDEN,
            err_body("user session required"),
        )
            .into_response();
    };
    let name = body.get("name").and_then(Value::as_str).unwrap_or("").to_string();
    // M2（weiye-mini task-103）：用户面**不接受** server_image（镜像由平台 pin）。
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, err_body("name is required")).into_response();
    }
    // design/016 SD-10：创建表单一步带上发现设置
    let vis = body.get("visibility").and_then(Value::as_str).map(str::to_string);
    let jp = body.get("join_policy").and_then(Value::as_str).map(str::to_string);
    let sum = body.get("summary").and_then(Value::as_str).map(str::to_string);
    let (kc, cx) = (ctx.kubeconfig.clone(), ctx.context.clone());
    match with_conn(&ctx, |c| {
        crate::provision::create_my_tenant_with(
            c, &uid, &name, vis.as_deref(), jp.as_deref(), sum.as_deref(), kc.as_deref(), cx.as_deref(),
        )
    }) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, err_body(&e)).into_response(),
    }
}

/// 自助 team 第二步：server 就绪后经 service 车道初始化团队（建首 team + owner 邀请）。
async fn api_my_teams_bootstrap(
    State(ctx): State<Arc<Ctx>>,
    headers: HeaderMap,
    Path(team): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    if let Err(r) = guard(&ctx, &headers) {
        return r;
    }
    let Some((uid, _)) = session_user(&ctx, &headers) else {
        return (
            StatusCode::FORBIDDEN,
            err_body("user session required"),
        )
            .into_response();
    };
    // 先归属（403），后参数（400）：越权者无论参数对错都先吃 403。
    if let Err(r) = own_guard(&ctx, &headers, &team) {
        return r;
    }
    // server_url 可选：留空时由平台自动建立通道（同机 port-forward），并把可用地址写进 letter
    let server_url = body.get("server_url").and_then(Value::as_str).unwrap_or("").to_string();
    let (kc, cx) = (ctx.kubeconfig.clone(), ctx.context.clone());
    match with_conn(&ctx, |c| {
        crate::provision::bootstrap_my_team(c, &team, &uid, &server_url, kc.as_deref(), cx.as_deref())
    }) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, err_body(&e)).into_response(),
    }
}

/// 平台服务车道调用：读租户 CA（namespace 的 Secret）→ 用租户 CA 签**平台服务证书** →
/// mTLS 调租户 server 的 `/rpc`（方法白名单在 serve 侧 fail-closed 强制）。
async fn service_call(ctx: &Ctx, team: &str, method: &str, args: Value) -> Response {
    let tenant = match with_conn(ctx, |c| crate::provision::tenant_get(c, team)) {
        Ok(Some(t)) => t,
        Ok(None) => return (StatusCode::NOT_FOUND, Json(json!({"ok": false, "error": format!("tenant {team} not found")}))).into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, err_body(&e)).into_response(),
    };
    let ns = tenant["namespace"].as_str().unwrap_or("").to_string();
    let ca_secret = format!("teamx-ca-{}", crate::k8s::short_id(team));
    let kubectl = crate::k8s::Kubectl::new(ctx.kubeconfig.clone(), ctx.context.clone());
    // 1) 取租户 CA（平台生成时写入租户 namespace 的 Secret）
    let secret = match kubectl.get_json("secret", &ca_secret, &ns) {
        Ok(Some(v)) => v,
        Ok(None) => return (StatusCode::BAD_GATEWAY, Json(json!({"ok": false, "error": format!("tenant CA secret {ca_secret} not found in {ns}")}))).into_response(),
        Err(e) => return (StatusCode::BAD_GATEWAY, err_body(&e)).into_response(),
    };
    let dec = |k: &str| -> Option<String> {
        secret["data"][k]
            .as_str()
            .and_then(base64_decode)
            .map(|b| String::from_utf8_lossy(&b).to_string())
    };
    let (ca_crt, ca_key) = match (dec("ca.crt"), dec("ca.key")) {
        (Some(a), Some(b)) => (a, b),
        _ => return (StatusCode::BAD_GATEWAY, err_body("tenant CA material missing in Secret")).into_response(),
    };
    // 2) 用租户 CA 签平台服务证书（service:platform-console:<instance>）
    // instance 只取 token 前缀（CN 由签发函数组装为 service:platform-console:<instance>，调用方不得自带前缀）
    let svc_cn = format!("console-{}", &ctx.launch_token[..8.min(ctx.launch_token.len())]);
    let svc = match crate::pki::issue_service_cert_with_ca(&ca_crt, &ca_key, "platform-console", &svc_cn) {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, err_body(&e)).into_response(),
    };
    // O-2 台账：平台服务证书（短 TTL，每次调用重签）也落一行 —— 否则"旧 CA 活数"永远少算。
    // 记录失败不阻断本次调用（台账是审计，不是门禁），但要写日志。
    {
        let r = with_conn(ctx, |c| {
            crate::issued::record(
                c, &svc.serial_hex, &svc.cn, "service", "platform-console", "", team,
                &crate::pki::ca_fingerprint_of(&ca_crt).unwrap_or_default(),
                crate::pki::SERVICE_CERT_DAYS, "platform-console",
            )
        });
        if let Err(e) = r {
            eprintln!("platform-console: issued.record failed: {e}");
        }
    }
    // 3) mTLS 调租户 server（集群内 Service DNS）
    let url = format!("https://teamx-serve-{}.{ns}.svc:5783", crate::k8s::short_id(team));
    let body = json!({ "method": method, "args": args }).to_string();
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let mtls = crate::tunnel_client::material_from_pem(&svc.cert_pem, &svc.key_pem, &ca_crt, "platform-console")
            .ok_or_else(|| "build service mTLS material failed".to_string())?;
        let cfg = crate::tunnel_client::client_config(&mtls)?;
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
        let resp = rt.block_on(crate::tunnel_client::https_post_shared(&url, "/rpc", &body, &cfg, 15))?;
        serde_json::from_str(&resp).map_err(|e| format!("bad response: {e}"))
    })
    .await;
    match out {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => (StatusCode::BAD_GATEWAY, err_body(&e)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": format!("join: {e}")}))).into_response(),
    }
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

// ---------------------------------------------------------------------------
// 平台面只读视图
// ---------------------------------------------------------------------------

async fn api_tokens(State(ctx): State<Arc<Ctx>>, headers: HeaderMap) -> Response {
    if let Err(r) = guard(&ctx, &headers) { return r; }
    match with_conn(&ctx, |c| crate::platform::list_tokens(c)) {
        Ok(v) => Json(json!({"ok": true, "tokens": v})).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, err_body(&e)).into_response(),
    }
}

async fn api_admins(State(ctx): State<Arc<Ctx>>, headers: HeaderMap) -> Response {
    if let Err(r) = guard(&ctx, &headers) { return r; }
    match with_conn(&ctx, |c| crate::platform::list_admins(c)) {
        Ok(v) => Json(json!({"ok": true, "admins": v})).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, err_body(&e)).into_response(),
    }
}

async fn api_audit(State(ctx): State<Arc<Ctx>>, headers: HeaderMap) -> Response {
    if let Err(r) = guard(&ctx, &headers) { return r; }
    match with_conn(&ctx, |c| crate::platform::audit_log(c, 100)) {
        Ok(v) => Json(json!({"ok": true, "audit": v})).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, err_body(&e)).into_response(),
    }
}

// ---------------------------------------------------------------------------
// GitHub OAuth（D8：与 #8 同一 GitHub App；配置齐才启用）
// ---------------------------------------------------------------------------

fn github_configured() -> bool {
    std::env::var("TEAMX_GITHUB_CLIENT_ID").map(|s| !s.is_empty()).unwrap_or(false)
        && std::env::var("TEAMX_GITHUB_CLIENT_SECRET").map(|s| !s.is_empty()).unwrap_or(false)
}

// ---------------------------------------------------------------------------
// GitHub OAuth 真流程 Phase 3（D8：与 #8 同一 GitHub App；配置齐才启用）。
//
// - 地址可覆盖（测试/企业版 GitHub）：TEAMX_GITHUB_BASE（默认 https://github.com，
//   授权 + 换 token）与 TEAMX_GITHUB_API_BASE（默认 https://api.github.com，用户信息）。
// - state 单次有效 + 10 分钟过期（CSRF）；内存态，重启作废。
// - 回调全程 fail-closed：任何一步失败都明确报错，不写半截会话。
// ---------------------------------------------------------------------------

/// OAuth 端点基地址（默认 github.com；mock 测试可覆盖）。
fn github_base() -> String {
    std::env::var("TEAMX_GITHUB_BASE").unwrap_or_else(|_| "https://github.com".to_string())
}
fn github_api_base() -> String {
    std::env::var("TEAMX_GITHUB_API_BASE").unwrap_or_else(|_| "https://api.github.com".to_string())
}

/// 发起 state（单次有效）。
fn oauth_issue(ctx: &Ctx) -> String {
    let state = crate::util::gen_access_token(24);
    if let Ok(mut m) = ctx.oauth_states.lock() {
        let now = std::time::Instant::now();
        m.retain(|_, t| now.duration_since(*t).as_secs() < 600);
        m.insert(state.clone(), now);
    }
    state
}

/// 消费 state（不存在/过期/复用一律 false）。
fn oauth_consume(ctx: &Ctx, state: &str) -> bool {
    if state.is_empty() {
        return false;
    }
    match ctx.oauth_states.lock() {
        Ok(mut m) => matches!(m.remove(state), Some(t) if t.elapsed().as_secs() < 600),
        Err(_) => false,
    }
}

/// 登录入口（公开 —— OAuth 本身就是登录方式，不要守卫）。
/// 302 到 GitHub 授权页（浏览器跟随；用户在 GitHub 完成授权）。
async fn api_oauth_start(State(ctx): State<Arc<Ctx>>) -> Response {
    if !github_configured() {
        return (StatusCode::SERVICE_UNAVAILABLE, err_body("GitHub OAuth not configured (set TEAMX_GITHUB_CLIENT_ID/SECRET)")).into_response();
    }
    let client_id = std::env::var("TEAMX_GITHUB_CLIENT_ID").unwrap_or_default();
    let state = oauth_issue(&ctx);
    let url = format!(
        "{}/login/oauth/authorize?client_id={client_id}&state={state}&scope=read:user",
        github_base()
    );
    axum::response::Redirect::to(&url).into_response()
}

/// 回调（公开）：验 state → 换 token → 取用户信息 → 绑定/新建用户 → 发会话 → 回 /。
async fn api_oauth_callback(
    State(ctx): State<Arc<Ctx>>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    if !github_configured() {
        return (StatusCode::SERVICE_UNAVAILABLE, err_body("GitHub OAuth not configured")).into_response();
    }
    let state = q.get("state").cloned().unwrap_or_default();
    if !oauth_consume(&ctx, &state) {
        return (StatusCode::BAD_REQUEST, err_body("bad or expired state (CSRF)")).into_response();
    }
    if let Some(e) = q.get("error") {
        return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": format!("github refused: {e}")}))).into_response();
    }
    let code = q.get("code").cloned().unwrap_or_default();
    if code.is_empty() {
        return (StatusCode::BAD_REQUEST, err_body("missing code")).into_response();
    }
    // 换 token + 取用户：阻塞 HTTP（ureq）跑在 spawn_blocking（async 上下文禁直接阻塞）。
    // secret 只出现在发往 GitHub 的请求体里，永不进日志/审计/响应。
    let client_id = std::env::var("TEAMX_GITHUB_CLIENT_ID").unwrap_or_default();
    let client_secret = std::env::var("TEAMX_GITHUB_CLIENT_SECRET").unwrap_or_default();
    let base = github_base();
    let api_base = github_api_base();
    let fetched = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(20)).build();
        let tok: Value = agent
            .post(&format!("{base}/login/oauth/access_token"))
            .set("Accept", "application/json")
            .send_json(serde_json::json!({
                "client_id": client_id, "client_secret": client_secret, "code": code,
            }))
            .map_err(|e| format!("token exchange failed: {e}"))?
            .into_json()
            .map_err(|e| format!("token response parse failed: {e}"))?;
        if let Some(e) = tok.get("error_description").or_else(|| tok.get("error")) {
            return Err(format!("github token error: {e}"));
        }
        let access = tok
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| "github did not return access_token".to_string())?;
        let me: Value = agent
            .get(&format!("{api_base}/user"))
            .set("Authorization", &format!("Bearer {access}"))
            .set("Accept", "application/vnd.github+json")
            .call()
            .map_err(|e| format!("github user fetch failed: {e}"))?
            .into_json()
            .map_err(|e| format!("github user parse failed: {e}"))?;
        Ok(me)
    })
    .await;
    let fetched: Result<Value, String> = match fetched {
        Ok(inner) => inner,
        Err(e) => Err(format!("oauth task failed: {e}")),
    };
    let me = match fetched {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::BAD_GATEWAY, err_body(&e)).into_response()
        }
    };
    let gid = me.get("id").map(|v| v.to_string()).unwrap_or_default();
    let login = me.get("login").and_then(Value::as_str).unwrap_or("").to_string();
    if gid.is_empty() || gid == "null" || login.is_empty() {
        return (StatusCode::BAD_GATEWAY, err_body("github user missing id/login")).into_response();
    }
    // 绑定/新建用户（github_id 锚定；禁用用户拒绝；重名加 -gh 后缀，不自动并户）。
    let user = match with_conn(&ctx, |c| crate::platform::find_or_create_github_user(c, &gid, &login)) {
        Ok(u) => u,
        Err(e) => {
            return (StatusCode::FORBIDDEN, err_body(&e)).into_response()
        }
    };
    let uid = user["user_id"].as_str().unwrap_or("").to_string();
    let uname = user["display_name"].as_str().unwrap_or("").to_string();
    let sess = match with_conn(&ctx, |c| crate::platform::issue_session(c, &uid)) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, err_body(&e)).into_response()
        }
    };
    let token = sess["token"].as_str().unwrap_or("").to_string();
    let _ = with_conn(&ctx, |c| {
        crate::platform::audit(c, &format!("user:{uname}"), "platform_user.github_login", None)
    });
    let mut resp = axum::response::Redirect::to("/").into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        // Lax（非 Strict）：OAuth 回调是跨站 302 回来，Strict 会丢 cookie；
        // 已有 state 单次校验防 CSRF，此处放宽是刻意的（task-103 建议 1）。
        format!("teamx_platform_token={token}; Path=/; HttpOnly; Secure; SameSite=Lax")
            .parse()
            .unwrap(),
    );
    resp
}


/// 平台控制台页面（单文件；平台面：租户 / token / 管理员 / 审计）。
const INDEX_HTML: &str = r#"<!doctype html>
<html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Teamx Cloud · 平台控制台</title>
<style>
 *{box-sizing:border-box;margin:0;padding:0}
 body{font-family:"Microsoft YaHei","PingFang SC",-apple-system,BlinkMacSystemFont,sans-serif;background:#f8f8f8;color:#333;min-height:100vh;display:flex;flex-direction:column}
 header{height:60px;background:#fff;border-bottom:1px solid #EBEEF5;display:flex;align-items:center;justify-content:space-between;padding:0 20px;box-shadow:0 1px 4px rgba(0,0,0,0.08)}
 .logo{display:flex;align-items:center;gap:10px}
 .logo-icon{width:32px;height:32px;background:linear-gradient(135deg,#4A90E5,#357ABD);border-radius:6px;display:flex;align-items:center;justify-content:center;color:#fff;font-weight:700;font-size:16px}
 .logo-text{font-size:16px;font-weight:600;color:#4A90E5}
 .logo-sub{font-size:13px;color:#909399;font-weight:400}
 .head-right{display:flex;align-items:center;gap:10px;font-size:13px;color:#909399}
 .main{flex:1;display:flex;align-items:center;justify-content:center;padding:40px 20px}
 .card{background:#fff;border-radius:4px;box-shadow:0 2px 4px 0 rgba(214,228,241,0.7);padding:48px 56px;text-align:center;max-width:460px;width:100%}
 .icon{width:72px;height:72px;margin:0 auto 20px;background:#EBF5FF;border-radius:50%;display:flex;align-items:center;justify-content:center;font-size:34px}
 h1{font-size:20px;font-weight:600;margin-bottom:6px}
 .subtitle{font-size:14px;color:#909399;margin-bottom:28px}
 .form{display:flex;flex-direction:column;gap:14px;text-align:left}
 .form form{display:flex;flex-direction:column;gap:14px;margin:0}
 .input{width:100%;padding:10px 14px;border:1px solid #EBEEF5;border-radius:4px;font-size:14px;color:#333;background:#fff;outline:none;transition:border-color 200ms}
 .input:focus{border-color:#4A90E5;box-shadow:0 0 0 2px rgba(74,144,229,0.15)}
 .input::placeholder{color:#C0C4CC}
 .btn{border:none;border-radius:4px;background:#4A90E5;color:#fff;font-size:15px;font-weight:500;cursor:pointer;transition:background 200ms;padding:10px 18px}
 .btn:hover{background:#357ABD}
 .btn:active{background:#2A6AA0}
 .btn-ghost{background:#fff;color:#4A90E5;border:1px solid #4A90E5}
 .btn-ghost:hover{background:#EBF5FF;color:#357ABD}
 .btn-sm{font-size:13px;padding:5px 12px;font-weight:400}
 .btn-danger{background:#F56C6C}
 .btn-primary{background:#4A90E5;color:#fff}
 .btn-danger:hover{background:#dd5c5c}
 .divider{display:flex;align-items:center;gap:10px;color:#C0C4CC;font-size:12px;margin:4px 0}
 .divider::before,.divider::after{content:"";flex:1;border-top:1px solid #EBEEF5}
 .error{font-size:13px;color:#F56C6C;margin-top:4px;display:none;min-height:18px}
 .oauth-row{display:flex;flex-direction:column;gap:10px}
 .app{flex:1;display:block;padding:20px 24px;width:100%}
 .app-inner{width:100%}
 @media (min-width:1600px){.app{padding:24px 32px}}
 .tabs{display:flex;gap:4px;border-bottom:1px solid #EBEEF5;margin-bottom:18px;background:#fff;border-radius:4px 4px 0 0;padding:8px 12px 0;box-shadow:0 2px 4px 0 rgba(214,228,241,0.7)}
 .tab{border:none;background:none;padding:10px 16px;font-size:14px;color:#909399;cursor:pointer;border-bottom:2px solid transparent;margin-bottom:-1px;font-family:inherit}
 .tab:hover{color:#4A90E5}
 .tab.active{color:#4A90E5;font-weight:600;border-bottom-color:#4A90E5}
 .quota-pill{display:inline-block;border-radius:999px;padding:2px 12px;font-size:12px;background:#EBF5FF;color:#4A90E5;margin-left:8px}
 /* 二级菜单：**左侧竖排**导航（与一级下划线 Tab 明显分层） */
 .list-head{display:flex;align-items:center;gap:8px;margin:2px 0 10px}
 .back-row{margin:0 0 10px}
 /* 轻量分段控件（用于「我的申请/待我审批」这类**视图切换**，不是章节导航） */
 .subtabs{display:inline-flex;gap:4px;padding:4px;background:#F2F6FC;border-radius:8px;margin:0 0 8px}
 .subtab{border:none;background:none;padding:6px 16px;font-size:13px;color:#606266;cursor:pointer;border-radius:6px;font-family:inherit;transition:background 150ms,color 150ms}
 .subtab:hover{color:#4A90E5}
 .subtab.active{background:#fff;color:#4A90E5;font-weight:600;box-shadow:0 1px 3px rgba(0,0,0,0.10)}
 .lang{display:inline-flex;background:#F2F6FC;border-radius:999px;padding:3px;margin-right:4px}
 .lang button{border:none;background:none;font:inherit;font-size:12.5px;color:#606266;padding:5px 12px;border-radius:999px;cursor:pointer}
 .lang button.on{background:#fff;color:#4A90E5;font-weight:600;box-shadow:0 1px 3px rgba(0,0,0,.08)}
 .usermenu{position:relative;display:inline-block}
 .usermenu-panel{display:none;position:absolute;right:0;top:130%;background:#fff;border:1px solid #EBEEF5;border-radius:6px;box-shadow:0 6px 18px rgba(0,0,0,.12);min-width:190px;padding:6px 0;z-index:50}
 .usermenu-panel a{display:block;padding:8px 16px;font-size:13px;color:#333;text-decoration:none;cursor:pointer}
 .usermenu-panel a:hover{background:#F2F6FC;color:#4A90E5}
 .usermenu-panel .sep{border-top:1px solid #EBEEF5;margin:4px 0}
 .usermenu-panel .dim{color:#C0C4CC;cursor:default}
 /* master-detail split（与成员端 console 的 discussions/documents 同口径） */
 /* 列表↔详情分栏：列表只占 1/3（owner 反馈：列表信息不多时别占太宽），详情留 2/3 */
 .split{display:grid;grid-template-columns:minmax(220px,25%) 1fr;gap:16px;align-items:start}
 .split-list{max-height:calc(100vh - 300px);overflow:auto}
 .split-detail{position:sticky;top:16px;min-height:220px}
 @media (max-width:900px){.split{grid-template-columns:1fr}.split-detail{position:static}.split-list{max-height:none}}
 .row-item{display:flex;align-items:center;gap:10px;padding:10px 8px;border-bottom:1px solid #EBEEF5;cursor:pointer;border-radius:4px}
 .row-item:last-child{border-bottom:none}
 .row-item:hover{background:#F2F6FC}
 .row-item.sel{background:#EBF3FC;box-shadow:inset 3px 0 0 #4A90E5}
 .row-item .ri-title{flex:1;font-weight:600;font-size:13px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
 .row-item .ri-meta{color:#909399;font-size:12px;white-space:nowrap;font-family:"SF Mono","Fira Code",monospace}
 .detail-empty{color:#909399;font-size:13px;padding:24px 4px;text-align:center}
 .pc-sum{color:#909399;font-size:13px;margin:6px 0 10px;word-break:break-word}
 .pc-meta{font-size:12px;color:#909399;display:flex;gap:14px;flex-wrap:wrap;margin-bottom:10px}
 .tag-j{display:inline-block;border-radius:3px;padding:1px 8px;font-size:12px;background:#E8F8E8;color:#67C23A;margin-left:6px}
 .tag-i{display:inline-block;border-radius:3px;padding:1px 8px;font-size:12px;background:#F2F6FC;color:#909399;margin-left:6px}
 .pager{display:flex;gap:8px;align-items:center;margin-top:12px;font-size:13px;color:#909399}
 .cards{display:grid;grid-template-columns:repeat(3,1fr);gap:16px;margin-bottom:18px}
 @media (max-width:900px){.cards{grid-template-columns:1fr}}
 .stat-card{background:#fff;border-radius:4px;box-shadow:0 2px 4px 0 rgba(214,228,241,0.7);padding:18px 20px;display:flex;align-items:center;gap:16px}
 .stat-icon{width:46px;height:46px;border-radius:10px;display:flex;align-items:center;justify-content:center;font-size:21px;flex-shrink:0}
 .stat-icon.blue{background:#EBF5FF;color:#4A90E5}.stat-icon.orange{background:#FDF0E0;color:#E6A23C}.stat-icon.green{background:#E8F8E8;color:#67C23A}
 .stat-label{font-size:13px;color:#909399;margin-bottom:4px}
 .stat-value{font-size:23px;font-weight:600;line-height:1.2;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
 .team-card{background:#fff;border-radius:4px;box-shadow:0 2px 4px 0 rgba(214,228,241,0.7);padding:18px 20px;margin-bottom:14px}
 .team-head{display:flex;align-items:center;gap:10px;flex-wrap:wrap;margin-bottom:8px}
 .team-head h3{margin:0;font-size:15px}
 .team-actions{margin-left:auto;display:flex;gap:8px}
 .team-meta{display:flex;gap:16px;flex-wrap:wrap;font-size:13px;color:#909399;margin-bottom:6px}
 .team-meta code{font-family:"SF Mono","Fira Code",ui-monospace,Menlo,monospace;font-size:12px;background:#F2F6FC;padding:1px 5px;border-radius:3px;color:#333}
 .boot-row{display:flex;gap:8px;align-items:center;margin-top:10px;padding-top:12px;border-top:1px solid #EBEEF5;flex-wrap:wrap}
 .boot-row .input{width:300px}
 .empty{padding:28px;text-align:center;color:#909399;font-size:13px}
 .empty .big{font-size:30px;margin-bottom:8px}
 .panel{background:#fff;border-radius:4px;box-shadow:0 2px 4px 0 rgba(214,228,241,0.7);padding:20px 22px;margin-bottom:18px}
 .panel h2{font-size:14px;font-weight:600;margin-bottom:12px}
 .panel h2 .tag{font-size:12px;color:#909399;font-weight:400}
 table{width:100%;border-collapse:collapse;font-size:13px}
 th,td{text-align:left;padding:9px 10px;border-bottom:1px solid #EBEEF5}
 th{background:#F2F6FC;font-weight:600;color:#909399;font-size:12px}
 tr:last-child td{border-bottom:none}
 code{font-family:"SF Mono","Fira Code",ui-monospace,Menlo,monospace;font-size:12px;background:#F2F6FC;padding:1px 5px;border-radius:3px}
 .row{display:flex;gap:8px;align-items:center;margin:10px 0;flex-wrap:wrap}
 .row .input{width:240px}
 .row .input.wide{width:340px}
 .muted{color:#909399;font-size:13px}
 .pill{display:inline-block;border-radius:999px;padding:1px 10px;font-size:12px;background:#EBF5FF;color:#4A90E5}
 .pill.green{background:#eef8e8;color:#4e9a2e}
 .pill.orange{background:#fdf3e7;color:#c07f1f}
 pre.out{background:#F2F6FC;border-radius:4px;padding:12px;font-size:12px;white-space:pre-wrap;word-break:break-all;max-height:220px;overflow:auto}
 a.link{color:#4A90E5;text-decoration:none}
 a.link:hover{text-decoration:underline}
 /* ---------- 深浅色（design/019：与官网一致，按钮在语言选择旁） ---------- */
 html[data-theme="dark"]{color-scheme:dark}
 html[data-theme="dark"] body{background:#12161C;color:#E6EAF0}
 html[data-theme="dark"] header{background:#171C23;border-bottom-color:#2A323B;box-shadow:none}
 html[data-theme="dark"] .logo-text{color:#7FB0EE}
 html[data-theme="dark"] .panel,html[data-theme="dark"] .card,html[data-theme="dark"] .stat-card,
 html[data-theme="dark"] .tabs,html[data-theme="dark"] .subnav{background:#1A2029;box-shadow:none;border-color:#2A323B}
 html[data-theme="dark"] .tabs{border-bottom-color:#2A323B}
 html[data-theme="dark"] .tab{color:#98A3B2}
 html[data-theme="dark"] .tab.active,html[data-theme="dark"] .tab:hover{color:#7FB0EE}
 html[data-theme="dark"] h1,html[data-theme="dark"] h2,html[data-theme="dark"] h3,html[data-theme="dark"] b{color:#E6EAF0}
 html[data-theme="dark"] .subtitle,html[data-theme="dark"] .muted,html[data-theme="dark"] .sec-sub{color:#8B96A5}
 html[data-theme="dark"] table th{background:#1F2630;color:#98A3B2;border-bottom-color:#2A323B}
 html[data-theme="dark"] table td{border-bottom-color:#242B34;color:#D8DEE7}
 html[data-theme="dark"] code,html[data-theme="dark"] pre.out{background:#1F2630;color:#C9D4E2;border-color:#2A323B}
 html[data-theme="dark"] .input,html[data-theme="dark"] select.input,html[data-theme="dark"] textarea{
   background:#151A21;border-color:#2A323B;color:#E6EAF0}
 html[data-theme="dark"] .input::placeholder{color:#6B7686}
 html[data-theme="dark"] .btn-ghost{background:#1A2029;color:#7FB0EE;border-color:#33507A}
 html[data-theme="dark"] .btn-ghost:hover{background:#1E2733}
 html[data-theme="dark"] .row-item{border-bottom-color:#242B34}
 html[data-theme="dark"] .row-item:hover{background:#1E2733}
 html[data-theme="dark"] .row-item.sel{background:#1E2A3A;box-shadow:inset 3px 0 0 #4A90E5}
 html[data-theme="dark"] .detail-empty{color:#8B96A5}
 html[data-theme="dark"] .usermenu-panel{background:#1A2029;border-color:#2A323B;box-shadow:0 8px 24px rgba(0,0,0,.5)}
 html[data-theme="dark"] .usermenu-panel a{color:#D8DEE7}
 html[data-theme="dark"] .usermenu-panel a:hover{background:#1E2733;color:#7FB0EE}
 html[data-theme="dark"] .usermenu-panel .sep{border-top-color:#2A323B}
 html[data-theme="dark"] .lang{background:#1F2630}
 html[data-theme="dark"] .lang button{color:#98A3B2}
 html[data-theme="dark"] .lang button.on{background:#2A3340;color:#7FB0EE;box-shadow:none}
 html[data-theme="dark"] .subtabs{background:#1F2630}
 html[data-theme="dark"] .subtab{color:#98A3B2}
 html[data-theme="dark"] .subtab.on{background:#2A3340;color:#7FB0EE}
 html[data-theme="dark"] .subnav-item{color:#98A3B2}
 html[data-theme="dark"] .subnav-item:hover{background:#1E2733;color:#7FB0EE}
 html[data-theme="dark"] .subnav-item.active{background:#1E2A3A;color:#7FB0EE}
 html[data-theme="dark"] .pill{background:#1D2A3A;color:#7FB0EE}
 html[data-theme="dark"] .tag,html[data-theme="dark"] .tag-i,html[data-theme="dark"] .tag-j{background:#1F2630;color:#98A3B2}
 html[data-theme="dark"] .boot-row,html[data-theme="dark"] .row{border-color:#2A323B}
 html[data-theme="dark"] .divider::before,html[data-theme="dark"] .divider::after{border-top-color:#2A323B}
 .themebtn{border:1px solid #EBEEF5;background:#fff;color:#606266;cursor:pointer;width:32px;height:32px;
   border-radius:999px;font-size:14px;line-height:1;margin-right:6px;display:inline-flex;align-items:center;justify-content:center}
 .themebtn:hover{color:#4A90E5;border-color:#4A90E5}
 html[data-theme="dark"] .themebtn{background:#1A2029;border-color:#2A323B;color:#98A3B2}
</style></head>
<body>
<header><div class="logo"><div class="logo-icon">Tx</div><span class="logo-text">Teamx Cloud</span><span class="logo-sub">平台控制台</span></div>
  <div class="head-right">
    <span class="lang" id="langSwitch" role="group" aria-label="language">
      <button id="lg-zh" class="on" onclick="setLang('zh')">中文</button>
      <button id="lg-en" onclick="setLang('en')">EN</button>
    </span>
    <button class="themebtn" id="themeBtn" onclick="toggleTheme()" aria-label="theme">☾</button>
    <div class="usermenu" id="usermenu" style="display:none">
      <button class="btn btn-sm" onclick="toggleMenu(event)"><span id="who"></span> ▾</button>
      <div class="usermenu-panel" id="usermenuPanel">
        <a onclick="openMyRequests()">我的申请</a>
        <a id="miPending" onclick="openPending()" style="display:none">待我审批</a>
        <div class="sep"></div>
        <a id="miTcoin" onclick="openTcoin()">TCoin</a>
        <a id="miCredit" onclick="openCredit()">Credit</a>
        <a onclick="alert('当前改密请用 CLI：teamx platform user set-password --user-id <id>')">设置</a>
        <div class="sep"></div>
        <a onclick="logout()">退出</a>
      </div>
    </div>
  </div>
</header>
<div class="main" id="loginWrap">
  <div class="card" id="login">
    <div class="icon">☁️</div>
    <h1>平台登录</h1>
    <div class="subtitle">Teamx Cloud · 平台控制台</div>
    <div class="form">
      <!-- 用户名/密码用真 <form>：回车即提交（此前是 div，回车无反应） -->
      <!-- 内联 onsubmit：即使页面脚本某处异常，登录也不会退化成"表单原生提交" -->
      <form id="pwForm" autocomplete="on" onsubmit="return tlogin(event)">
        <input class="input" id="uname" placeholder="用户名" autocomplete="username">
        <input class="input" id="password" type="password" placeholder="密码" autocomplete="current-password">
        <button class="btn" type="submit">登 录</button>
      </form>
      <div class="error" id="loginErr"></div>
      <div class="oauth-row"><a class="btn btn-ghost" id="oauthBtn" style="display:none;text-decoration:none;text-align:center" href="/api/oauth/start">GitHub 登录</a></div>
      <div class="muted">GitHub OAuth：<span id="oauthState">检查中…</span></div>
      <div class="muted" id="scriptState" style="font-size:12px"></div>
    </div>
  </div>
</div>

<div class="app" id="app" style="display:none">
  <nav class="tabs">
    <button class="tab active" data-tab="all" onclick="showTab('all')">全部团队</button>
    <button class="tab" data-tab="mine" onclick="showTab('mine')">我的团队</button>
    <button class="tab op-only" data-tab="tenants" onclick="showTab('tenants')">租户管理</button>
    <button class="tab op-only" data-tab="platform" onclick="showTab('platform')">平台管理</button>
  </nav>
  <div class="tabpage" id="tab-all">
    <div class="panel" style="padding:14px 16px">
      <div class="row" style="margin:0">
        <input class="input" id="dQuery" placeholder="搜索团队名或简介" onkeydown="if(event.key==='Enter'){loadDir(1)}">
        <select class="input" id="dSort" onchange="loadDir(1)">
          <option value="created">最新创建</option>
          <option value="members">成员最多</option>
        </select>
        <label class="muted"><input type="checkbox" id="dJoinable" onchange="loadDir(1)"> 只看可加入</label>
        <button class="btn btn-sm" onclick="loadDir(1)">搜索</button>
      </div>
    </div>
    <div class="split">
      <section class="panel split-list">
        <h2>全部团队 <span class="tag" id="dirCount"></span></h2>
        <div id="dirList"></div>
        <div class="pager" id="dirPager"></div>
      </section>
      <section class="panel split-detail" id="dirDetail"><div class="detail-empty">← 点击左侧团队查看详情</div></section>
    </div>
  </div>
  <div class="tabpage" id="tab-reqs" style="display:none">
    <div class="split">
      <section class="panel split-list">
        <h2 id="reqListTitle">我的申请</h2>
        <div class="subtabs" style="margin:0 0 8px"><button class="subtab active" id="reqTabMine" onclick="reqMode('mine')">我的申请</button><button class="subtab" id="reqTabPend" onclick="reqMode('pend')">待我审批</button></div>
        <div id="reqList"></div>
      </section>
      <section class="panel split-detail" id="reqDetail"><div class="detail-empty">← 点击左侧条目查看详情</div></section>
    </div>
  </div>
  <div class="tabpage" id="tab-mine">
  <div id="sub-list">
    <div class="cards" id="mineCards"></div>
    <div class="split">
      <section class="panel split-list">
        <div class="list-head">
          <button class="btn btn-sm" onclick="openCreateForm()">＋ 创建团队</button>
          <button class="btn btn-sm btn-ghost" onclick="openLetters()">加入团队</button>
          <span class="muted" id="mineCount" style="margin-left:auto"></span>
        </div>
        <div id="mine"></div></section>
      <section class="panel split-detail" id="mineDetail"><div class="detail-empty">← 选左侧团队查看/管理<br><br>或点上方「创建团队 / 加入团队」（表单在右侧显示）</div></section>
    </div>
  </div>
  </div>
  <div class="tabpage" id="tab-tenants" style="display:none">

  <div class="panel"><h2>租户 <span class="tag">project（运维全量）</span></h2>
    <div class="row"><input class="input" id="team" placeholder="team id"><button class="btn btn-sm" onclick="createTenant()">创建租户</button></div>
  </div>
  <div class="split">
    <section class="panel split-list"><div id="tenants"></div></section>
    <section class="panel split-detail" id="tenantDetail"><div class="detail-empty">← 点击左侧租户查看详情</div></section>
  </div>

  </div>
  <div class="tabpage" id="tab-tcoin" style="display:none">
  <div class="panel">
    <h2>TCoin 余额 <span class="tag">1 TCoin = 0.001 USD</span></h2>
    <div class="pc-meta" style="margin-bottom:8px"><span>平台赠送额度；消耗按 LLM tokens 折算（唯一计量源）。<b>v1 只记账不阻断</b> —— 余额不足仅提示。</span></div>
    <div id="tcWarn"></div>
    <div class="split">
      <div class="split-list" id="tcList"><div class="detail-empty">加载中…</div></div>
      <div class="split-detail" id="tcDetail"><div class="detail-empty">← 选择左侧团队查看流水</div></div>
    </div>
  </div>
  <div class="panel"><h2>我的最近动态</h2>
    <table><thead><tr><th>时间</th><th>团队</th><th>变动</th><th>原因</th></tr></thead><tbody id="tcRecent"></tbody></table>
  </div>
  </div>
  <div class="tabpage" id="tab-credit" style="display:none">
  <div class="panel">
    <h2>团队信用 <span class="tag">Credit · 由履约行为自动派生</span></h2>
    <div class="pc-meta" style="margin-bottom:8px"><span>对外只显示 <b>分档徽章</b>；精确分与构成明细仅团队成员/owner 可见。
      基线 <b id="crBaseline">60</b> · 窗口 <b id="crWindow">90</b> 天 · 有效事件 &lt; <b id="crMin">5</b> 时显示「数据不足」。<b>v1 只展示、不影响任何操作。</b></span></div>
    <div class="split">
      <div class="split-list" id="crList"><div class="detail-empty">加载中…</div></div>
      <div class="split-detail" id="crDetail"><div class="detail-empty">← 选择左侧团队查看构成明细</div></div>
    </div>
  </div>
  </div>
  <div class="tabpage" id="tab-platform" style="display:none">
  <div class="panel"><h2>平台 token</h2><table><thead><tr><th>id</th><th>state</th><th>created</th><th>redeemed_by</th></tr></thead><tbody id="tokens"></tbody></table></div>
  <div class="panel"><h2>平台管理员</h2><table><thead><tr><th>user</th><th>granted_at</th><th>via</th></tr></thead><tbody id="admins"></tbody></table></div>
  <div class="panel"><h2>平台审计 <span class="tag">独立于租户台账</span></h2><table><thead><tr><th>at</th><th>actor</th><th>action</th><th>detail</th></tr></thead><tbody id="audit"></tbody></table></div>
  </div>
</div>
<script>
// 登录入口（内联 onsubmit 调用；脚本异常也能用）
function tlogin(e){
  try{ if(e && e.preventDefault) e.preventDefault(); loginPassword(); }
  catch(err){ showFatal('login: ' + (err && err.message || err)); }
  return false;   // 阻止表单原生提交（原生提交会 GET /console? 且不写会话 cookie）
}
// 启动块已移到脚本**末尾**（见文件末尾）：那里 DICT/ERR 才已赋值。
// 历史 bug：启动放开头 → initLang() 时 DICT 仍 undefined（var 提升未赋值）→ 抛错 →
// 后续（含登录表单处理器）全不执行 → 表单原生提交 GET /console? → 没有会话 cookie → 停在登录页。
async function jget(u){const r=await fetch(u,{cache:'no-store'});return await r.json()}
async function jpost(u,b){const r=await fetch(u,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(b||{})});return await r.json()}
async function jdel(u){const r=await fetch(u,{method:'DELETE'});return await r.json()}
function esc(v){return String(v==null?'':v).replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]))}
/* ================= 双语（design/019 SD-14） =================
   方案：**词表按"中文原文 → 英文"精确匹配**，遍历文本节点与 placeholder/title 属性替换；
   MutationObserver 让动态渲染（innerHTML）出来的内容同样被翻译。
   语言选择存 cookie `teamx_lang`；默认跟随浏览器语言。服务端只回稳定 `code`，
   错误文案由 `ERR` 表映射（`errText`）。 */
var LANG = 'zh';
var DICT = {
  "加载中…":"Loading…","成员":"Members","仅邀请":"Invite only","余额":"Balance",
  "我的申请":"My applications","创建团队":"Create team","公开":"Public","私有":"Private",
  "平台控制台":"Platform console","待我审批":"Awaiting my review","我加入的":"Joined",
  "邀请函":"Invitation","复制邀请函":"Copy invitation","数据不足":"Not enough data","分数":"Score",
  "不可用：":"Unavailable: ","全部团队":"All teams","加入团队":"Join a team","租户":"Tenants",
  "时间":"Time","团队":"Team","变动":"Change","原因":"Reason","我创建的":"Created by me",
  "状态":"Status","待审批":"Pending","已复制":"Copied","允许申请加入":"Allow join requests",
  "加入":"Join","暂无":"None yet","去全部团队发现":"Discover teams","允许申请":"Open to requests",
  "团队信用（派生）":"Team credit (derived)","本团队累计消耗":"Team consumption",
  "回收租户":"Destroy tenant","还没有参与任何团队":"Not in any team yet","去我的团队":"Go to my teams",
  "有效事件":"Valid events","刷新":"Refresh","累计消耗":"Total consumption","暂无流水":"No entries yet",
  "无未结算消耗":"No unsettled usage","设置":"Settings","退出":"Sign out","平台登录":"Platform sign-in",
  "用户名":"Username","密码":"Password","登 录":"Sign in","登录":"Sign in","检查中…":"Checking…",
  "我的团队":"My teams","租户管理":"Tenant admin","平台管理":"Platform admin",
  "运行中工作区":"Running workspaces","Owner 邀请函":"Owner invitation","邀请成员":"Invite member",
  "生成邀请函":"Generate invitation","发现设置":"Discovery settings","保存":"Save",
  "离开团队":"Leave team","初始化团队":"Initialize team","公开简介":"Public summary",
  "团队列表":"Teams","加入团队发现":"Discover","我的团队信用":"My team credit",
  "未结算用量":"Unsettled usage","平台总账流水":"Platform ledger","成员分配余额":"Member balances",
  "退出登录":"Sign out","GitHub 登录":"Sign in with GitHub","未启用":"Not enabled",
  "待处理":"Pending","已批准":"Approved","已拒绝":"Rejected","已撤回":"Withdrawn",
  "席位":"Seat","工作区运行中":"Workspace running","工作区":"Workspace","角色":"Role",
  "只读":"read-only","成员操作":"Member ops","平台 token":"Platform tokens","平台管理员":"Platform admins",
  "平台审计":"Platform audit","独立于租户台账":"separate from tenant ledgers",
  "创建租户":"Create tenant","当前":"Current ","共":"Total ","已加入":"Joined",
  "可加入":"Joinable","我创建的团队":"Teams I created","我加入的团队":"Teams I joined",
  "Teamx Cloud · 平台控制台":"Teamx Cloud · Platform console","最新创建":"Newest","成员最多":"Most members",
  "只看可加入":"Joinable only","搜索":"Search","← 点击左侧团队查看详情":"← Pick a team on the left",
  "← 点击左侧条目查看详情":"← Pick an item on the left","＋ 创建团队":"＋ Create team",
  "← 选左侧团队查看/管理":"← Pick a team to view or manage",
  "或点上方「创建团队 / 加入团队」（表单在右侧显示）":"or use “Create team / Join a team” above (the form opens on the right)",
  "project（运维全量）":"project (operator, all tenants)","← 点击左侧租户查看详情":"← Pick a tenant on the left",
  "TCoin 余额":"TCoin balance",
  "平台赠送额度；消耗按 LLM tokens 折算（唯一计量源）。":"Platform-issued credit; usage converts from LLM tokens (single source of truth).",
  "v1 只记账不阻断":"v1 records only, never blocks","—— 余额不足仅提示。":"— low balance is a warning only.",
  "← 选择左侧团队查看流水":"← Pick a team to see its ledger","我的最近动态":"My recent activity",
  "团队信用":"Team credit","Credit · 由履约行为自动派生":"Credit · derived from delivery behaviour",
  "对外只显示":"Publicly only","分档徽章":"the grade badge",
  "；精确分与构成明细仅团队成员/owner 可见。":"; exact score and breakdown are visible to members/owner only.",
  "· 窗口":"· window ","天 · 有效事件":"days · valid events","时显示「数据不足」。":"shows “not enough data”.",
  "v1 只展示、不影响任何操作。":"v1 is display-only and affects nothing.",
  "← 选择左侧团队查看构成明细":"← Pick a team to see the breakdown",
  "信用":"Credit","公开":"Public",
  "结算失败：":"Settle failed: ","刷新失败：":"Refresh failed: ","离开失败：":"Leave failed: ",
  "初始化失败：":"Initialize failed: ","退出失败：":"Sign out failed: ","创建失败：":"Create failed: "
};
/* 服务端错误码 → 文案（SD-14：服务端只回 code，前端出文案） */
var ERR = {
  zh:{ too_many_attempts:"尝试过于频繁，请稍后再试", invalid_credentials:"用户名或密码不正确",
      unauthorized:"请先登录", github_not_configured:"GitHub 登录未配置", not_your_tenant:"不是你的团队",
      not_member:"你不是该团队成员", owner_only:"仅 owner 可操作", quota_exceeded:"已超出配额",
      not_found:"未找到", tenant_unreachable:"租户 server 暂不可达", already_exists:"已存在",
      cooldown:"仍在冷却期内", disabled:"账号已停用", request_failed:"请求失败" },
  en:{ too_many_attempts:"Too many attempts, try again later", invalid_credentials:"Invalid username or password",
      unauthorized:"Please sign in first", github_not_configured:"GitHub sign-in is not configured",
      not_your_tenant:"Not your team", not_member:"You are not a member of this team",
      owner_only:"Owner only", quota_exceeded:"Quota exceeded", not_found:"Not found",
      tenant_unreachable:"Tenant server unreachable", already_exists:"Already exists",
      cooldown:"Still in cooldown", disabled:"Account disabled", request_failed:"Request failed" }
};
function T(zh){ return (LANG==='en' && DICT[zh]) ? DICT[zh] : zh; }
function errText(d){
  var c = d && d.code;
  if (LANG==='en'){ if(c && ERR.en[c]) return ERR.en[c]; return (d && d.error) || ''; }
  if (c && ERR.zh[c]) return ERR.zh[c] + (d.error && c!=='request_failed' ? '' : ('：'+(d.error||'')));
  return (d && d.error) || '';
}
function getCookie(n){ var m=document.cookie.match('(^|; )'+n+'=([^;]*)'); return m?decodeURIComponent(m[2]):null; }
function setCookieLang(l){ document.cookie = 'teamx_lang='+l+'; Path=/; SameSite=Strict; Max-Age=31536000'; }
function applyLangNode(root){
  // 防御：DICT 未赋值/未定义时直接返回（顺序错误也不拖垮整页）
  if (typeof DICT === 'undefined' || !DICT) return;
  root = root || document.body;
  var walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, null);
  var n;
  while((n = walker.nextNode())){
    var t = n.nodeValue, k = t && t.trim();
    if(k && DICT[k] && k === t.trim()){ n.nodeValue = (LANG==='en' ? DICT[k] : k); }
    else if(k && DICT[k] && k !== t){ n.nodeValue = t.replace(k, (LANG==='en' ? DICT[k] : k)); }
  }
  var els = root.querySelectorAll ? root.querySelectorAll('[placeholder],[title]') : [];
  for(var i=0;i<els.length;i++){
    ['placeholder','title'].forEach(function(a){
      var v = els[i].getAttribute(a); if(v && DICT[v.trim()]) els[i].setAttribute(a, DICT[v.trim()]);
    });
  }
}
function setLang(l){
  LANG = l;
  document.documentElement.lang = (l==='zh' ? 'zh-CN' : 'en');
  try{ setCookieLang(l); }catch(e){}
  var a=document.getElementById('lg-zh'), b=document.getElementById('lg-en');
  if(a)a.classList.toggle('on', l==='zh'); if(b)b.classList.toggle('on', l==='en');
  applyLangNode(document.body);
}
/* 动态渲染的内容也要翻译：观察 DOM 变化后重扫（节流） */
var __langTimer=null;
function watchLang(){
  try{
    new MutationObserver(function(){
      if(LANG!=='en') return;
      if(__langTimer) clearTimeout(__langTimer);
      __langTimer = setTimeout(function(){ applyLangNode(document.body); }, 40);
    }).observe(document.body, {childList:true, subtree:true});
  }catch(e){}
}
function initLang(){
  var l = getCookie('teamx_lang') || ((navigator.language||'zh').toLowerCase().indexOf('zh')===0 ? 'zh' : 'en');
  setLang(l); watchLang();
}
/* 深浅色（与官网一致）：localStorage 记忆，默认跟随系统 */
var THEME = 'light';
function applyTheme(){
  document.documentElement.dataset.theme = THEME;
  var b = document.getElementById('themeBtn');
  if(b) b.textContent = (THEME === 'dark') ? '☀' : '☾';
  try{ localStorage.setItem('teamx_theme', THEME); }catch(e){}
}
function toggleTheme(){ THEME = (THEME === 'dark') ? 'light' : 'dark'; applyTheme(); }
function initTheme(){
  var t = null;
  try{ t = localStorage.getItem('teamx_theme'); }catch(e){}
  if(!t) t = (window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches) ? 'dark' : 'light';
  THEME = t; applyTheme();
}
/* 前端错误可见化：任何未捕获异常/被拒 Promise 都直接显示在页面上（否则表现为"点了没反应"） */
function showFatal(msg){
  try{
    var el = document.getElementById('fatalErr');
    if(!el){
      el = document.createElement('div'); el.id='fatalErr';
      el.style.cssText='position:fixed;left:0;right:0;bottom:0;z-index:9999;background:#F56C6C;color:#fff;'+
        'font-size:13px;padding:10px 16px;white-space:pre-wrap;max-height:40vh;overflow:auto';
      document.body.appendChild(el);
      el.onclick=function(){ el.remove(); };
    }
    el.textContent = '页面脚本错误（点此关闭）：' + msg;
  }catch(e){}
}
window.addEventListener('error', function(e){ showFatal((e && e.message) || 'unknown'); });
window.addEventListener('unhandledrejection', function(e){
  var r=e && e.reason; showFatal((r && (r.message||r)) || 'unhandled rejection');
});
function toggleMenu(e){e.stopPropagation();const p=document.getElementById('usermenuPanel');p.style.display=p.style.display==='block'?'none':'block'}
document.addEventListener('click',()=>{const p=document.getElementById('usermenuPanel');if(p)p.style.display='none'});
function openMyRequests(){showTab('reqs');loadMineReqs();loadPendingReqs()}
function openPending(){showTab('reqs');loadPendingReqs();loadMineReqs()}
async function loadDir(page){
  const q=encodeURIComponent(document.getElementById('dQuery').value.trim());
  const sort=document.getElementById('dSort').value;
  const j=document.getElementById('dJoinable').checked?'1':'0';
  const d=await jget(`/api/discovery/directory?query=${q}&sort=${sort}&joinable=${j}&page=${page||1}&per_page=20`);
  if(!d.ok){document.getElementById('dirList').innerHTML=`<div class="muted">加载失败：${esc(d.error||'')}</div>`;return}
  const items=d.items||[];
  window.__dirItems=items;
  document.getElementById('dirCount').textContent=`${d.total||0} 个`;
  document.getElementById('dirList').innerHTML=items.length?items.map(t=>`
    <div class="row-item" data-id="${esc(t.team_id)}" onclick="openDirDetail('${esc(t.team_id)}',this)">
      <span class="ri-title">${esc(t.name)}</span>
      ${t.relation==='owner'?'<span class="tag-i">我创建的</span>'
        :(t.relation==='member'?'<span class="tag-i">我加入的</span>'
          :(t.can_apply?'<span class="tag-j">可加入</span>':'<span class="tag-i">仅邀请</span>'))}
      ${t.credit_grade?`<span class="pill ${crPill(t.credit_grade)}" title="团队信用分档">信用 ${esc(t.credit_grade)}</span>`:''}
      <span class="ri-meta">${esc(t.member_count)} 人</span>
    </div>`).join('')
    : '<div class="detail-empty">没有匹配的公开团队</div>';
  const total=d.total||0, per=d.per_page||20, cur=d.page||1, pages=Math.max(1,Math.ceil(total/per));
  document.getElementById('dirPager').innerHTML=`共 ${total} 个 · 第 ${cur}/${pages} 页
    <button class="btn btn-sm" ${cur<=1?'disabled':''} onclick="loadDir(${cur-1})">上一页</button>
    <button class="btn btn-sm" ${cur>=pages?'disabled':''} onclick="loadDir(${cur+1})">下一页</button>`;
}
function markSel(el){document.querySelectorAll('.row-item.sel').forEach(x=>x.classList.remove('sel'));if(el)el.classList.add('sel')}
async function openDirDetail(id,el){
  markSel(el);
  const box=document.getElementById('dirDetail');
  box.innerHTML='<div class="detail-empty">加载中…</div>';
  const d=await jget('/api/discovery/teams/'+encodeURIComponent(id));
  if(!d.ok){box.innerHTML=`<div class="detail-empty">${esc(d.error||'不可见')}</div>`;return}
  const t=d.team||{};
  box.innerHTML=`<div class="team-head"><h3>${esc(t.name)}</h3>${t.joinable?'<span class="tag-j">可申请加入</span>':'<span class="tag-i">仅邀请</span>'}</div>
   <div class="pc-sum">${esc(t.public_summary||'（未填写简介）')}</div>
   <div class="pc-meta"><span>owner ${esc(t.owner_name||'—')}</span><span>成员 ${esc(t.member_count)}</span><span>状态 ${esc(t.state||'')}</span><span>team ${esc((t.team_id||'').slice(0,8))}</span></div>
   <div class="muted" style="margin-bottom:10px">${esc(t.tenant_meta_note||'')}</div>
   ${t.can_apply?`<div class="boot-row"><input class="input wide" id="applyMsg" placeholder="申请留言（可选）"><button class="btn btn-sm" onclick="applyJoin('${esc(t.team_id)}')">申请加入</button></div>`
     : (t.relation==='owner'?'<div class="muted">你是该团队的 owner。</div>'
        : (t.relation==='member'?'<div class="muted">你已加入该团队。</div>'
           : '<div class="muted">该团队仅接受邀请。</div>'))}`;
}
async function applyJoin(id){
  const message=(document.getElementById('applyMsg')||{}).value||'';
  const r=await jpost('/api/discovery/teams/'+encodeURIComponent(id)+'/apply',{message});
  if(!r.ok){alert(errText(r));return}
  alert('申请已提交，等待团队 owner 审批。可在「我的申请」查看状态。');
  document.getElementById('dirDetail').style.display='none';
}
const REQ_ZH={pending:'待审批',approved:'已批准',rejected:'已拒绝',withdrawn:'已撤回'};
let REQ_MODE='mine';
function reqMode(m){REQ_MODE=m;
  document.getElementById('reqTabMine').className='subtab'+(m==='mine'?' active':'');
  document.getElementById('reqTabPend').className='subtab'+(m==='pend'?' active':'');
  document.getElementById('reqListTitle').textContent=(m==='mine'?'我的申请':'待我审批');
  document.getElementById('reqDetail').innerHTML='<div class="detail-empty">← 点击左侧条目查看详情</div>';
  if(m==='mine')loadMineReqs();else loadPendingReqs();}
async function loadMineReqs(){
  const d=await jget('/api/discovery/mine');
  const rows=(d.requests||[]); window.__reqs=rows; window.__reqKind='mine';
  if(REQ_MODE!=='mine')return;
  document.getElementById('reqList').innerHTML=rows.length?rows.map(r=>`
    <div class="row-item" data-id="${esc(r.request_id)}" onclick="openReqDetail('${esc(r.request_id)}',this)">
      <span class="ri-title">${esc(r.team_name||r.team_id)}</span>
      <span class="pill">${esc(REQ_ZH[r.status]||r.status)}</span>
    </div>`).join(''):'<div class="detail-empty">还没有申请记录</div>';
}
async function withdrawReq(id){
  const r=await jpost('/api/discovery/requests/'+encodeURIComponent(id)+'/withdraw',{});
  if(!r.ok){alert(errText(r));return}
  loadMineReqs();
}
async function loadPendingReqs(){
  const d=await jget('/api/discovery/pending');
  const rows=(d.requests||[]); window.__reqs=rows; window.__reqKind='pend';
  document.getElementById('miPending').style.display=rows.length?'block':'none';
  if(REQ_MODE!=='pend')return;
  document.getElementById('reqList').innerHTML=rows.length?rows.map(r=>`
    <div class="row-item" data-id="${esc(r.request_id)}" onclick="openReqDetail('${esc(r.request_id)}',this)">
      <span class="ri-title">${esc(r.team_name||r.team_id)}</span>
      <span class="ri-meta">${esc(r.applicant_name||r.applicant_user_id)}</span>
    </div>`).join(''):'<div class="detail-empty">暂无待审申请</div>';
}
function openReqDetail(id,el){
  markSel(el);
  const kind=window.__reqKind;
  const r=(window.__reqs||[]).find(x=>x.request_id===id)||{};
  const box=document.getElementById('reqDetail');
  if(kind==='pend'){
    box.innerHTML=`<div class="team-head"><h3>${esc(r.team_name||r.team_id)}</h3><span class="tag-i">待审批</span></div>
      <div class="pc-meta"><span>申请人 ${esc(r.applicant_name||r.applicant_user_id)}</span><span>${esc((r.created_at||'').slice(0,16).replace('T',' '))}</span></div>
      <div class="pc-sum">${esc(r.message||'（无留言）')}</div>
      <div class="boot-row"><button class="btn btn-sm" onclick="decideReq('${esc(r.request_id)}',true)">批准</button>
      <button class="btn btn-sm btn-danger" onclick="decideReq('${esc(r.request_id)}',false)">拒绝</button></div>`;
  }else{
    box.innerHTML=`<div class="team-head"><h3>${esc(r.team_name||r.team_id)}</h3><span class="pill">${esc(REQ_ZH[r.status]||r.status)}</span></div>
      <div class="pc-meta"><span>申请于 ${esc((r.created_at||'').slice(0,16).replace('T',' '))}</span></div>
      ${r.status==='rejected'&&r.cooldown_until?`<div class="muted">冷却至 ${esc((r.cooldown_until||'').slice(0,10))}（到期后可再次申请）</div>`:''}
      ${r.status==='approved'?(r.letter?`
        <div style="margin-top:8px"><b>邀请函</b>（导入本地即成为成员）<button class="btn btn-sm" style="margin-left:8px" onclick="copyText(${JSON.stringify(r.letter)})">复制邀请函</button></div>
        <pre class="out" style="max-height:150px">${esc((r.letter||'').slice(0,300))}…</pre>
        <div class="muted">成员工作区：${esc(r.pod_state||'pending')}${r.pod_detail?`（${esc(r.pod_detail)}）`:''}</div>`
        :`<div class="muted">已批准，但邀请函未生成（可见 agent 侧交付状态）。</div>`):''}
      <div class="boot-row">${r.status==='pending'?`<button class="btn btn-sm btn-danger" onclick="withdrawReq('${esc(r.request_id)}')">撤回申请</button>`:''}</div>`;
  }
}
async function decideReq(id,approve){
  const r=await jpost('/api/discovery/requests/'+encodeURIComponent(id)+'/decide',{approve});
  if(!r.ok){alert(errText(r));return}
  alert(approve?'已批准（邀请函与成员工作区见后续步骤）':'已拒绝（申请人 7 天后可再次申请）');
  loadPendingReqs();loadMineReqs();
}
function showTab(name){document.querySelectorAll('.tab').forEach(b=>b.classList.toggle('active',b.dataset.tab===name));document.querySelectorAll('.tabpage').forEach(p=>p.style.display=p.id==='tab-'+name?'block':'none')}
async function copyText(t){try{await navigator.clipboard.writeText(t);alert('已复制')}catch(e){const ta=document.createElement('textarea');ta.value=t;document.body.appendChild(ta);ta.select();document.execCommand('copy');ta.remove();alert('已复制')}}
function loginFail(msg){const e=document.getElementById('loginErr');e.textContent=msg||'登录失败';e.style.display='block'}
async function boot(){
  const me=await jget('/api/me');
  if(!me.ok){document.getElementById('loginWrap').style.display='flex';document.getElementById('app').style.display='none';document.getElementById('oauthState').textContent=me.github_oauth?'可用':'未配置（TEAMX_GITHUB_CLIENT_ID/SECRET）';document.getElementById('oauthBtn').style.display=me.github_oauth?'block':'none';return}
  document.getElementById('loginWrap').style.display='none';document.getElementById('app').style.display='block';
  document.getElementById('who').textContent=(me.who||'').replace('user:','');
  document.getElementById('usermenu').style.display='inline-block';
  const isOp=(me.who||'').indexOf('operator')===0;
  document.querySelectorAll('.tab.op-only').forEach(b=>b.style.display=isOp?'':'none');
  showTab('all');reqMode('mine');
  // 首屏各块各自容错：某一块失败不应把整个已登录界面拖垮
  for(const step of [loadDir(1),loadMine(),loadTenants(),loadPlatform()]){ try{ await step; }catch(e){ showFatal('加载失败：'+(e&&e.message||e)); } }
}
try{ document.getElementById('pwForm').addEventListener('submit',e=>{e.preventDefault();loginPassword()}); }
catch(e){ showFatal('form bind: ' + (e && e.message || e)); }
async function loginPassword(){
  const name=document.getElementById('uname').value.trim();
  const password=document.getElementById('password').value;
  const btn=document.querySelector('#pwForm button[type=submit]');
  const old=btn?btn.textContent:null;
  if(btn){ btn.disabled=true; btn.textContent='登录中…'; }
  try{
    const r=await jpost('/api/login/password',{name,password});
    if(r.ok){ await boot(); return; }
    loginFail(errText(r));
  }catch(e){ loginFail('请求失败：'+(e&&e.message||e)); }
  finally{ if(btn){ btn.disabled=false; btn.textContent=old; } }
}
async function logout(){
  const r=await jpost('/api/logout',{});
  if(!r.ok){alert(T('退出失败：')+errText(r));return}
  const um=document.getElementById('usermenu'); if(um) um.style.display='none';
  const up=document.getElementById('usermenuPanel'); if(up) up.style.display='none';
  // 清空上一会话的界面残留（团队/租户/平台三块）
  ['mine','mineCards','mineDetail','tenants','tenantDetail','tokens','admins','audit','dirList','reqList'].forEach(id=>{const el=document.getElementById(id); if(el) el.innerHTML=''});
  await boot();
}
// 「创建团队 / 加入团队」的表单与取件页都渲染到**右侧详情区**（保持左侧列表可见，owner 反馈）
function openCreateForm(){
  if(typeof markSel==='function')markSel(null);
  const box=document.getElementById('mineDetail');
  box.innerHTML=`
    <div class="team-head"><h3>创建团队</h3><span class="tag">O-1(a) 全新开始 · 空账本</span></div>
    <div class="form" style="gap:14px;margin-top:10px">
      <input class="input" id="ncName" placeholder="团队名，比如 data-team">
      <select class="input" id="ncVis"><option value="private">私有（不可被发现）</option><option value="public">公开（出现在「全部团队」）</option></select>
      <select class="input" id="ncJoin"><option value="invite_only">仅邀请</option><option value="open_request">允许申请加入</option></select>
      <input class="input" id="ncSum" placeholder="公开简介（≤200 字，可选）">
      <div><button class="btn" onclick="createMyTeam()">创建团队</button><span class="muted" id="ncQuota" style="margin-left:10px"></span></div>
      <div class="muted">创建后 server 由平台自动拉起（约 1 分钟，需等 Pod 就绪）；随后在团队详情里取 <b>owner 邀请函</b> 导入即成为 owner。</div>
    </div>`;
  const q=document.getElementById('ncQuota');
  if(q&&window.__mineQuota)q.textContent='当前 '+window.__mineQuota;
  const n=document.getElementById('ncName'); if(n)n.focus();
}
function openLetters(){
  if(typeof markSel==='function')markSel(null);
  document.getElementById('mineDetail').innerHTML=`
    <div class="team-head"><h3>加入团队</h3><span class="tag">取件页</span></div>
    <div class="muted" style="margin-top:6px">这里是你<b>已加入</b>团队的成员邀请函（换机器/没保存时可重新取回）。<br>
      想加入新团队 → 去「全部团队」发现并申请；你自己创建的团队不需要"加入"（用团队详情里的 owner 邀请函）。</div>
    <div id="lettersList" style="margin-top:10px"></div>`;
  loadLetters();
}
async function loadMine(){try{
  const d=await jget('/api/my/teams');
  if(!d.ok){document.getElementById('mine').innerHTML='';document.getElementById('mineCards').innerHTML='';return}
  const owned=d.owned||[], joined=d.joined||[];
  document.getElementById('mineCards').innerHTML=
   `<div class="stat-card"><div class="stat-icon blue">👥</div><div><div class="stat-label">我创建的</div><div class="stat-value">${d.used}/${d.quota}</div></div></div>`+
   `<div class="stat-card"><div class="stat-icon orange">🤝</div><div><div class="stat-label">我加入的</div><div class="stat-value">${d.joined_count}</div></div></div>`+
   `<div class="stat-card"><div class="stat-icon green">💻</div><div><div class="stat-label">运行中工作区</div><div class="stat-value">${d.running_workspaces}</div></div></div>`;
  window.__mineQuota=d.used+'/'+d.quota;
  const q=document.getElementById('ncQuota'); if(q) q.textContent='当前 '+window.__mineQuota;
  const mc=document.getElementById('mineCount'); if(mc) mc.textContent='共 '+(owned.length+joined.length)+' 个';
  window.__owned=owned; window.__joined=joined;
  const row=(id,name,meta,kind)=>`<div class="row-item" data-id="${esc(id)}" onclick="openMineDetail('${esc(id)}','${kind}',this)"><span class="ri-title">${esc(name)}</span><span class="ri-meta">${esc(meta)}</span></div>`;
  document.getElementById('mine').innerHTML=
    `<div class="muted" style="margin:6px 0 4px">我创建的（${owned.length}）</div>`+
    (owned.length?owned.map(t=>row(t.team_id,t.team_name||'未命名',(t.visibility==='public'?'公开':'私有')+' · '+(t.namespace||''),'owned')).join(''):'<div class="detail-empty">暂无</div>')+
    `<div class="muted" style="margin:14px 0 4px">我加入的（${joined.length}）</div>`+
    (joined.length?joined.map(t=>row(t.team_id,t.team_name||t.team_id,(t.joined_at||'').slice(0,10)+' 加入','joined')).join(''):'<div class="detail-empty">暂无</div>');
  if(!owned.length&&!joined.length){
    document.getElementById('mine').innerHTML='<div class="detail-empty">还没有团队<br><br><button class="btn btn-sm" onclick="openCreateForm()">创建团队</button> <button class="btn btn-sm" onclick="showTab(\'all\')">去全部团队发现</button></div>';
  }
}catch(e){}}

async function openMineDetail(id,kind,el){
  markSel(el);
  const box=document.getElementById('mineDetail');
  box.innerHTML='<div class="detail-empty">加载中…</div>';
  let det={};try{det=(await jget('/api/discovery/teams/'+encodeURIComponent(id))).team||{};}catch(e){}
  const t=(kind==='owned'?(window.__owned||[]):(window.__joined||[])).find(x=>x.team_id===id)||{};
  const short=id.slice(0,8); const ws=t.workspace_state||'none';
  if(kind==='owned'){
    const vis=det.visibility||'private', jp=det.join_policy||'invite_only', sum=det.public_summary||'';
    box.innerHTML=`
      <!-- 概览卡 -->
      <div class="panel">
        <div class="team-head"><h3>${esc(t.team_name||id)}</h3><span class="pill">${vis==='public'?'公开':'私有'}</span>${jp==='open_request'?'<span class="tag-j">允许申请</span>':'<span class="tag-i">仅邀请</span>'}</div>
        <div class="pc-meta"><span>team <code>${esc(id)}</code></span><span>ns <code>${esc(t.namespace||'')}</code></span><span>CA <code>${esc((t.ca_fingerprint||'').slice(0,16))}…</code></span></div>
        <div class="pc-sum">${esc(sum||'（未填写公开简介）')}</div>
        ${t.bootstrapped ? `
        <div class="muted" style="margin-top:10px">✅ 已初始化（团队数据就绪）</div>` : `
        <div class="boot-row" style="margin-top:10px"><input class="input wide" id="bsUrl_${short}" placeholder="租户 endpoint（留空 = 平台自动连接）"><button class="btn btn-sm" onclick="bootstrapTeam('${esc(id)}')">初始化团队</button></div>
        <div class="muted">留空即可：平台会自行建立到该租户 server 的通道（同机 port-forward），并把可用地址写进 owner 邀请函。</div>`}
      </div>
      <!-- Owner 邀请函 -->
      <div class="panel">
        <h2>Owner 邀请函 <span class="tag">认领你在这个团队的 owner 座位</span></h2>
        <div id="ownerLetter_${short}"></div>
      </div>
      <!-- 成员（含「邀请成员」按钮） -->
      <div class="panel">
        <h2>成员 <span class="tag">只读</span></h2>
        <div class="row" style="margin-bottom:10px"><button class="btn btn-sm" onclick="toggleInvite('${short}')">＋ 邀请成员</button></div>
        <div id="invBox_${short}" style="display:none;border-top:1px solid #EBEEF5;padding-top:10px;margin-bottom:10px">
          <div class="boot-row"><input class="input" id="invRole_${short}" placeholder="role: 描述" value="contributor: 成员"><button class="btn btn-sm" onclick="inviteMember('${esc(id)}')">生成邀请函</button></div>
          <pre class="out" id="invOut_${short}" style="display:none"></pre>
        </div>
        <div id="roster_${short}" class="muted">加载中…</div>
      </div>
      <!-- Credit -->
      <div class="panel">
        <h2>Credit <span class="tag">团队信用（派生）</span></h2>
        <div id="crTeam_${short}" class="muted">加载中…</div>
      </div>
      <!-- TCoin -->
      <div class="panel">
        <h2>TCoin <span class="tag">本团队累计消耗</span></h2>
        <div id="tcTeam_${short}" class="muted">加载中…</div>
      </div>
      <!-- 发现设置 -->
      <div class="panel">
        <h2>发现设置 <span class="tag">公开可见性与加入策略</span></h2>
        <div class="row"><select class="input" id="vis_${short}"><option value="private"${vis==='private'?' selected':''}>私有</option><option value="public"${vis==='public'?' selected':''}>公开</option></select>
          <select class="input" id="jp_${short}"><option value="invite_only"${jp==='invite_only'?' selected':''}>仅邀请</option><option value="open_request"${jp==='open_request'?' selected':''}>允许申请加入</option></select>
          <input class="input wide" id="sum_${short}" placeholder="公开简介" value="${esc(sum)}"><button class="btn btn-sm" onclick="saveDiscover('${esc(id)}')">保存</button></div>
      </div>
      <!-- 危险操作 -->
      <div class="panel">
        <h2>危险操作 <span class="tag">不可恢复</span></h2>
        <button class="btn btn-sm btn-danger" onclick="destroyTenant('${esc(id)}','${esc(t.team_name||'')}')">回收租户</button>
      </div>`;
    loadOwnerLetter(id);
    loadRoster(id);
    loadTeamTcoin(id);
    loadTeamCredit(id);
  }else{
    box.innerHTML=`
      <div class="panel">
        <div class="team-head"><h3>${esc(t.team_name||t.team_id)}</h3><span class="tag-i">我加入的</span>${ws==='Running'?'<span class="tag-j">工作区运行中</span>':'<span class="tag-i">工作区 '+esc(ws)+'</span>'}</div>
        <div class="pc-meta"><span>加入于 ${esc((t.joined_at||'').slice(0,10)||'—')}</span><span>席位 <code>${esc((t.member_id||'').slice(0,12))}</code></span>${t.workspace_live?'':'<span class="tag-i">状态为快照</span>'}</div>
        <div class="pc-sum">${esc(det.public_summary||'')}</div>
      </div>
      <div class="panel">
        <h2>我的邀请函 <span class="tag">导入本地即成为成员</span></h2>
        ${t.letter_available?`<button class="btn btn-sm" onclick="copyText(${JSON.stringify(t.letter||'')})">复制邀请函</button>`:'<div class="muted">邀请函不可用（可能未生成）</div>'}
      </div>
      <div class="panel">
        <h2>Credit <span class="tag">团队信用（派生）</span></h2>
        <div id="crTeam_${short}" class="muted">加载中…</div>
      </div>
      <div class="panel">
        <h2>危险操作</h2>
        <button class="btn btn-sm btn-danger" onclick="leaveTeam('${esc(id)}','${esc(t.team_name||'')}')">离开团队</button>
        <div class="muted" style="margin-top:6px">离开会释放座位并回收工作区（PVC 保留 7 天），之后可再次申请</div>
      </div>`;
  }
}

// 成员卡上的「邀请成员」开关（owner 反馈：邀请入口应挂在成员卡上）
function toggleInvite(short){
  const b=document.getElementById('invBox_'+short);
  if(!b) return;
  b.style.display = (b.style.display==='block') ? 'none' : 'block';
}
// ---- credit（design/018 SD-16：用户名菜单实页 + 团队详情 + 目录徽章） ----
const CR_DESC={S:'极可信（90+）',A:'可信（80-89）',B:'正常（60-79）',C:'需关注（40-59）',D:'高风险（<40）'};
function crPill(g){return g==='S'||g==='A'?('pill green'):(g==='D'?'pill orange':'pill')}
function openCredit(){showTab('credit');loadCredit()}
async function loadCredit(){
  const list=document.getElementById('crList');
  const d=await jget('/api/my/credit');
  if(!d.ok){list.innerHTML='<div class="detail-empty">'+esc(d.error||'')+'</div>';return}
  document.getElementById('crBaseline').textContent=d.baseline;
  document.getElementById('crWindow').textContent=d.window_days;
  document.getElementById('crMin').textContent=d.min_events;
  const xs=d.teams||[];
  list.innerHTML=xs.length?xs.map(t=>`<div class="row-item" onclick="openCreditTeam('${esc(t.team_id)}',this)">
    <div style="flex:1"><div><b>${esc(t.team_name||t.team_id)}</b> <span class="tag-i">${t.relation==='owner'?'Owner':'成员'}</span></div>
      <div class="muted">${t.insufficient||t.awaiting_refresh?'<span class="tag-i">数据不足</span>':('分数 <b>'+t.credit.score+'</b> · '+esc(CR_DESC[t.grade]||''))}
        ${t.awaiting_refresh?' · <span class="muted">待刷新</span>':''}</div></div>
    ${t.grade&&!t.insufficient?`<span class="${crPill(t.grade)}">${esc(t.grade)}</span>`:''}</div>`).join('')
    :'<div class="detail-empty">还没有参与任何团队<br><br><button class="btn btn-sm" onclick="showTab(\'mine\')">去我的团队</button></div>';
}
async function openCreditTeam(id,el){
  if(typeof markSel==='function')markSel(el);
  const box=document.getElementById('crDetail');
  box.innerHTML='<div class="detail-empty">加载中…</div>';
  const d=await jget('/api/my/credit/'+encodeURIComponent(id));
  if(!d.ok){box.innerHTML='<div class="detail-empty">'+esc(d.error||'')+'</div>';return}
  const c=d.credit;
  const head=`<div class="team-head"><h3>${esc(id)}</h3>
    ${c&&!c.insufficient?`<span class="${crPill(c.grade)}">${esc(c.grade)}</span>`:'<span class="tag-i">数据不足</span>'}</div>
    <div class="pc-meta"><span>分数 <b>${c?c.score:'—'}</b></span><span>有效事件 ${c?c.event_count:0}</span>
      <span>模型 <code>${esc(d.model_version||'')}</code></span></div>
    ${c&&c.computed_at?'<div class="muted">上次刷新 '+esc(c.computed_at.slice(0,16).replace('T',' '))+'</div>':'<div class="muted">尚未刷新（点下方「刷新」拉取租户侧行为）</div>'}`;
  const bd=(c&&c.breakdown)||[];
  const table=bd.length?('<table><thead><tr><th>信号</th><th>计数</th><th>得分</th><th>说明</th></tr></thead><tbody>'+
    bd.map(r=>`<tr><td>${esc(r.signal)}</td><td>${esc(r.count)}</td>
      <td style="color:${r.points<0?'#F56C6C':'#4e9a2e'}">${r.points>0?'+':''}${r.points}</td><td class="muted">${esc(r.note||'')}</td></tr>`).join('')+
    '</tbody></table>'):'<div class="muted">暂无构成明细（先刷新）</div>';
  box.innerHTML=head+'<h2 style="margin-top:12px">构成明细 <span class="tag">可解释、无黑箱</span></h2>'+table+
    `<div class="muted" style="margin-top:8px">基线 ${d.baseline||60} 分起步；负向按类封顶；90 天线性衰减；<b>v1 不影响任何操作</b>。</div>
     <div class="row" style="margin-top:8px"><button class="btn btn-sm" onclick="refreshCredit('${esc(id)}')">刷新</button>
     <span class="muted">刷新会经自动通道读取该租户的行为计数</span></div>`;
}
async function refreshCredit(id){
  const r=await jpost('/api/my/credit/'+encodeURIComponent(id)+'/refresh',{});
  if(!r.ok){alert(T('刷新失败：')+errText(r));return}
  alert(r.insufficient?('已刷新：数据不足（有效事件 '+r.event_count+' < 5），不显示分数'):('已刷新：'+r.grade+'（'+r.score+' 分）'));
  openCreditTeam(id,null); loadCredit();
}
async function loadTeamCredit(id){
  const short=id.slice(0,8); const el=document.getElementById('crTeam_'+short); if(!el)return;
  const d=await jget('/api/my/credit/'+encodeURIComponent(id));
  if(!d.ok){el.innerHTML='<span class="muted">不可用：'+esc(d.error||'')+'</span>';return}
  const c=d.credit;
  el.innerHTML=(c?(c.insufficient?'<span class="tag-i">数据不足</span>（有效事件 '+c.event_count+'）':'<span class="'+crPill(c.grade)+'">'+esc(c.grade)+'</span> 分数 <b>'+c.score+'</b>'):
    '<span class="muted">尚未刷新</span>')+
    ` <button class="btn btn-sm" style="margin-left:8px" onclick="refreshCredit('${esc(id)}').then(()=>loadTeamCredit('${esc(id)}'))">刷新</button>
     <div class="muted" style="margin-top:4px">派生自台账事件（完成/驳回/阻塞/入场/留存）+ TCoin 欠费；对外只暴露分档。</div>`;
}

// ---- TCoin（design/017 SD-15：用户名菜单实页 + 团队详情累计） ----
function openTcoin(){showTab('tcoin');loadTcoin()}
async function loadTcoin(){
  const list=document.getElementById('tcList'), warn=document.getElementById('tcWarn');
  const d=await jget('/api/my/tcoin');
  if(!d.ok){list.innerHTML='<div class="detail-empty">'+esc(d.error||'')+'</div>';return}
  const xs=d.teams||[];
  warn.innerHTML=xs.filter(t=>t.warning).length?('<div class="panel" style="border-left:3px solid #E6A23C;background:#fdf3e7"><b>提示</b>：'+
    xs.filter(t=>t.warning).map(t=>esc(t.team_name||t.team_id)+' — '+esc(t.warning)).join('；')+
    '<div class="muted">v1 只记账不阻断：余额不足不会停止任何操作。</div></div>'):'';
  list.innerHTML=xs.length?xs.map(t=>`<div class="row-item" onclick="openTcoinTeam('${esc(t.team_id)}',this)">
    <div style="flex:1"><div><b>${esc(t.team_name||t.team_id)}</b> <span class="tag-i">${t.relation==='owner'?'Owner':'成员'}</span></div>
      <div class="muted">余额 <b>${t.balance}</b> TCoin · 累计消耗 ${t.consumed_total}${t.my_balance==null?'':' · 我的分配 '+t.my_balance}</div></div>
    ${t.warning?'<span class="pill orange">'+esc(t.warning)+'</span>':''}</div>`).join(''):
    '<div class="detail-empty">还没有参与任何团队<br><br><button class="btn btn-sm" onclick="showTab(\'mine\')">去我的团队</button></div>';
  const rec=d.recent||[];
  document.getElementById('tcRecent').innerHTML=rec.length?rec.map(e=>`<tr><td>${esc((e.at||'').slice(0,16).replace('T',' '))}</td><td>${esc(e.team_name||e.team_id||'')}</td>
    <td style="color:${e.delta<0?'#F56C6C':'#4e9a2e'}">${e.delta>0?'+':''}${e.delta}</td><td>${esc(e.reason)}${e.ref?' <code>'+esc(e.ref)+'</code>':''}</td></tr>`).join(''):
    '<tr><td colspan="4" class="muted">暂无流水</td></tr>';
}
async function openTcoinTeam(id,el){
  if(typeof markSel==='function')markSel(el);
  const box=document.getElementById('tcDetail');
  box.innerHTML='<div class="detail-empty">加载中…</div>';
  const d=await jget('/api/my/tcoin/'+encodeURIComponent(id));
  if(!d.ok){box.innerHTML='<div class="detail-empty">'+esc(d.error||'')+'</div>';return}
  const us=d.usage||{}; const ms=us.members||[];
  box.innerHTML=`<div class="team-head"><h3>${esc(id)}</h3><span class="pill">${esc(d.relation||'')}</span></div>
    <div class="pc-meta"><span>余额 <b>${d.balance}</b> TCoin</span><span>累计消耗 <b>${d.consumed_total}</b></span></div>
    ${d.warning?'<div class="pill orange">'+esc(d.warning)+'</div>':''}
    <h2 style="margin-top:12px">未结算用量 <span class="tag">来自租户 activity（唯一计量源）</span></h2>
    ${us.ok===false?'<div class="muted">租户侧不可达：'+esc(us.error||'')+'</div>':
      (ms.length?('<table><thead><tr><th>成员</th><th>消耗 TCoin</th><th>行数</th></tr></thead><tbody>'+
        ms.map(m=>`<tr><td>${esc(m.member_name||m.member_id)}</td><td>${m.tcoin}</td><td>${m.rows}</td></tr>`).join('')+
        '</tbody></table><div class="muted">合计 '+us.pending_tcoin+' TCoin（未结算，结算后计入平台总账）</div>'):'<div class="muted">无未结算消耗</div>')}
    <div class="row" style="margin-top:8px"><button class="btn btn-sm" onclick="settleTcoin('${esc(id)}')">结算（写入平台总账）</button></div>
    <h2 style="margin-top:12px">平台总账流水</h2>
    <table><thead><tr><th>seq</th><th>时间</th><th>账户</th><th>变动</th><th>原因</th></tr></thead><tbody>
    ${(d.entries||[]).map(e=>`<tr><td>${e.seq}</td><td>${esc((e.at||'').slice(0,16).replace('T',' '))}</td><td>${esc(e.account)}</td>
      <td style="color:${e.delta<0?'#F56C6C':'#4e9a2e'}">${e.delta>0?'+':''}${e.delta}</td><td>${esc(e.reason)}${e.ref?' <code>'+esc(e.ref)+'</code>':''}</td></tr>`).join('')||'<tr><td colspan="5" class="muted">暂无流水</td></tr>'}
    </tbody></table>
    ${d.allocations?('<h2 style="margin-top:12px">成员分配余额</h2><table><thead><tr><th>成员</th><th>余额</th></tr></thead><tbody>'+
      (d.allocations||[]).map(a=>`<tr><td><code>${esc((a.member_id||'').slice(0,12))}</code></td><td>${a.balance}</td></tr>`).join('')+'</tbody></table>'):''}`;
}
async function settleTcoin(id){
  const r=await jpost('/api/my/tcoin/'+encodeURIComponent(id)+'/settle',{});
  if(!r.ok){alert(T('结算失败：')+errText(r));return}
  alert(r.consumed?('已结算 '+r.consumed+' TCoin（批次 '+r.batch+'）'):'无未结算消耗');
  openTcoinTeam(id,null); loadTcoin();
}
async function loadTeamTcoin(id){
  const short=id.slice(0,8); const el=document.getElementById('tcTeam_'+short); if(!el)return;
  const d=await jget('/api/my/tcoin/'+encodeURIComponent(id));
  if(!d.ok){el.innerHTML='<span class="muted">不可用：'+esc(d.error||'')+'</span>';return}
  const us=d.usage||{};
  el.innerHTML=`<div class="pc-meta"><span>余额 <b>${d.balance}</b> TCoin</span><span>本团队累计消耗 <b>${d.consumed_total}</b></span>
    ${us.ok===false?'<span class="muted">租户侧不可达</span>':'<span>未结算 '+us.pending_tcoin+'</span>'}</div>
    <div class="row"><button class="btn btn-sm" onclick="settleTcoin('${esc(id)}').then(()=>loadTeamTcoin('${esc(id)}'))">结算未用消耗</button></div>`;
}

async function loadOwnerLetter(id){
  const short=id.slice(0,8); const el=document.getElementById('ownerLetter_'+short);
  const d=await jget('/api/my/letters');
  const x=(d.letters||[]).find(v=>v.team_id===id&&v.kind==='owner');
  if(!x||!x.available){el.innerHTML='<span class="muted">尚未生成（先点上面的「初始化团队」）</span>';return}
  el.innerHTML=`<button class="btn btn-sm" onclick="copyText(${JSON.stringify(x.letter||'')})">复制 owner 邀请函</button>
    <div class="muted" style="margin-top:6px">在本地执行 <code>teamx team import</code> 导入，即认领该团队的 owner 座位。</div>`;
}
// 打开**云 member** 的 teamx console（本地跑的那个是另一回事）
async function openMyConsole(team){
  const r=await jget('/api/my/team-console/'+encodeURIComponent(team));
  if(!r.ok){alert('无法打开：'+errText(r));return}
  window.open(r.url,'_blank','noopener');
}
async function loadRoster(id){
  const short=id.slice(0,8); const el=document.getElementById('roster_'+short);
  const d=await jget('/api/my/teams/'+encodeURIComponent(id)+'/roster');
  if(!d.ok){el.innerHTML='<span class="muted">不可用：'+esc(d.error||'')+'</span>';return}
  const ms=d.members||[];
  el.innerHTML=ms.length?('<table><thead><tr><th>成员</th><th>角色</th><th>状态</th></tr></thead><tbody>'+
    ms.map(m=>`<tr><td>${esc(m.display_name)}${m.is_lead?' <span class="tag-j">lead</span>':''}${m.is_me?` <a class="link" href="javascript:void(0)" onclick="openMyConsole('${esc(id)}')">Teamx Console ↗</a>`:''}</td><td>${esc(m.role)}</td><td>${esc(m.state)}</td></tr>`).join('')+'</tbody></table>'):'<div class="muted">暂无成员</div>';
}
async function inviteMember(id){
  const short=id.slice(0,8);
  const role_desc=document.getElementById('invRole_'+short).value.trim()||'contributor: 成员';
  const r=await jpost('/api/my/teams/'+encodeURIComponent(id)+'/invite',{role_desc});
  const out=document.getElementById('invOut_'+short); out.style.display='block';
  if(!r.ok){out.textContent='失败：'+errText(r);return}
  out.textContent=r.letter||JSON.stringify(r,null,2);
  alert('邀请函已生成，可复制发给成员');
  loadRoster(id);   // 新席位可能已建，刷新名册
}
async function leaveTeam(id,name){
  if(!confirm('离开团队「'+name+'」？\n\n会释放我的座位、回收我的工作区（PVC 保留 7 天），之后可再次申请。'))return;
  const r=await jpost('/api/my/teams/'+encodeURIComponent(id)+'/leave',{});
  if(!r.ok){alert(T('离开失败：')+errText(r));return}
  alert('已离开'+(r.pod_deleted?'（工作区已回收）':'（工作区待回收）'));
  loadMine();
}
async function loadLetters(){
  const d=await jget('/api/my/letters'); const el=document.getElementById('lettersList');
  if(!d.ok){el.innerHTML='<span class="muted">'+esc(d.error||'')+'</span>';return}
  const xs=(d.letters||[]).filter(x=>x.kind==='member');
  el.innerHTML=xs.length?('<table><thead><tr><th>团队</th><th>成员邀请函</th></tr></thead><tbody>'+
    xs.map(t=>`<tr><td>${esc(t.team_name||t.team_id)}</td><td>${t.available?`<button class="btn btn-sm" onclick="copyText(${JSON.stringify(t.letter||'')})">复制邀请函</button>`:'<span class="muted">尚未生成（等待审批生效）</span>'}</td></tr>`).join('')+
    '</tbody></table>'):'<div class="muted">还没有加入任何团队<br><br><button class="btn btn-sm" onclick="showTab(\'all\')">去全部团队发现</button></div>';
}
async function createMyTeam(){
  const name=document.getElementById('ncName').value.trim(); if(!name)return;
  const visibility=document.getElementById('ncVis').value, join_policy=document.getElementById('ncJoin').value;
  const summary=document.getElementById('ncSum').value;
  const r=await jpost('/api/my/teams',{name,visibility,join_policy,summary});
  if(!r.ok){alert(errText(r));return}
  await loadMine();
  if(r.team_id)openMineDetail(r.team_id,'owned',null);
  alert('团队已创建（空账本）。平台正在拉起 server（约 1 分钟）；就绪后点详情里的「初始化团队」，再取 Owner 邀请函导入。');
}
async function saveDiscover(team){
  const short=team.slice(0,8);
  const visibility=document.getElementById('vis_'+short).value;
  const join_policy=document.getElementById('jp_'+short).value;
  const summary=document.getElementById('sum_'+short).value;
  const r=await jpost('/api/discovery/teams/'+encodeURIComponent(team)+'/settings',{visibility,join_policy,summary});
  if(!r.ok){alert(errText(r));return}
  alert('已保存：'+(visibility==='public'?'公开':'私有')+' / '+(join_policy==='open_request'?'允许申请':'仅邀请'));
  loadDir(1);
}
async function bootstrapTeam(team){
  team=team||'';const short=team.slice(0,8);
  const urlEl=document.getElementById('bsUrl_'+short);
  const server_url=(urlEl?urlEl.value:'').trim();
  if(!team){alert('缺少 team id');return}
  const r=await jpost('/api/my/teams/'+encodeURIComponent(team)+'/bootstrap',{server_url});
  if(!r.ok){alert(T('初始化失败：')+errText(r));return}
  await loadMine();
  // 此前这里写向一个已不存在的元素（letterOut）→ 抛错导致「初始化成功却不刷新」。
  openMineDetail(team,'owned',null);
}
async function loadTenants(){const d=await jget('/api/tenants');
 window.__tenants=d.tenants||[];
 const tb=document.getElementById('tenants');
 tb.innerHTML=window.__tenants.length?window.__tenants.map(t=>`
   <div class="row-item" data-id="${esc(t.team_id)}" onclick="openTenantDetail('${esc(t.team_id)}',this)">
     <span class="ri-title">${esc(t.team_name||t.team_id)}</span>
     <span class="ri-meta">${esc(t.namespace)}</span>
   </div>`).join(''):'<div class="detail-empty">暂无租户</div>'}
function openTenantDetail(id,el){
  markSel(el);
  const t=(window.__tenants||[]).find(x=>x.team_id===id)||{};
  document.getElementById('tenantDetail').innerHTML=`
    <div class="team-head"><h3>${esc(t.team_name||id)}</h3><span class="pill">租户</span></div>
    <div class="pc-meta"><span>team <code>${esc(id)}</code></span><span>ns <code>${esc(t.namespace||'')}</code></span></div>
    <div class="pc-meta"><span>CA <code>${esc((t.ca_fingerprint||'').slice(0,24))}…</code></span><span>镜像 ${esc(t.server_image||'—')}</span></div>
    <div class="pc-meta"><span>owner ${esc(t.owner_user_id||'—')}</span><span>创建 ${esc((t.created_at||'').slice(0,10))}</span></div>
    <div class="boot-row"><button class="btn btn-sm btn-danger" onclick="destroyTenant('${esc(id)}')">回收租户</button></div>`;
}
async function loadPlatform(){const [t,a,au]=await Promise.all([jget('/api/tokens'),jget('/api/admins'),jget('/api/audit')]);
 document.getElementById('tokens').innerHTML=(t.tokens||[]).map(x=>`<tr><td><code>${esc(x.id.slice(0,8))}</code></td><td><span class="pill">${esc(x.state)}</span></td><td class="muted">${esc(x.created_at)}</td><td>${esc(x.redeemed_by||'—')}</td></tr>`).join('')||'<tr><td colspan=4 class="muted">无</td></tr>';
 document.getElementById('admins').innerHTML=(a.admins||[]).map(x=>`<tr><td>${esc(x.user_name)}</td><td class="muted">${esc(x.granted_at)}</td><td class="muted">${esc(x.granted_via)}</td></tr>`).join('')||'<tr><td colspan=3 class="muted">无</td></tr>';
 document.getElementById('audit').innerHTML=(au.audit||[]).map(x=>`<tr><td class="muted">${esc(x.at)}</td><td><code>${esc(x.actor)}</code></td><td>${esc(x.action)}</td><td class="muted">${esc(x.detail||'')}</td></tr>`).join('')||'<tr><td colspan=4 class="muted">无</td></tr>'}
async function createTenant(){const team=document.getElementById('team').value.trim();if(!team)return;const r=await jpost('/api/tenants',{team});if(!r.ok)alert(r.error);await loadTenants();await loadPlatform()}
async function destroyTenant(t,teamName){
  const expect=teamName||t;
  const typed=prompt('回收租户「'+expect+'」是不可逆操作，将销毁：\n  · 租户 server 与全部数据\n  · 租户独立 CA（成员证书立即失效）\n  · 成员工作区（成员会失联）\n\n请输入团队名确认：');
  if(typed===null)return;
  if(typed.trim()!==expect.trim()){alert('团队名不匹配，已取消');return}
  const r=await jdel('/api/tenants/'+encodeURIComponent(t));
  if(!r.ok){alert(errText(r));return}
  loadMine();loadTenants();loadPlatform();
}
/* ===== 启动（放最后：DICT/ERR 等已赋值）===== */
try{
  initLang();
  initTheme();
  var scriptEl=document.getElementById('scriptState');
  if(scriptEl) scriptEl.textContent='脚本已就绪';
}catch(e){ showFatal('init: ' + (e && e.message || e)); }
try{
  const q=new URLSearchParams(location.search);
  if(q.get('token')){
    fetch('/api/login',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({token:q.get('token')})})
      .then(()=>{history.replaceState({},'',location.pathname);boot()});
  } else {
    boot();
  }
}catch(e){ showFatal('boot: ' + (e && e.message || e)); }
</script>
</body></html>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_guard_soft_backoff_then_hard_lock_then_clear() {
        use std::time::{Duration, Instant};
        let mut g = LoginGuard::new();
        let t0 = Instant::now();
        let keys = vec!["ip:1.2.3.4".to_string(), "user:bob".to_string()];

        // 4 次失败：未到软阈值 → 不拦
        for _ in 0..4 {
            let (w, locked) = g.fail(&keys, t0);
            assert_eq!(w, 0);
            assert!(!locked);
        }
        assert!(g.blocked_for(&keys, t0).is_none(), "4 次不应拦");

        // 第 5 次 → 指数退避
        let (w, locked) = g.fail(&keys, t0);
        assert!(!locked);
        assert!(w >= 1, "第 5 次应有退避: {w}");
        assert!(g.blocked_for(&keys, t0).is_some(), "超软阈值应拦");

        // 成功登录 → 清零
        g.ok(&keys);
        assert!(g.blocked_for(&keys, t0).is_none(), "成功应清零");

        // 连打 10 次 → 锁定
        let mut locked_any = false;
        for _ in 0..10 {
            let (_, l) = g.fail(&keys, t0);
            locked_any |= l;
        }
        assert!(locked_any, "10 次失败应触发锁定");
        let wait = g.blocked_for(&keys, t0).expect("锁定中应拦");
        assert!(wait >= 800, "锁定等待应接近 900s: {wait}");

        // 账号维度独立：换个 IP 仍被同一账号挡住
        let keys2 = vec!["ip:9.9.9.9".to_string(), "user:bob".to_string()];
        assert!(g.blocked_for(&keys2, t0).is_some(), "账号维度须独立生效");

        // 时间推进过锁定期 → 放行（窗口外的失败也被 prune）
        let t1 = t0 + Duration::from_secs(16 * 60);
        assert!(g.blocked_for(&keys, t1).is_none(), "锁定期过后应放行");

        // 退避是**从最后一次失败起算的截止时间**（回归：曾写成常量 → 失败留在窗口内永不解除）
        let mut g2 = LoginGuard::new();
        for _ in 0..5 {
            g2.fail(&keys, t0);
        }
        assert!(g2.blocked_for(&keys, t0).is_some(), "刚失败应拦");
        let t_backoff = t0 + Duration::from_secs(backoff_secs(5 - 3) + 1);
        assert!(
            g2.blocked_for(&keys, t_backoff).is_none(),
            "退避到期即应解除（仍在 15 分钟失败窗口内）"
        );
    }

    #[test]
    fn login_guard_blocks_right_after_soft_threshold() {
        use std::time::{Duration, Instant};
        let mut g = LoginGuard::new(); // soft=3? 默认 soft=5；显式设 3 以模拟 E2E 配置
        g.soft = 3;
        let t0 = Instant::now();
        let keys = vec!["ip:127.0.0.1".to_string(), "user:bob".to_string()];
        for i in 0..3 {
            let (w, locked) = g.fail(&keys, t0 + Duration::from_millis(i * 10));
            eprintln!("fail #{i}: wait={w} locked={locked}");
        }
        // 第 3 次失败之后 ~100ms 再来一次（正确密码）→ 必须仍被拦
        let b = g.blocked_for(&keys, t0 + Duration::from_millis(100));
        assert!(b.is_some(), "超软阈值后 100ms 应仍被拦（首步退避 2s）");
        // 退避到期（2s + 余量）→ 放行
        assert!(
            g.blocked_for(&keys, t0 + Duration::from_millis(2500)).is_none(),
            "退避到期应放行"
        );
    }

    #[test]
    fn console_has_bilingual_infrastructure() {
        // design/019 SD-14：内嵌词典 + cookie 语言 + 服务端 error code → 前端映射
        for needle in [
            "var DICT = {",
            "var ERR = {",
            "function applyLangNode(",
            "function setLang(",
            "function initLang(",
            "MutationObserver(",
            "teamx_lang",           // cookie 持久化
            "function errText(",    // 服务端 code → 文案
            "lg-zh", "lg-en",       // 切换器
            "initLang();",          // 启动即生效
            "function tlogin(",     // 登录入口（内联 onsubmit；脚本局部失败也能登录）
            "onsubmit=\"return tlogin(event)\"",
            "id=\"scriptState\"",       // 「脚本已就绪」可见标记（诊断）
        ] {
            assert!(INDEX_HTML.contains(needle), "双语基建缺少: {needle}");
        }
        // 词表规模与关键翻译
        let dict = &INDEX_HTML[INDEX_HTML.find("var DICT = {").unwrap()..];
        let dict = &dict[..dict.find("\n};").unwrap_or(dict.len())];
        let entries = dict.matches("\":\"").count();
        assert!(entries >= 120, "词表条目过少: {entries}");
        for (zh, en) in [
            ("\"加载中…\":\"Loading…\"", ""),
            ("\"全部团队\":\"All teams\"", ""),
            ("\"我的团队\":\"My teams\"", ""),
            ("\"退出\":\"Sign out\"", ""),
            ("\"登录\":\"Sign in\"", ""),
        ] {
            let _ = en;
            assert!(dict.contains(zh), "词表缺少 {zh}");
        }
        // 错误码映射（服务端只回码）
        assert!(INDEX_HTML.contains("invalid_credentials"));
        assert!(INDEX_HTML.contains("too_many_attempts"));
    }

    #[test]
    fn err_code_maps_known_messages() {
        assert_eq!(err_code("invalid credentials"), "invalid_credentials");
        assert_eq!(err_code("too_many_attempts"), "too_many_attempts");
        assert_eq!(err_code("not your tenant"), "not_your_tenant");
        assert_eq!(err_code("only the owner can settle this team"), "owner_only");
        assert_eq!(err_code("tenant xyz not found"), "not_found");
        assert_eq!(
            err_code("cannot reach tenant server (auto port-forward to ns/svc not ready)"),
            "tenant_unreachable"
        );
        assert_eq!(err_code("GitHub OAuth not configured"), "github_not_configured");
        assert_eq!(err_code("something weird"), "request_failed");
    }

    /// 防回归：静态 `getElementById('x')` 必须有对应的 `id="x"`。
    ///
    /// 历史踩坑：`logout()` 里的 `logoutBtn`、`bootstrapTeam()` 里的 `letterOut` 都是
    /// **指向不存在元素**的死引用 —— 静默抛 TypeError，表现为「点退出没反应」
    /// 「初始化成功却不刷新」。这类 bug 只有真机点出来才发现，故用测试兜住。
    #[test]
    fn index_html_get_element_by_id_targets_exist() {
        use std::collections::HashSet;
        let html = INDEX_HTML;
        let mut ids: HashSet<String> = HashSet::new();
        let mut rest = html;
        while let Some(p) = rest.find("id=\"") {
            let tail = &rest[p + 4..];
            if let Some(q) = tail.find('"') {
                ids.insert(tail[..q].to_string());
            }
            rest = &rest[p + 4..];
        }
        // 运行时创建的 id（如 showFatal 里的 createElement + el.id='fatalErr'）也算已知
        let mut rest = html;
        while let Some(p) = rest.find(".id='") {
            let tail = &rest[p + 5..];
            if let Some(q) = tail.find('\'') {
                ids.insert(tail[..q].to_string());
            }
            rest = &rest[p + 5..];
        }
        let mut missing: Vec<String> = Vec::new();
        let mut rest = html;
        while let Some(p) = rest.find("getElementById('") {
            let tail = &rest[p + 16..];
            if let Some(q) = tail.find('\'') {
                let name = &tail[..q];
                let after = tail.get(q + 1..).unwrap_or("");
                // 跳过拼接/模板引用（如 getElementById('sub-'+n)）：
                // 名字里含 +/${ 属于运行时拼出来；名字后紧跟 + 也是拼接。
                if !name.contains('+')
                    && !name.contains("${")
                    && !after.starts_with('+')
                    && !ids.contains(name)
                {
                    missing.push(name.to_string());
                }
            }
            rest = &rest[p + 16..];
        }
        missing.sort();
        missing.dedup();
        assert!(missing.is_empty(), "getElementById 指向不存在的 id: {missing:?}");
    }

    #[test]
    fn index_html_has_platform_surfaces() {
        for needle in ["平台控制台", "/api/tenants", "/invite", "平台审计", "GitHub OAuth",
            "/api/my/teams", "bootstrapTeam", "createMyTeam", "loginPassword", "logout",
            "pwForm", "tab-all", "discovery/directory", "loadDir", "usermenu", "openMyRequests", "saveDiscover",
            "openTcoin", "loadTcoin", "settleTcoin", "tab-tcoin", "tcList", "tcDetail",
            "openCredit", "loadCredit", "refreshCredit", "tab-credit", "crList", "crDetail", "credit_grade",
            "split-detail", "row-item", "markSel", "openTenantDetail", "reqMode", "btn-primary", "copyText",
            "loadLetters", "leaveTeam", "loadRoster", "inviteMember", "ncName", "ncVis", "ncJoin",
            "list-head", "mineCount", "openCreateForm", "openLetters", "mineDetail", "创建团队", "加入团队",
            "toggleInvite", "invBox_",  // 团队成员卡上的「邀请成员」入口
            "loadOwnerLetter", "ownerLetter_",
            "function esc(", "esc(t.team_name", "esc(t.namespace", "esc(t.server_image"] {
            assert!(INDEX_HTML.contains(needle), "missing {needle}");
        }
    }

    fn test_ctx() -> Ctx {
        Ctx {
            db_path: std::path::PathBuf::from(":memory:"),
            launch_token: "launch".to_string(),
            kubeconfig: None,
            context: None,
            login_guard: std::sync::Arc::new(std::sync::Mutex::new(LoginGuard::new())),
            oauth_states: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            console_page: "/console".to_string(),
            console_target_override: None,
        }
    }

    #[test]
    fn cookie_token_reads_all_cookie_fields() {
        use axum::http::{HeaderMap, HeaderValue};
        let mut h = HeaderMap::new();
        // 单字段多 cookie
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; teamx_platform_token=TOK123; b=2"),
        );
        assert_eq!(cookie_token(&h).as_deref(), Some("TOK123"));
        // HTTP/2 拆分多字段（Firefox 行为）：token 在第二个字段
        let mut h2 = HeaderMap::new();
        h2.insert(header::COOKIE, HeaderValue::from_static("oc_locale=en"));
        h2.append(header::COOKIE, HeaderValue::from_static("teamx_platform_token=TOK456"));
        assert_eq!(cookie_token(&h2).as_deref(), Some("TOK456"));
        // 无关 cookie → None
        let mut h3 = HeaderMap::new();
        h3.insert(header::COOKIE, HeaderValue::from_static("a=1"));
        assert_eq!(cookie_token(&h3), None);
        // 多 cookie 场景：必须返回**全部**候选（顺序保持、去重），鉴权逐个尝试
        let mut multi = HeaderMap::new();
        multi.insert(
            header::COOKIE,
            HeaderValue::from_static("teamx_platform_token=STALE; other=1; teamx_platform_token=GOOD"),
        );
        assert_eq!(cookie_tokens(&multi), vec!["STALE".to_string(), "GOOD".to_string()]);
        let mut dup = HeaderMap::new();
        dup.insert(header::COOKIE, HeaderValue::from_static("teamx_platform_token=X; teamx_platform_token=X"));
        assert_eq!(cookie_tokens(&dup), vec!["X".to_string()], "同名同值应去重");
    }

    #[test]
    fn oauth_state_single_use() {
        let ctx = test_ctx();
        let s = oauth_issue(&ctx);
        assert!(!s.is_empty());
        assert!(oauth_consume(&ctx, &s), "首次消费应成功");
        assert!(!oauth_consume(&ctx, &s), "复用必须拒绝（CSRF）");
        assert!(!oauth_consume(&ctx, ""), "空 state 拒绝");
        assert!(!oauth_consume(&ctx, "nope"), "未知 state 拒绝");
    }

    #[test]
    fn oauth_is_disabled_without_credentials() {
        // 未配置时不得"假装可用"
        std::env::remove_var("TEAMX_GITHUB_CLIENT_ID");
        std::env::remove_var("TEAMX_GITHUB_CLIENT_SECRET");
        assert!(!github_configured());
    }

    // -----------------------------------------------------------------------
    // `/c/{member16}` 反代 E2E（design/021 §J）
    // -----------------------------------------------------------------------

    use axum::extract::ws::{Message as WsMessage, WebSocketUpgrade};
    use axum::response::Html;
    use std::net::SocketAddr;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// 假成员 console：index（HTML）/ api_me（校验 console token）/ WS echo。
    fn fake_console_app(token: String) -> Router {
        let tok_me = token.clone();
        Router::new()
            .route(
                "/",
                get(|| async {
                    Html("<!doctype html><html><head><title>Teamx Console</title></head><body>FAKE-CONSOLE-INDEX</body></html>")
                }),
            )
            .route(
                "/api/me",
                get(move |headers: HeaderMap| {
                    let tok = tok_me.clone();
                    async move {
                        let got = headers
                            .get(header::COOKIE)
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("");
                        if got == format!("teamx_console_token={tok}") {
                            Json(json!({"ok": true, "data": {"name": "seat-member"}})).into_response()
                        } else {
                            (StatusCode::FORBIDDEN, "missing console token").into_response()
                        }
                    }
                }),
            )
            .route(
                "/api/cloud/{id}/ws",
                get(|ws: WebSocketUpgrade| async move {
                    ws.on_upgrade(|mut socket| async move {
                        while let Some(Ok(m)) = socket.recv().await {
                            match m {
                                WsMessage::Text(t) => {
                                    let _ = socket.send(WsMessage::Text(t)).await;
                                }
                                WsMessage::Binary(b) => {
                                    let _ = socket.send(WsMessage::Binary(b)).await;
                                }
                                WsMessage::Close(_) => break,
                                _ => {}
                            }
                        }
                    })
                }),
            )
            .route(
                "/api/repos/clone",
                post(|headers: HeaderMap, body: axum::body::Bytes| async move {
                    let ct = headers
                        .get(header::CONTENT_TYPE)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_string();
                    (
                        [(header::SET_COOKIE, "upstream=1; Path=/")],
                        Json(json!({
                            "ok": true,
                            "ct": ct,
                            "echo": String::from_utf8_lossy(&body),
                        })),
                    )
                }),
            )
    }

    /// 起一个假成员 console（可选自签 TLS），返回 base URL 与任务句柄。
    async fn spawn_fake_console(tls: bool, token: String) -> (String, tokio::task::JoinHandle<()>) {
        let app = fake_console_app(token);
        let port = free_port();
        let addr = format!("127.0.0.1:{port}");
        if tls {
            let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
            let cert_pem = cert.cert.pem();
            let key_pem = cert.key_pair.serialize_pem();
            let config = axum_server::tls_rustls::RustlsConfig::from_pem(cert_pem.into_bytes(), key_pem.into_bytes())
                .await
                .unwrap();
            let sock: SocketAddr = addr.parse().unwrap();
            let h = tokio::spawn(async move {
                let _ = axum_server::bind_rustls(sock, config)
                    .serve(app.into_make_service())
                    .await;
            });
            (format!("https://{addr}"), h)
        } else {
            let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
            let h = tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });
            (format!("http://{addr}"), h)
        }
    }

    fn mktmp_db(tag: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "teamx-pc-console-{}-{}-{}.db",
            std::process::id(),
            tag,
            uuid::Uuid::new_v4().simple()
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    /// 建一个平台 DB：用户 + 会话 + 一个 owner 租户（owner_member_id = seat）。
    fn setup_console_ctx(override_url: Option<String>) -> (Arc<Ctx>, std::path::PathBuf, String, String) {
        let db = mktmp_db("console");
        {
            let conn = crate::db::open(&db).unwrap();
            crate::db::migrate(&conn).unwrap();
        }
        let mut conn = crate::db::open(&db).unwrap();
        let user = crate::platform::create_user(&mut conn, "alice-console", "password123").unwrap();
        let uid = user["user_id"].as_str().unwrap().to_string();
        let session =
            crate::platform::issue_session(&mut conn, &uid).unwrap()["token"].as_str().unwrap().to_string();
        let team = "team-console-e2e";
        let seat = "474a5a6b-2440-4c1c-b24c-4d2044589b65";
        conn.execute(
            "INSERT OR REPLACE INTO teams (id,name,invite_token,created_at,updated_at) VALUES (?1,?1,'tok','2026-01-01',?1)",
            rusqlite::params![team],
        )
        .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO tenants (team_id,namespace,ca_fingerprint,created_at,owner_member_id) VALUES (?1,'tenant-x','fp','2026-01-01',?2)",
            rusqlite::params![team, seat],
        )
        .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO tenant_owners (team_id,user_id,created_at) VALUES (?1,?2,'2026-01-01')",
            rusqlite::params![team, uid],
        )
        .unwrap();
        drop(conn);
        let ctx = Arc::new(Ctx {
            db_path: db.clone(),
            launch_token: "launch".into(),
            kubeconfig: None,
            context: None,
            login_guard: std::sync::Arc::new(std::sync::Mutex::new(LoginGuard::new())),
            oauth_states: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            console_page: "/console".into(),
            console_target_override: override_url,
        });
        (ctx, db, session, seat.to_string())
    }

    async fn spawn_platform(ctx: Arc<Ctx>) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let app = build_router(ctx, false);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let h = tokio::spawn(async move {
            let _ = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await;
        });
        (addr, h)
    }

    async fn raw_get(addr: SocketAddr, path: &str, cookie: Option<&str>) -> (u16, Vec<(String, String)>, String) {
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut req = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n");
        if let Some(c) = cookie {
            req.push_str(&format!("Cookie: {c}\r\n"));
        }
        req.push_str("\r\n");
        s.write_all(req.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.unwrap();
        let text = String::from_utf8_lossy(&buf).to_string();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_str(), ""));
        let status: u16 = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        let mut hs = Vec::new();
        for l in head.lines().skip(1) {
            if let Some((k, v)) = l.split_once(':') {
                hs.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
            }
        }
        (status, hs, body.to_string())
    }

    async fn raw_post(
        addr: SocketAddr,
        path: &str,
        cookie: Option<&str>,
        content_type: &str,
        body: &[u8],
    ) -> (u16, Vec<(String, String)>, String) {
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut req = format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n",
            body.len()
        );
        if let Some(c) = cookie {
            req.push_str(&format!("Cookie: {c}\r\n"));
        }
        req.push_str("\r\n");
        s.write_all(req.as_bytes()).await.unwrap();
        s.write_all(body).await.unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.unwrap();
        let text = String::from_utf8_lossy(&buf).to_string();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_str(), ""));
        let status: u16 = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        let mut hs = Vec::new();
        for l in head.lines().skip(1) {
            if let Some((k, v)) = l.split_once(':') {
                hs.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
            }
        }
        (status, hs, body.to_string())
    }

    /// Regression (Bug 1): the `/c/{member16}` routes must accept POST (and
    /// every other method) and forward the request body + Content-Type to the
    /// member console, while dropping the upstream `Set-Cookie`.
    #[tokio::test]
    async fn console_route_forwards_post_body_and_drops_set_cookie() {
        let (target, _fake) = spawn_fake_console(false, "tok".into()).await;
        let (ctx, _db, session, seat) = setup_console_ctx(Some(target));
        let (addr, _plat) = spawn_platform(ctx).await;
        let m16 = crate::k8s::short_id(&seat);

        let (status, headers, body) = raw_post(
            addr,
            &format!("/c/{m16}/api/repos/clone"),
            Some(&format!("teamx_platform_token={session}")),
            "application/json",
            br#"{"name":"yongyan-dev"}"#,
        )
        .await;
        assert_eq!(status, 200, "POST 必须被反代（不能 405）: {body}");
        assert!(body.contains("yongyan-dev"), "上游必须收到 POST body: {body}");
        assert!(body.contains("\"ct\":\"application/json\""), "Content-Type 必须保留: {body}");
        assert!(
            !headers.iter().any(|(k, _)| k == "set-cookie"),
            "上游 Set-Cookie 必须丢弃: {headers:?}"
        );
    }

    #[tokio::test]
    async fn console_route_without_session_redirects_to_login() {
        let (target, _fake) = spawn_fake_console(false, "tok".into()).await;
        let (ctx, _db, _session, seat) = setup_console_ctx(Some(target));
        let (addr, _plat) = spawn_platform(ctx).await;
        let m16 = crate::k8s::short_id(&seat);

        let (status, headers, _) = raw_get(addr, &format!("/c/{m16}"), None).await;
        assert_eq!(status, 303, "未登录必须跳登录页");
        let loc = headers.iter().find(|(k, _)| k == "location").map(|(_, v)| v.clone()).unwrap_or_default();
        assert_eq!(loc, "/console");

        // 页面下的 API 路径同样先过平台会话（不泄露 console token）
        let (status2, _, _) = raw_get(addr, &format!("/c/{m16}/api/me"), None).await;
        assert_eq!(status2, 303);
    }

    #[tokio::test]
    async fn console_route_with_session_serves_member_console_and_injects_base() {
        let seat = "474a5a6b-2440-4c1c-b24c-4d2044589b65";
        let tok = crate::k8s::console_token_for(seat);
        let (target, _fake) = spawn_fake_console(true, tok).await; // 自签 TLS 上游
        let (ctx, _db, session, seat) = setup_console_ctx(Some(target));
        let (addr, _plat) = spawn_platform(ctx).await;
        let m16 = crate::k8s::short_id(&seat);
        let cookie = format!("teamx_platform_token={session}");

        let (status, headers, body) = raw_get(addr, &format!("/c/{m16}"), Some(&cookie)).await;
        assert_eq!(status, 200, "有会话应 200（自签上游 TLS 必须跳过校验）: {body}");
        assert!(body.contains("FAKE-CONSOLE-INDEX"), "页面必须来自 member console: {body}");
        assert!(body.contains(&format!("<base href=\"/c/{m16}/\">")), "必须注入前缀 base");
        assert!(
            !headers.iter().any(|(k, _)| k == "set-cookie"),
            "console token cookie 绝不能下发浏览器: {headers:?}"
        );

        // 平台把确定性 token 注入上游 Cookie → fake /api/me 校验通过
        let (s2, _, b2) = raw_get(addr, &format!("/c/{m16}/api/me"), Some(&cookie)).await;
        assert_eq!(s2, 200, "平台必须向上游注入 console token: {b2}");
        assert!(b2.contains("\"ok\":true"), "{b2}");
    }

    #[tokio::test]
    async fn my_team_console_url_is_prefix_path_without_token() {
        let (target, _fake) = spawn_fake_console(false, "tok".into()).await;
        let (ctx, _db, session, seat) = setup_console_ctx(Some(target));
        let (addr, _plat) = spawn_platform(ctx).await;
        let m16 = crate::k8s::short_id(&seat);
        let (status, _, body) = raw_get(
            addr,
            "/api/my/team-console/team-console-e2e",
            Some(&format!("teamx_platform_token={session}")),
        )
        .await;
        assert_eq!(status, 200);
        assert!(body.contains(&format!("\"url\":\"/c/{m16}\"")), "URL 必须改为前缀路径: {body}");
        assert!(!body.contains("token"), "浏览器 URL 里绝不能出现 console token: {body}");
    }

    #[tokio::test]
    async fn console_route_rejects_seat_not_owned_by_user() {        let (target, _fake) = spawn_fake_console(false, "tok".into()).await;
        let (ctx, _db, session, _seat) = setup_console_ctx(Some(target));
        let (addr, _plat) = spawn_platform(ctx).await;
        // 合法 member16 形状，但不在本人席位里 → 404（不越权）
        let (status, _, _) = raw_get(addr, "/c/deadbeefdeadbeef", Some(&format!("teamx_platform_token={session}"))).await;
        assert_eq!(status, 404);
    }

    #[tokio::test]
    async fn console_ws_relays_over_self_signed_tls() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let seat = "474a5a6b-2440-4c1c-b24c-4d2044589b65";
        let tok = crate::k8s::console_token_for(seat);
        let (target, _fake) = spawn_fake_console(true, tok).await; // 自签 TLS 上游
        let (ctx, _db, session, seat) = setup_console_ctx(Some(target));
        let (addr, _plat) = spawn_platform(ctx).await;
        let m16 = crate::k8s::short_id(&seat);

        let url = format!("ws://{addr}/c/{m16}/api/cloud/42/ws");
        let mut req = url.as_str().into_client_request().unwrap();
        req.headers_mut()
            .insert("cookie", format!("teamx_platform_token={session}").parse().unwrap());
        let (mut ws, _resp) = tokio_tungstenite::connect_async(req).await.unwrap();
        ws.send(tokio_tungstenite::tungstenite::Message::Text("hello-ws".into()))
            .await
            .unwrap();
        let got = ws.next().await.unwrap().unwrap();
        assert_eq!(got.into_text().unwrap(), "hello-ws", "WS 必须经自签 TLS 上游回环");
    }
}
