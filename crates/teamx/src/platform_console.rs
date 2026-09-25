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

// ---------------------------------------------------------------------------
// 团队发现与加入（design/015）：全部团队 / 详情 / 申请 / 审批
// ---------------------------------------------------------------------------

/// 要求登录用户会话；返回 (user_id, display_name)。
#[allow(clippy::result_large_err)]
fn require_user(ctx: &Ctx, headers: &HeaderMap) -> Result<(String, String), Response> {
    session_user(ctx, headers).ok_or_else(|| {
        (StatusCode::FORBIDDEN, Json(json!({"ok": false, "error": "user session required"})))
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
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": e}))).into_response(),
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
        Err(_) => return (StatusCode::NOT_FOUND, Json(json!({"ok": false, "error": "team not found"}))).into_response(),
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
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": e}))).into_response(),
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
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": e}))).into_response(),
    }
}

/// 「我的申请」（SD-26）。
async fn api_dir_mine(State(ctx): State<Arc<Ctx>>, headers: HeaderMap) -> Response {
    if let Err(r) = guard(&ctx, &headers) { return r; }
    let (uid, _) = match require_user(&ctx, &headers) { Ok(v) => v, Err(r) => return r };
    match with_conn(&ctx, |c| crate::discovery::my_requests(c, &uid)) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": e}))).into_response(),
    }
}

async fn api_dir_withdraw(
    State(ctx): State<Arc<Ctx>>, headers: HeaderMap, Path(id): Path<String>,
) -> Response {
    if let Err(r) = guard(&ctx, &headers) { return r; }
    let (uid, _) = match require_user(&ctx, &headers) { Ok(v) => v, Err(r) => return r };
    match with_conn(&ctx, |c| crate::discovery::withdraw_request(c, &id, &uid)) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": e}))).into_response(),
    }
}

/// owner 待审列表。
async fn api_dir_pending(State(ctx): State<Arc<Ctx>>, headers: HeaderMap) -> Response {
    if let Err(r) = guard(&ctx, &headers) { return r; }
    let (uid, _) = match require_user(&ctx, &headers) { Ok(v) => v, Err(r) => return r };
    match with_conn(&ctx, |c| crate::discovery::pending_for_owner(c, &uid)) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": e}))).into_response(),
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
    let (kc, cx) = (ctx.kubeconfig.clone(), ctx.context.clone());
    match with_conn(&ctx, |c| {
        crate::provision::my_teams(c, &uid, kc.as_deref(), cx.as_deref())
    }) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e}))).into_response(),
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
            Json(json!({"ok": false, "error": "user session required"})),
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
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e}))).into_response(),
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
            Json(json!({"ok": false, "error": "user session required"})),
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
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": e}))).into_response(),
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
            Json(json!({"ok": false, "error": "user session required"})),
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
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": e}))).into_response(),
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
            Json(json!({"ok": false, "error": "user session required"})),
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
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e}))).into_response(),
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
            Json(json!({"ok": false, "error": "user session required"})),
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
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": e}))).into_response(),
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
            Json(json!({"ok": false, "error": "user session required"})),
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
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": e}))).into_response(),
    }
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
        crate::provision::tenant_service_post_via_endpoint(
            c, &team, kc.as_deref(), cx.as_deref(), "team.roster", json!({ "team": team }),
        )
    });
    match out {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": e}))).into_response(),
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
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": e}))).into_response(),
    }
}

/// 离开团队（design/016 SD-11 / ADR-0002）：双侧真释放。
async fn api_my_leave(State(ctx): State<Arc<Ctx>>, headers: HeaderMap, Path(team): Path<String>) -> Response {
    if let Err(r) = guard(&ctx, &headers) { return r; }
    let (uid, _) = match require_user(&ctx, &headers) { Ok(v) => v, Err(r) => return r };
    let (kc, cx) = (ctx.kubeconfig.clone(), ctx.context.clone());
    match with_conn(&ctx, |c| crate::provision::leave_team(c, &team, &uid, kc.as_deref(), cx.as_deref())) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": e}))).into_response(),
    }
}

