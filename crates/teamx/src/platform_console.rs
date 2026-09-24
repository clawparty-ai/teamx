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
    // 注意：HTTP/2 下浏览器（Firefox）可能把 cookie 拆成多个 `cookie` 字段发送；
    // 只读第一个（headers.get）会丢 token → 已登录用户被踢 401。必须遍历全部。
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|raw| raw.split(';').map(str::trim))
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
        format!("teamx_platform_token={}; Path=/; HttpOnly; Secure; SameSite=Strict", ctx.launch_token)
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
        format!("teamx_platform_token={token}; Path=/; HttpOnly; Secure; SameSite=Strict")
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
    // M2（weiye-mini task-103）：用户面**不接受** server_image（镜像由平台 pin）。
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": "name is required"}))).into_response();
    }
    let (kc, cx) = (ctx.kubeconfig.clone(), ctx.context.clone());
    match with_conn(&ctx, |c| {
        crate::provision::create_my_tenant(c, &uid, &name, kc.as_deref(), cx.as_deref())
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
</style></head>
<body>
<header><div class="logo"><div class="logo-icon">Tx</div><span class="logo-text">Teamx Cloud</span><span class="logo-sub">平台控制台</span></div><div class="head-right"><span id="who"></span><button class="btn btn-sm" id="logoutBtn" style="display:none" onclick="logout()">退出</button></div></header>
<div class="main" id="loginWrap">
  <div class="card" id="login">
    <div class="icon">☁️</div>
    <h1>平台登录</h1>
    <div class="subtitle">Teamx Cloud · 平台控制台</div>
    <div class="form">
      <!-- 用户名/密码用真 <form>：回车即提交（此前是 div，回车无反应） -->
      <form id="pwForm" autocomplete="on">
        <input class="input" id="uname" placeholder="用户名" autocomplete="username">
        <input class="input" id="password" type="password" placeholder="密码" autocomplete="current-password">
        <button class="btn" type="submit">登 录</button>
      </form>
      <div class="error" id="loginErr"></div>
      <div class="oauth-row"><a class="btn btn-ghost" id="oauthBtn" style="display:none;text-decoration:none;text-align:center" href="/api/oauth/start">GitHub 登录</a></div>
      <div class="muted">GitHub OAuth：<span id="oauthState">检查中…</span></div>
    </div>
  </div>
</div>

<div class="app" id="app" style="display:none">
  <nav class="tabs">
    <button class="tab active" data-tab="mine" onclick="showTab('mine')">我的团队</button>
    <button class="tab op-only" data-tab="tenants" onclick="showTab('tenants')">租户管理</button>
    <button class="tab" data-tab="members" onclick="showTab('members')">成员操作</button>
    <button class="tab op-only" data-tab="platform" onclick="showTab('platform')">平台管理</button>
  </nav>
  <div class="tabpage" id="tab-mine">
  <div class="cards" id="mineCards"></div>
  <div class="panel"><h2>创建团队 <span class="tag">O-1(a) 全新开始 · 空账本</span></h2>
    <div class="row"><input class="input" id="myName" placeholder="团队名，比如 data-team"><button class="btn btn-sm" onclick="createMyTeam()">创建团队</button><span class="muted" id="quota"></span></div>
  </div>
  <div id="mine"></div>
  <div class="panel"><h2>Owner 邀请函</h2>
    <div class="muted">点团队卡片上的"初始化团队"后，邀请函显示在这里 —— 导入本地（<code>teamx team import</code>）即成为该团队 owner
      <button class="btn btn-sm copybtn" onclick="copyLetter()">复制邀请函</button></div>
    <pre class="out muted" id="letterOut">（初始化团队后显示）</pre>
  </div>
  </div>
  <div class="tabpage" id="tab-tenants" style="display:none">

  <div class="panel"><h2>租户 <span class="tag">project（运维全量）</span></h2>
    <div class="row"><input class="input" id="team" placeholder="team id"><button class="btn btn-sm" onclick="createTenant()">创建租户</button></div>
    <table><thead><tr><th>team_id</th><th>namespace</th><th>CA 指纹</th><th>server 镜像</th><th></th></tr></thead><tbody id="tenants"></tbody></table>
  </div>

  </div>
  <div class="tabpage" id="tab-members" style="display:none">
  <div class="panel"><h2>租户内成员操作 <span class="tag">经 service 车道 §6.5</span></h2>
    <div class="muted">普通用户填自己名下租户的 team id 即可（归属鉴权）；运维可操作全部。</div>
    <div class="row"><input class="input" id="invTeam" placeholder="team id"><input class="input" id="invRole" placeholder="role: 描述" value="contributor: 成员"><button class="btn btn-sm" onclick="invite()">邀请成员</button></div>
    <div class="row"><input class="input" id="pvTeam" placeholder="team id"><input class="input" id="pvMember" placeholder="member id"><button class="btn btn-sm" onclick="provision()">一键 provision</button></div>
    <pre class="out muted" id="svcOut"></pre>
  </div>

  </div>
  <div class="tabpage" id="tab-platform" style="display:none">
  <div class="panel"><h2>平台 token</h2><table><thead><tr><th>id</th><th>state</th><th>created</th><th>redeemed_by</th></tr></thead><tbody id="tokens"></tbody></table></div>
  <div class="panel"><h2>平台管理员</h2><table><thead><tr><th>user</th><th>granted_at</th><th>via</th></tr></thead><tbody id="admins"></tbody></table></div>
  <div class="panel"><h2>平台审计 <span class="tag">独立于租户台账</span></h2><table><thead><tr><th>at</th><th>actor</th><th>action</th><th>detail</th></tr></thead><tbody id="audit"></tbody></table></div>
  </div>
</div>
<script>
const q=new URLSearchParams(location.search);
if(q.get('token')){fetch('/api/login',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({token:q.get('token')})}).then(()=>{history.replaceState({},'',location.pathname);boot()})}
else boot();
async function jget(u){const r=await fetch(u,{cache:'no-store'});return await r.json()}
async function jpost(u,b){const r=await fetch(u,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(b||{})});return await r.json()}
async function jdel(u){const r=await fetch(u,{method:'DELETE'});return await r.json()}
function esc(v){return String(v==null?'':v).replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]))}
function showTab(name){document.querySelectorAll('.tab').forEach(b=>b.classList.toggle('active',b.dataset.tab===name));document.querySelectorAll('.tabpage').forEach(p=>p.style.display=p.id==='tab-'+name?'block':'none')}
async function copyLetter(){const t=document.getElementById('letterOut').textContent;if(!t){alert('暂无邀请函，先点"初始化团队"');return}try{await navigator.clipboard.writeText(t);alert('已复制')}catch(e){const ta=document.createElement('textarea');ta.value=t;document.body.appendChild(ta);ta.select();document.execCommand('copy');ta.remove();alert('已复制')}}
function loginFail(msg){const e=document.getElementById('loginErr');e.textContent=msg||'登录失败';e.style.display='block'}
async function boot(){
  const me=await jget('/api/me');
  if(!me.ok){document.getElementById('loginWrap').style.display='flex';document.getElementById('app').style.display='none';document.getElementById('oauthState').textContent=me.github_oauth?'可用':'未配置（TEAMX_GITHUB_CLIENT_ID/SECRET）';document.getElementById('oauthBtn').style.display=me.github_oauth?'block':'none';return}
  document.getElementById('loginWrap').style.display='none';document.getElementById('app').style.display='block';
  document.getElementById('who').textContent=me.who||'';
  document.getElementById('logoutBtn').style.display='inline-block';
  const isOp=(me.who||'').indexOf('operator')===0;
  document.querySelectorAll('.tab.op-only').forEach(b=>b.style.display=isOp?'':'none');
  showTab('mine');
  await loadMine();await loadTenants();await loadPlatform();
}
document.getElementById('pwForm').addEventListener('submit',e=>{e.preventDefault();loginPassword()});
async function loginPassword(){const name=document.getElementById('uname').value.trim();const password=document.getElementById('password').value;const r=await jpost('/api/login/password',{name,password});if(r.ok)boot();else loginFail(r.error)}
async function logout(){await jpost('/api/logout',{});document.getElementById('logoutBtn').style.display='none';boot()}
async function loadMine(){try{const d=await jget('/api/my/teams');if(!d.ok){document.getElementById('quota').textContent='';document.getElementById('mine').innerHTML='';document.getElementById('mineCards').innerHTML='';return}
const ts=d.tenants||[];const me=(await jget('/api/me')).who||'';
document.getElementById('mineCards').innerHTML=
`<div class="stat-card"><div class="stat-icon blue">👥</div><div><div class="stat-label">我的团队</div><div class="stat-value">${ts.length}</div></div></div>`+
`<div class="stat-card"><div class="stat-icon orange">📊</div><div><div class="stat-label">租户配额</div><div class="stat-value">${d.used}/${d.quota}</div></div></div>`+
`<div class="stat-card"><div class="stat-icon green">👤</div><div><div class="stat-label">当前身份</div><div class="stat-value" style="font-size:15px">${esc(me)}</div></div></div>`;
document.getElementById('quota').textContent=`配额 ${d.used}/${d.quota}`;
document.getElementById('mine').innerHTML=ts.map(t=>{const short=t.team_id.slice(0,8);return `<div class="team-card"><div class="team-head"><h3>${esc(t.team_name||'未命名团队')}</h3><span class="pill">自助租户</span><span class="team-actions"><button class="btn btn-sm btn-danger" onclick="destroyTenant('${t.team_id}')">回收</button></span></div><div class="team-meta"><span>team <code>${esc(t.team_id)}</code></span><span>ns <code>${esc(t.namespace)}</code></span><span>CA <code>${esc((t.ca_fingerprint||'').slice(0,16))}…</code></span><span>${esc(t.server_image||'')}</span></div><div class="boot-row"><input class="input" id="bsUrl_${short}" placeholder="租户 endpoint（O-5，形如 https://host:5783）"><button class="btn btn-sm" onclick="bootstrapTeam('${t.team_id}')">初始化团队</button></div></div>`}).join('')||'<div class="team-card"><div class="empty"><div class="big">🌱</div>还没有团队，在上面创建一个吧<div class="muted">配额内免费建，server 由平台自动拉起</div></div></div>'}catch(e){}}
async function createMyTeam(){const name=document.getElementById('myName').value.trim();if(!name)return;const r=await jpost('/api/my/teams',{name});if(!r.ok){alert(r.error);return}await loadMine();await loadTenants();await loadPlatform();alert('团队已创建（空账本）。server 启动后填 endpoint 点"初始化团队"。')}
async function bootstrapTeam(team){team=team||'';const short=team.slice(0,8);const urlEl=document.getElementById('bsUrl_'+short);const server_url=(urlEl?urlEl.value:'').trim();if(!team||!server_url){alert('填该团队卡片上的 endpoint');return}const r=await jpost('/api/my/teams/'+encodeURIComponent(team)+'/bootstrap',{server_url});if(!r.ok){alert(r.error);return}const b=r.bootstrap||{};document.getElementById('letterOut').textContent=b.letter||JSON.stringify(r,null,2);await loadMine()}
async function loadTenants(){const d=await jget('/api/tenants');const tb=document.getElementById('tenants');tb.innerHTML=(d.tenants||[]).map(t=>`<tr><td><code>${esc(t.team_id)}</code></td><td><code>${esc(t.namespace)}</code></td><td><code>${esc((t.ca_fingerprint||'').slice(0,16))}…</code></td><td>${esc(t.server_image||'—')}</td><td><button class="btn btn-sm btn-danger" onclick="destroyTenant('${t.team_id}')">回收</button></td></tr>`).join('')||'<tr><td colspan=5 class="muted">暂无租户</td></tr>'}
async function loadPlatform(){const [t,a,au]=await Promise.all([jget('/api/tokens'),jget('/api/admins'),jget('/api/audit')]);
 document.getElementById('tokens').innerHTML=(t.tokens||[]).map(x=>`<tr><td><code>${esc(x.id.slice(0,8))}</code></td><td><span class="pill">${esc(x.state)}</span></td><td class="muted">${esc(x.created_at)}</td><td>${esc(x.redeemed_by||'—')}</td></tr>`).join('')||'<tr><td colspan=4 class="muted">无</td></tr>';
 document.getElementById('admins').innerHTML=(a.admins||[]).map(x=>`<tr><td>${esc(x.user_name)}</td><td class="muted">${esc(x.granted_at)}</td><td class="muted">${esc(x.granted_via)}</td></tr>`).join('')||'<tr><td colspan=3 class="muted">无</td></tr>';
 document.getElementById('audit').innerHTML=(au.audit||[]).map(x=>`<tr><td class="muted">${esc(x.at)}</td><td><code>${esc(x.actor)}</code></td><td>${esc(x.action)}</td><td class="muted">${esc(x.detail||'')}</td></tr>`).join('')||'<tr><td colspan=4 class="muted">无</td></tr>'}
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
            "/api/my/teams", "bootstrapTeam", "createMyTeam", "letterOut", "loginPassword", "logout",
            "pwForm",
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
            oauth_states: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
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
