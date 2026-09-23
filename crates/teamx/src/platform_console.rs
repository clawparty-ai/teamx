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
    response::{Html, IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use axum_server::tls_rustls::RustlsConfig;
use serde_json::{json, Value};

use crate::cli::PlatformConsoleCmd;

/// 平台控制台状态：平台 DB 句柄 + launch token + K8s 访问参数 + OAuth state 表。
#[derive(Clone)]
struct Ctx {
    db_path: std::path::PathBuf,
    launch_token: String,
    kubeconfig: Option<String>,
    context: Option<String>,
    /// OAuth state（CSRF）：state → 发起时间；单次有效，10 分钟过期。
    /// 内存态（单进程控制台；重启后未完成的登录作废，需重走 start）。
    oauth_states: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>>,
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
    let ctx = Arc::new(Ctx {
        db_path,
        launch_token: launch_token.clone(),
        kubeconfig: cmd.kubeconfig.as_ref().map(|p| p.to_string_lossy().to_string()),
        context: cmd.context.clone(),
        oauth_states: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/api/login", post(api_login))
        .route("/api/login/password", post(api_login_password))
        .route("/api/logout", post(api_logout))
        .route("/api/me", get(api_me))
        .route("/api/tenants", get(api_tenants).post(api_tenant_create))
        .route("/api/my/teams", get(api_my_teams).post(api_my_teams_create))
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
        .with_state(ctx.clone());

    // TLS：用平台自身 instance 证书（`<teamx_home>/ca/server.*`），与另外两个 console 同源
    let home = crate::db::teamx_home();
    let pk = crate::pki::ensure_pki(&home).map_err(|e| format!("pki: {e}"))?;
    let cert_pem = std::fs::read(&pk.server_cert).map_err(|e| format!("read cert: {e}"))?;
    let key_pem = std::fs::read(&pk.server_key).map_err(|e| format!("read key: {e}"))?;
    let config = RustlsConfig::from_pem(cert_pem, key_pem)
        .await
        .map_err(|e| format!("tls config: {e}"))?;

    let addr: std::net::SocketAddr = format!("{}:{}", cmd.addr, cmd.port)
        .parse()
        .map_err(|e| format!("bad bind addr: {e}"))?;
    eprintln!("teamx platform console (platform plane)");
    eprintln!("  open: https://{addr}/?token={launch_token}");
    eprintln!("  (self-signed cert; your browser will warn about it once)");
    eprintln!("  GitHub OAuth: {}", if github_configured() { "enabled" } else { "disabled (set TEAMX_GITHUB_CLIENT_ID/SECRET)" });
    axum_server::bind_rustls(addr, config)
        .serve(app.into_make_service())
        .await
        .map_err(|e| format!("serve: {e}"))
}

// ---------------------------------------------------------------------------
// 认证
// ---------------------------------------------------------------------------

fn cookie_token(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';')
        .map(str::trim)
        .find_map(|kv| kv.strip_prefix("teamx_platform_token=").map(str::to_string))
}

/// 平台面鉴权（fail-closed）：cookie 是 launch token（运维）**或**有效的用户会话（Phase 1）。
/// 先比 launch token（无 DB 开销），再验会话（查库 + 过期/禁用检查）。
#[allow(clippy::result_large_err)] // Response 作为错误值直接返回，避免多层包装
fn guard(ctx: &Ctx, headers: &HeaderMap) -> Result<(), Response> {
    match cookie_token(headers) {
        Some(t) if t == ctx.launch_token => Ok(()),
        Some(t) => match with_conn(ctx, |c| crate::platform::verify_session(c, &t)) {
            Ok(_) => Ok(()),
            Err(_) => Err(unauth()),
        },
        None => Err(unauth()),
    }
}

fn unauth() -> Response {
    (StatusCode::UNAUTHORIZED, Json(json!({"ok": false, "error": "unauthorized"}))).into_response()
}

/// 会话对应的平台用户（None = 运维 launch-token 或未登录）。
fn session_user(ctx: &Ctx, headers: &HeaderMap) -> Option<(String, String)> {
    let t = cookie_token(headers)?;
    if t == ctx.launch_token {
        return None;
    }
    with_conn(ctx, |c| crate::platform::verify_session(c, &t))
        .ok()
        .map(|v| {
            (
                v["user_id"].as_str().unwrap_or("").to_string(),
                v["display_name"].as_str().unwrap_or("").to_string(),
            )
        })
}

/// 当前身份（展示 + 审计用）：运维 launch token 或已登录用户。
fn identity(ctx: &Ctx, headers: &HeaderMap) -> String {
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

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn api_login(State(ctx): State<Arc<Ctx>>, Json(body): Json<Value>) -> Response {
    let token = body.get("token").and_then(Value::as_str).unwrap_or("");
    if token != ctx.launch_token {
        return (StatusCode::UNAUTHORIZED, Json(json!({"ok": false, "error": "bad token"}))).into_response();
    }
    let mut resp = Json(json!({"ok": true})).into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        format!("teamx_platform_token={}; Path=/; HttpOnly; SameSite=Strict", ctx.launch_token)
            .parse()
            .unwrap(),
    );
    resp
}

/// 密码登录（Phase 1）：成功发会话 cookie（7 天），失败统一 401（防枚举，不写库）。
async fn api_login_password(State(ctx): State<Arc<Ctx>>, Json(body): Json<Value>) -> Response {
    let name = body.get("name").and_then(Value::as_str).unwrap_or("");
    let password = body.get("password").and_then(Value::as_str).unwrap_or("");
    let user = match with_conn(&ctx, |c| crate::platform::authenticate(c, name, password)) {
        Ok(u) => u,
        Err(_) => {
            return (StatusCode::UNAUTHORIZED, Json(json!({"ok": false, "error": "invalid credentials"})))
                .into_response()
        }
    };
    let uid = user["user_id"].as_str().unwrap_or("").to_string();
    let uname = user["display_name"].as_str().unwrap_or("").to_string();
    let sess = match with_conn(&ctx, |c| crate::platform::issue_session(c, &uid)) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e})))
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
        format!("teamx_platform_token={token}; Path=/; HttpOnly; SameSite=Strict")
            .parse()
            .unwrap(),
    );
    resp
}