/// Owner 邀请函取件页（design/016 SD-10）。
async fn api_my_letters(State(ctx): State<Arc<Ctx>>, headers: HeaderMap) -> Response {
    if let Err(r) = guard(&ctx, &headers) { return r; }
    let (uid, _) = match require_user(&ctx, &headers) { Ok(v) => v, Err(r) => return r };
    match with_conn(&ctx, |c| crate::provision::my_letters(c, &uid)) {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": e}))).into_response(),
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
    // server_url 可选：留空时由平台自动建立通道（同机 port-forward），并把可用地址写进 letter
    let server_url = body.get("server_url").and_then(Value::as_str).unwrap_or("").to_string();
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
 .usermenu{position:relative;display:inline-block}
 .usermenu-panel{display:none;position:absolute;right:0;top:130%;background:#fff;border:1px solid #EBEEF5;border-radius:6px;box-shadow:0 6px 18px rgba(0,0,0,.12);min-width:190px;padding:6px 0;z-index:50}
 .usermenu-panel a{display:block;padding:8px 16px;font-size:13px;color:#333;text-decoration:none;cursor:pointer}
 .usermenu-panel a:hover{background:#F2F6FC;color:#4A90E5}
 .usermenu-panel .sep{border-top:1px solid #EBEEF5;margin:4px 0}
 .usermenu-panel .dim{color:#C0C4CC;cursor:default}
 /* master-detail split（与成员端 console 的 discussions/documents 同口径） */
 .split{display:grid;grid-template-columns:minmax(300px,38%) 1fr;gap:16px;align-items:start}
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
</style></head>
<body>
<header><div class="logo"><div class="logo-icon">Tx</div><span class="logo-text">Teamx Cloud</span><span class="logo-sub">平台控制台</span></div>
  <div class="head-right">
    <div class="usermenu" id="usermenu" style="display:none">
      <button class="btn btn-sm" onclick="toggleMenu(event)"><span id="who"></span> ▾</button>
      <div class="usermenu-panel" id="usermenuPanel">
        <a onclick="openMyRequests()">我的申请</a>
        <a id="miPending" onclick="openPending()" style="display:none">待我审批</a>
        <div class="sep"></div>
        <a id="miTcoin" onclick="openTcoin()">TCoin</a>
        <a id="miCredit" onclick="openCredit()">credit</a>
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
    <h2>团队信用 <span class="tag">credit · 由履约行为自动派生</span></h2>
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
const q=new URLSearchParams(location.search);
if(q.get('token')){fetch('/api/login',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({token:q.get('token')})}).then(()=>{history.replaceState({},'',location.pathname);boot()})}
else boot();
async function jget(u){const r=await fetch(u,{cache:'no-store'});return await r.json()}
async function jpost(u,b){const r=await fetch(u,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(b||{})});return await r.json()}
async function jdel(u){const r=await fetch(u,{method:'DELETE'});return await r.json()}
function esc(v){return String(v==null?'':v).replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]))}
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
  if(!r.ok){alert(r.error);return}
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
  if(!r.ok){alert(r.error);return}
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
  if(!r.ok){alert(r.error);return}
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
  showTab('all');reqMode('mine');await loadDir(1);
  await loadMine();await loadTenants();await loadPlatform();
}
document.getElementById('pwForm').addEventListener('submit',e=>{e.preventDefault();loginPassword()});
async function loginPassword(){const name=document.getElementById('uname').value.trim();const password=document.getElementById('password').value;const r=await jpost('/api/login/password',{name,password});if(r.ok)boot();else loginFail(r.error)}
async function logout(){
  const r=await jpost('/api/logout',{});
  if(!r.ok){alert('退出失败：'+(r.error||''));return}
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
      <div class="team-head"><h3>${esc(t.team_name||id)}</h3><span class="pill">${vis==='public'?'公开':'私有'}</span>${jp==='open_request'?'<span class="tag-j">允许申请</span>':'<span class="tag-i">仅邀请</span>'}</div>
      <div class="pc-meta"><span>team <code>${esc(id)}</code></span><span>ns <code>${esc(t.namespace||'')}</code></span><span>CA <code>${esc((t.ca_fingerprint||'').slice(0,16))}…</code></span></div>
      <div class="pc-sum">${esc(sum||'（未填写公开简介）')}</div>
      <h2 style="margin-top:12px">初始化（若尚未）</h2>
      <div class="boot-row"><input class="input wide" id="bsUrl_${short}" placeholder="租户 endpoint（留空 = 平台自动连接）"><button class="btn btn-sm" onclick="bootstrapTeam('${esc(id)}')">初始化团队</button></div>
      <div class="muted">留空即可：平台会自行建立到该租户 server 的通道（同机 port-forward），并把可用地址写进 owner 邀请函。</div>
      <h2 style="margin-top:12px">Owner 邀请函 <span class="tag">认领你在这个团队的 owner 座位</span></h2>
      <div id="ownerLetter_${short}"></div>
      <h2 style="margin-top:12px">credit <span class="tag">团队信用（派生）</span></h2>
      <div id="crTeam_${short}" class="muted">加载中…</div>
      <h2 style="margin-top:12px">TCoin <span class="tag">本团队累计消耗</span></h2>
      <div id="tcTeam_${short}" class="muted">加载中…</div>
      <h2 style="margin-top:12px">成员 <span class="tag">只读</span></h2>
      <div id="roster_${short}" class="muted">加载中…</div>
      <h2 style="margin-top:12px">邀请成员</h2>
      <div class="boot-row"><input class="input" id="invRole_${short}" placeholder="role: 描述" value="contributor: 成员"><button class="btn btn-sm" onclick="inviteMember('${esc(id)}')">生成邀请函</button></div>
      <pre class="out" id="invOut_${short}" style="display:none"></pre>
      <h2 style="margin-top:12px">发现设置</h2>
      <div class="boot-row"><select class="input" id="vis_${short}"><option value="private"${vis==='private'?' selected':''}>私有</option><option value="public"${vis==='public'?' selected':''}>公开</option></select>
        <select class="input" id="jp_${short}"><option value="invite_only"${jp==='invite_only'?' selected':''}>仅邀请</option><option value="open_request"${jp==='open_request'?' selected':''}>允许申请加入</option></select>
        <input class="input wide" id="sum_${short}" placeholder="公开简介" value="${esc(sum)}"><button class="btn btn-sm" onclick="saveDiscover('${esc(id)}')">保存</button></div>
      <div class="boot-row" style="border-top:1px solid #EBEEF5;margin-top:14px;padding-top:12px"><button class="btn btn-sm btn-danger" onclick="destroyTenant('${esc(id)}','${esc(t.team_name||'')}')">回收租户</button></div>`;
    loadOwnerLetter(id);
    loadRoster(id);
    loadTeamTcoin(id);
    loadTeamCredit(id);
  }else{
    box.innerHTML=`
      <div class="team-head"><h3>${esc(t.team_name||t.team_id)}</h3><span class="tag-i">我加入的</span>${ws==='Running'?'<span class="tag-j">工作区运行中</span>':'<span class="tag-i">工作区 '+esc(ws)+'</span>'}</div>
      <div class="pc-meta"><span>加入于 ${esc((t.joined_at||'').slice(0,10)||'—')}</span><span>席位 <code>${esc((t.member_id||'').slice(0,12))}</code></span>${t.workspace_live?'':'<span class="tag-i">状态为快照</span>'}</div>
      <div class="pc-sum">${esc(det.public_summary||'')}</div>
      <h2 style="margin-top:12px">credit <span class="tag">团队信用（派生）</span></h2>
      <div id="crTeam_${short}" class="muted">加载中…</div>
      <h2 style="margin-top:12px">我的邀请函</h2>
      ${t.letter_available?`<button class="btn btn-sm" onclick="copyText(${JSON.stringify(t.letter||'')})">复制邀请函</button>`:'<div class="muted">邀请函不可用（可能未生成）</div>'}
      <div class="boot-row" style="border-top:1px solid #EBEEF5;margin-top:14px;padding-top:12px">
        <button class="btn btn-sm btn-danger" onclick="leaveTeam('${esc(id)}','${esc(t.team_name||'')}')">离开团队</button>
        <span class="muted">离开会释放座位并回收工作区（PVC 保留 7 天），之后可再次申请</span></div>`;
  }
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
  if(!r.ok){alert('刷新失败：'+(r.error||''));return}
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
  if(!r.ok){alert('结算失败：'+(r.error||''));return}
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
async function loadRoster(id){
  const short=id.slice(0,8); const el=document.getElementById('roster_'+short);
  const d=await jget('/api/my/teams/'+encodeURIComponent(id)+'/roster');
  if(!d.ok){el.innerHTML='<span class="muted">不可用：'+esc(d.error||'')+'</span>';return}
  const ms=d.members||[];
  el.innerHTML=ms.length?('<table><thead><tr><th>成员</th><th>角色</th><th>状态</th></tr></thead><tbody>'+
    ms.map(m=>`<tr><td>${esc(m.display_name)}${m.is_lead?' <span class="tag-j">lead</span>':''}</td><td>${esc(m.role)}</td><td>${esc(m.state)}</td></tr>`).join('')+'</tbody></table>'):'<div class="muted">暂无成员</div>';
}
async function inviteMember(id){
  const short=id.slice(0,8);
  const role_desc=document.getElementById('invRole_'+short).value.trim()||'contributor: 成员';
  const r=await jpost('/api/my/teams/'+encodeURIComponent(id)+'/invite',{role_desc});
  const out=document.getElementById('invOut_'+short); out.style.display='block';
  if(!r.ok){out.textContent='失败：'+(r.error||'');return}
  out.textContent=r.letter||JSON.stringify(r,null,2);
  alert('邀请函已生成，可复制发给成员');
}
async function leaveTeam(id,name){
  if(!confirm('离开团队「'+name+'」？\n\n会释放我的座位、回收我的工作区（PVC 保留 7 天），之后可再次申请。'))return;
  const r=await jpost('/api/my/teams/'+encodeURIComponent(id)+'/leave',{});
  if(!r.ok){alert('离开失败：'+(r.error||''));return}
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
  if(!r.ok){alert(r.error);return}
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
  if(!r.ok){alert(r.error);return}
  alert('已保存：'+(visibility==='public'?'公开':'私有')+' / '+(join_policy==='open_request'?'允许申请':'仅邀请'));
  loadDir(1);
}
async function bootstrapTeam(team){
  team=team||'';const short=team.slice(0,8);
  const urlEl=document.getElementById('bsUrl_'+short);
  const server_url=(urlEl?urlEl.value:'').trim();
  if(!team){alert('缺少 team id');return}
  const r=await jpost('/api/my/teams/'+encodeURIComponent(team)+'/bootstrap',{server_url});
  if(!r.ok){alert('初始化失败：'+(r.error||''));return}
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
  if(!r.ok){alert(r.error);return}
  loadMine();loadTenants();loadPlatform();
}
</script>
</body></html>
"#;

#[cfg(test)]
mod tests {
    use super::*;

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