/// 登出：吊销会话（若是）+ 清 cookie（幂等）。
async fn api_logout(State(ctx): State<Arc<Ctx>>, headers: HeaderMap) -> Response {
    if let Some(t) = cookie_token(&headers) {
        if t != ctx.launch_token {
            let _ = with_conn(&ctx, |c| crate::platform::revoke_session(c, &t));
        }
    }
    let mut resp = Json(json!({"ok": true})).into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        "teamx_platform_token=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0"
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
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e}))).into_response(),
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
                Json(json!({"ok": false, "error": "not your tenant"})),
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
            Json(json!({"ok": false, "error": "use POST /api/my/teams for self-service"})),
        )
            .into_response();
    }
    let team = body.get("team").and_then(Value::as_str).unwrap_or("").to_string();
    let image = body.get("server_image").and_then(Value::as_str).map(str::to_string);
    if team.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": "team is required"}))).into_response();
    }
    let (kc, cx) = (ctx.kubeconfig.clone(), ctx.context.clone());
    match with_conn(&ctx, |c| crate::provision::create_tenant(c, &team, image.as_deref(), kc.as_deref(), cx.as_deref())) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": e}))).into_response(),
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
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": e}))).into_response(),
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
    service_call(&ctx, &team, "invite", json!({
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
        return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": "member is required"}))).into_response();
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
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": e}))).into_response(),
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
            Json(json!({"ok": false, "error": "user session required"})),
        )
            .into_response();
    };
    match with_conn(&ctx, |c| {
        let owned = crate::platform::owned_teams(c, &uid).map_err(|e| e.to_string())?;
        let all = crate::provision::tenant_list(c)?;
        let mine: Vec<Value> = all
            .into_iter()
            .filter(|t| {
                t.get("team_id")
                    .and_then(Value::as_str)
                    .map(|id| owned.contains(&id.to_string()))
                    .unwrap_or(false)
            })
            .collect();
        let quota = crate::platform::tenant_quota(c, &uid).map_err(|e| e.to_string())?;
        Ok::<_, String>(json!({ "ok": true, "tenants": mine, "used": owned.len(), "quota": quota }))
    }) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e}))).into_response(),
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
            Json(json!({"ok": false, "error": "user session required"})),
        )
            .into_response();
    };
    let name = body.get("name").and_then(Value::as_str).unwrap_or("").to_string();
    let image = body.get("server_image").and_then(Value::as_str).map(str::to_string);
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": "name is required"}))).into_response();
    }
    let (kc, cx) = (ctx.kubeconfig.clone(), ctx.context.clone());
    match with_conn(&ctx, |c| {
        crate::provision::create_my_tenant(c, &uid, &name, image.as_deref(), kc.as_deref(), cx.as_deref())
    }) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": e}))).into_response(),
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
            Json(json!({"ok": false, "error": "user session required"})),
        )
            .into_response();
    };
    // 先归属（403），后参数（400）：越权者无论参数对错都先吃 403。
    if let Err(r) = own_guard(&ctx, &headers, &team) {
        return r;
    }
    let server_url = body.get("server_url").and_then(Value::as_str).unwrap_or("").to_string();
    if server_url.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "server_url is required (tenant endpoint, O-5)"})),
        )
            .into_response();
    }
    let (kc, cx) = (ctx.kubeconfig.clone(), ctx.context.clone());
    match with_conn(&ctx, |c| {
        crate::provision::bootstrap_my_team(c, &team, &uid, &server_url, kc.as_deref(), cx.as_deref())
    }) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": e}))).into_response(),
    }
}

/// 平台服务车道调用：读租户 CA（namespace 的 Secret）→ 用租户 CA 签**平台服务证书** →
/// mTLS 调租户 server 的 `/rpc`（方法白名单在 serve 侧 fail-closed 强制）。
async fn service_call(ctx: &Ctx, team: &str, method: &str, args: Value) -> Response {
    let tenant = match with_conn(ctx, |c| crate::provision::tenant_get(c, team)) {
        Ok(Some(t)) => t,
        Ok(None) => return (StatusCode::NOT_FOUND, Json(json!({"ok": false, "error": format!("tenant {team} not found")}))).into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e}))).into_response(),
    };
    let ns = tenant["namespace"].as_str().unwrap_or("").to_string();
    let ca_secret = format!("teamx-ca-{}", crate::k8s::short_id(team));
    let kubectl = crate::k8s::Kubectl::new(ctx.kubeconfig.clone(), ctx.context.clone());
    // 1) 取租户 CA（平台生成时写入租户 namespace 的 Secret）
    let secret = match kubectl.get_json("secret", &ca_secret, &ns) {
        Ok(Some(v)) => v,
        Ok(None) => return (StatusCode::BAD_GATEWAY, Json(json!({"ok": false, "error": format!("tenant CA secret {ca_secret} not found in {ns}")}))).into_response(),
        Err(e) => return (StatusCode::BAD_GATEWAY, Json(json!({"ok": false, "error": e}))).into_response(),
    };
    let dec = |k: &str| -> Option<String> {
        secret["data"][k]
            .as_str()
            .and_then(base64_decode)
            .map(|b| String::from_utf8_lossy(&b).to_string())
    };
    let (ca_crt, ca_key) = match (dec("ca.crt"), dec("ca.key")) {
        (Some(a), Some(b)) => (a, b),
        _ => return (StatusCode::BAD_GATEWAY, Json(json!({"ok": false, "error": "tenant CA material missing in Secret"}))).into_response(),
    };
    // 2) 用租户 CA 签平台服务证书（service:platform-console:<instance>）
    // instance 只取 token 前缀（CN 由签发函数组装为 service:platform-console:<instance>，调用方不得自带前缀）
    let svc_cn = format!("console-{}", &ctx.launch_token[..8.min(ctx.launch_token.len())]);
    let svc = match crate::pki::issue_service_cert_with_ca(&ca_crt, &ca_key, "platform-console", &svc_cn) {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e}))).into_response(),
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
        Ok(Err(e)) => (StatusCode::BAD_GATEWAY, Json(json!({"ok": false, "error": e}))).into_response(),
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
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e}))).into_response(),
    }
}

async fn api_admins(State(ctx): State<Arc<Ctx>>, headers: HeaderMap) -> Response {
    if let Err(r) = guard(&ctx, &headers) { return r; }
    match with_conn(&ctx, |c| crate::platform::list_admins(c)) {
        Ok(v) => Json(json!({"ok": true, "admins": v})).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e}))).into_response(),
    }
}

async fn api_audit(State(ctx): State<Arc<Ctx>>, headers: HeaderMap) -> Response {
    if let Err(r) = guard(&ctx, &headers) { return r; }
    match with_conn(&ctx, |c| crate::platform::audit_log(c, 100)) {
        Ok(v) => Json(json!({"ok": true, "audit": v})).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e}))).into_response(),
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
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"ok": false, "error": "GitHub OAuth not configured (set TEAMX_GITHUB_CLIENT_ID/SECRET)"}))).into_response();
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
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"ok": false, "error": "GitHub OAuth not configured"}))).into_response();
    }
    let state = q.get("state").cloned().unwrap_or_default();
    if !oauth_consume(&ctx, &state) {
        return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": "bad or expired state (CSRF)"}))).into_response();
    }
    if let Some(e) = q.get("error") {
        return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": format!("github refused: {e}")}))).into_response();
    }
    let code = q.get("code").cloned().unwrap_or_default();
    if code.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": "missing code"}))).into_response();
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
            return (StatusCode::BAD_GATEWAY, Json(json!({"ok": false, "error": e}))).into_response()
        }
    };
    let gid = me.get("id").map(|v| v.to_string()).unwrap_or_default();
    let login = me.get("login").and_then(Value::as_str).unwrap_or("").to_string();
    if gid.is_empty() || gid == "null" || login.is_empty() {
        return (StatusCode::BAD_GATEWAY, Json(json!({"ok": false, "error": "github user missing id/login"}))).into_response();
    }
    // 绑定/新建用户（github_id 锚定；禁用用户拒绝；重名加 -gh 后缀，不自动并户）。
    let user = match with_conn(&ctx, |c| crate::platform::find_or_create_github_user(c, &gid, &login)) {
        Ok(u) => u,
        Err(e) => {
            return (StatusCode::FORBIDDEN, Json(json!({"ok": false, "error": e}))).into_response()
        }
    };
    let uid = user["user_id"].as_str().unwrap_or("").to_string();
    let uname = user["display_name"].as_str().unwrap_or("").to_string();
    let sess = match with_conn(&ctx, |c| crate::platform::issue_session(c, &uid)) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e}))).into_response()
        }
    };
    let token = sess["token"].as_str().unwrap_or("").to_string();
    let _ = with_conn(&ctx, |c| {
        crate::platform::audit(c, &format!("user:{uname}"), "platform_user.github_login", None)
    });
    let mut resp = axum::response::Redirect::to("/").into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        format!("teamx_platform_token={token}; Path=/; HttpOnly; SameSite=Lax")
            .parse()
            .unwrap(),
    );
    resp
}


/// 平台控制台页面（单文件；平台面：租户 / token / 管理员 / 审计）。
const INDEX_HTML: &str = r#"<!doctype html>
<html lang="zh-CN"><head><meta charset="utf-8"><title>Teamx Cloud · 平台控制台</title>
<style>
 :root{--fg:#1f2328;--muted:#6b7280;--bd:#e5e7eb;--bg:#fff;--accent:#7c5cff}
 body{font:14px/1.6 -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;color:var(--fg);background:#fafafa;margin:0}
 header{padding:14px 20px;border-bottom:1px solid var(--bd);background:var(--bg);display:flex;justify-content:space-between;align-items:center}
 h1{font-size:15px;margin:0} h2{font-size:13px;margin:20px 0 8px;color:var(--muted);text-transform:uppercase;letter-spacing:.04em}
 main{padding:20px;max-width:1000px;margin:0 auto}
 table{width:100%;border-collapse:collapse;background:var(--bg);border:1px solid var(--bd);border-radius:8px;overflow:hidden}
 th,td{text-align:left;padding:8px 10px;border-bottom:1px solid var(--bd);font-size:13px}
 th{background:#f6f7f9;font-weight:600;color:var(--muted)}
 code{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:12px}
 .btn{border:1px solid var(--bd);background:var(--bg);border-radius:6px;padding:4px 10px;cursor:pointer;font-size:13px}
 .btn:hover{border-color:var(--accent);color:var(--accent)}
 input{border:1px solid var(--bd);border-radius:6px;padding:6px 8px;font-size:13px;width:260px}
 .muted{color:var(--muted)} .row{display:flex;gap:8px;align-items:center;margin:8px 0}
 .pill{border:1px solid var(--bd);border-radius:999px;padding:0 8px;font-size:12px}
</style></head>
<body>
<header><h1>Teamx Cloud · 平台控制台 <span class="muted">(platform plane)</span></h1><span><span id="who" class="muted"></span> <button class="btn" id="logoutBtn" style="display:none" onclick="logout()">退出</button></span></header>
<main>
  <div id="login">
    <h2>平台登录</h2>
    <div class="row"><input id="uname" placeholder="用户名"><input id="password" type="password" placeholder="密码"><button class="btn" onclick="loginPassword()">登录</button></div>
    <div class="row"><input id="token" placeholder="launch token（启动时打印，运维）"><button class="btn" onclick="login()">运维登录</button></div>
    <div class="muted">GitHub OAuth：<span id="oauthState">检查中…</span> <a class="btn" id="oauthBtn" style="display:none;text-decoration:none" href="/api/oauth/start">GitHub 登录</a></div>
  </div>

  <div id="app" style="display:none">
    <h2>租户（project）</h2>
    <div class="row"><input id="team" placeholder="team id"><button class="btn" onclick="createTenant()">创建租户</button></div>
    <table><thead><tr><th>team_id</th><th>namespace</th><th>CA 指纹</th><th>server 镜像</th><th></th></tr></thead><tbody id="tenants"></tbody></table>

    <h2>我的团队（自助，O-1(a) 全新开始）</h2>
    <div class="muted" id="quota"></div>
    <div class="row"><input id="myName" placeholder="团队名"><button class="btn" onclick="createMyTeam()">创建团队</button></div>
    <table><thead><tr><th>team_id</th><th>namespace</th><th>CA 指纹</th><th></th></tr></thead><tbody id="mine"></tbody></table>
    <div class="row"><input id="bsTeam" placeholder="team id"><input id="bsUrl" placeholder="租户 endpoint（O-5，形如 https://host:5783）" style="width:340px"><button class="btn" onclick="bootstrapTeam()">初始化团队</button></div>
    <div class="muted">初始化后把 owner 邀请函导入本地（`teamx team import`）即成为该团队 owner：</div>
    <pre id="letterOut" class="muted" style="white-space:pre-wrap;word-break:break-all"></pre>

    <h2>租户内成员操作（经 service 车道 §6.5）</h2>
    <div class="row"><input id="invTeam" placeholder="team id"><input id="invRole" placeholder="role: 描述" value="contributor: 成员"><button class="btn" onclick="invite()">邀请成员</button></div>
    <div class="row"><input id="pvTeam" placeholder="team id"><input id="pvMember" placeholder="member id"><button class="btn" onclick="provision()">一键 provision</button></div>
    <pre id="svcOut" class="muted"></pre>

    <h2>平台 token</h2><table><thead><tr><th>id</th><th>state</th><th>created</th><th>redeemed_by</th></tr></thead><tbody id="tokens"></tbody></table>
    <h2>平台管理员</h2><table><thead><tr><th>user</th><th>granted_at</th><th>via</th></tr></thead><tbody id="admins"></tbody></table>
    <h2>平台审计（独立于租户台账）</h2><table><thead><tr><th>at</th><th>actor</th><th>action</th><th>detail</th></tr></thead><tbody id="audit"></tbody></table>
  </div>
</main>
<script>
const q=new URLSearchParams(location.search);
if(q.get('token')){fetch('/api/login',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({token:q.get('token')})}).then(()=>{history.replaceState({},'',location.pathname);boot()})}
else boot();
async function jget(u){const r=await fetch(u,{cache:'no-store'});return await r.json()}
async function jpost(u,b){const r=await fetch(u,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(b||{})});return await r.json()}
async function jdel(u){const r=await fetch(u,{method:'DELETE'});return await r.json()}
async function boot(){
  const me=await jget('/api/me');
  if(!me.ok){document.getElementById('login').style.display='block';document.getElementById('app').style.display='none';document.getElementById('oauthState').textContent=me.github_oauth?'可用':'未配置（TEAMX_GITHUB_CLIENT_ID/SECRET）';document.getElementById('oauthBtn').style.display=me.github_oauth?'inline':'none';return}
  document.getElementById('login').style.display='none';document.getElementById('app').style.display='block';
  document.getElementById('who').textContent=me.who||'';
  document.getElementById('logoutBtn').style.display='inline';
  await loadMine();await loadTenants();await loadPlatform();
}
async function login(){const t=document.getElementById('token').value.trim();const r=await jpost('/api/login',{token:t});if(r.ok)boot();else alert(r.error||'登录失败')}
async function loginPassword(){const name=document.getElementById('uname').value.trim();const password=document.getElementById('password').value;const r=await jpost('/api/login/password',{name,password});if(r.ok)boot();else alert(r.error||'登录失败')}
async function logout(){await jpost('/api/logout',{});document.getElementById('logoutBtn').style.display='none';boot()}
async function loadMine(){try{const d=await jget('/api/my/teams');if(!d.ok)return;document.getElementById('quota').textContent=`配额：${d.used}/${d.quota}`;document.getElementById('mine').innerHTML=(d.tenants||[]).map(t=>`<tr><td><code>${t.team_id}</code></td><td><code>${t.namespace}</code></td><td><code>${(t.ca_fingerprint||'').slice(0,16)}…</code></td><td><button class="btn" onclick="destroyTenant('${t.team_id}')">回收</button></td></tr>`).join('')||'<tr><td colspan=4 class="muted">暂无团队，去上面创建一个</td></tr>'}catch(e){}}
async function createMyTeam(){const name=document.getElementById('myName').value.trim();if(!name)return;const r=await jpost('/api/my/teams',{name});if(!r.ok){alert(r.error);return}document.getElementById('bsTeam').value=r.team_id;await loadMine();await loadTenants();await loadPlatform();alert('团队已创建（空账本）。server 启动后填 endpoint 点"初始化团队"。')}
async function bootstrapTeam(){const team=document.getElementById('bsTeam').value.trim();const server_url=document.getElementById('bsUrl').value.trim();if(!team||!server_url){alert('team id 和 endpoint 都要填');return}const r=await jpost('/api/my/teams/'+encodeURIComponent(team)+'/bootstrap',{server_url});if(!r.ok){alert(r.error);return}const b=r.bootstrap||{};document.getElementById('letterOut').textContent=b.letter||JSON.stringify(r,null,2);await loadMine()}
async function loadTenants(){const d=await jget('/api/tenants');const tb=document.getElementById('tenants');tb.innerHTML=(d.tenants||[]).map(t=>`<tr><td><code>${t.team_id}</code></td><td><code>${t.namespace}</code></td><td><code>${(t.ca_fingerprint||'').slice(0,16)}…</code></td><td>${t.server_image||'—'}</td><td><button class="btn" onclick="destroyTenant('${t.team_id}')">回收</button></td></tr>`).join('')||'<tr><td colspan=5 class="muted">暂无租户</td></tr>'}
async function loadPlatform(){const [t,a,au]=await Promise.all([jget('/api/tokens'),jget('/api/admins'),jget('/api/audit')]);
 document.getElementById('tokens').innerHTML=(t.tokens||[]).map(x=>`<tr><td><code>${x.id.slice(0,8)}</code></td><td><span class="pill">${x.state}</span></td><td class="muted">${x.created_at}</td><td>${x.redeemed_by||'—'}</td></tr>`).join('')||'<tr><td colspan=4 class="muted">无</td></tr>';
 document.getElementById('admins').innerHTML=(a.admins||[]).map(x=>`<tr><td>${x.user_name}</td><td class="muted">${x.granted_at}</td><td class="muted">${x.granted_via}</td></tr>`).join('')||'<tr><td colspan=3 class="muted">无</td></tr>';
 document.getElementById('audit').innerHTML=(au.audit||[]).map(x=>`<tr><td class="muted">${x.at}</td><td><code>${x.actor}</code></td><td>${x.action}</td><td class="muted">${x.detail||''}</td></tr>`).join('')||'<tr><td colspan=4 class="muted">无</td></tr>'}
async function createTenant(){const team=document.getElementById('team').value.trim();if(!team)return;const r=await jpost('/api/tenants',{team});if(!r.ok)alert(r.error);await loadTenants();await loadPlatform()}
async function destroyTenant(t){if(!confirm('回收租户 '+t+' ?（删 namespace，级联清掉 server/CA/PVC）'))return;const r=await jdel('/api/tenants/'+encodeURIComponent(t));if(!r.ok)alert(r.error);await loadTenants();await loadPlatform()}
async function invite(){const team=document.getElementById('invTeam').value.trim();const role_desc=document.getElementById('invRole').value.trim();const r=await jpost('/api/tenants/'+encodeURIComponent(team)+'/invite',{role_desc});document.getElementById('svcOut').textContent=JSON.stringify(r,null,2)}
async function provision(){const team=document.getElementById('pvTeam').value.trim();const member=document.getElementById('pvMember').value.trim();const r=await jpost('/api/tenants/'+encodeURIComponent(team)+'/provision',{member});document.getElementById('svcOut').textContent=JSON.stringify(r,null,2)}
</script>
</body></html>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_html_has_platform_surfaces() {
        for needle in ["平台控制台", "/api/tenants", "/invite", "/provision", "平台审计", "GitHub OAuth",
            "/api/my/teams", "bootstrapTeam", "createMyTeam", "letterOut", "loginPassword", "logout"] {
            assert!(INDEX_HTML.contains(needle), "missing {needle}");
        }
    }

    fn test_ctx() -> Ctx {
        Ctx {
            db_path: std::path::PathBuf::from(":memory:"),
            launch_token: "launch".to_string(),
            kubeconfig: None,
            context: None,
            oauth_states: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        }
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
}
