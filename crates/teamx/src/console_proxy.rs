//! console_proxy.rs — 平台控制台反向代理成员 console（`/c/{member16}`）。
//!
//! 把浏览器对 `https://<platform>/c/<member16>/...` 的请求（**含 WebSocket upgrade**）
//! 反代到该成员的**云 console**：集群内 ClusterIP Service（`teamx-m-<short16>-console`，
//! 端口 9528），平台侧经 `kubectl port-forward` 拨号（见 `ensure_forward`）。
//!
//! 三条硬约束（design/021 §J）：
//! - **鉴权 = 平台会话**：路由层先过平台会话；未登录 303 跳平台登录页。
//! - **console token 绝不进浏览器**：本模块只在**上游请求**里注入
//!   `Cookie: teamx_console_token=<tok>`，并把上游 `Set-Cookie` 丢弃。token 由
//!   `k8s::console_token_for(member_id)` 确定性派生，与 Pod 内 console 的一致。
//! - **前缀隔离**：console 内嵌 JS 用 `<base href>` 计算 `BASE`（见 `console_html.rs`），
//!   因此这里只需在 HTML 响应里注入 `<base href="/c/<member16>/">`，其余照搬转发。
//!
//! **自签证书**：成员 console 用工作区自签证书，平台侧无法用公开根校验 —— 上游 TLS
//! **显式跳过证书校验**（`tls::client_config`）。风险与边界见模块文档末与报告。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use axum::body::{to_bytes, Body, Bytes};
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::http::{header, HeaderMap, Method, Request, Response, StatusCode};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

/// 成员 console 的认证 cookie 名（与 `console.rs` 内 `COOKIE` 一致）。
pub const CONSOLE_TOKEN_COOKIE: &str = "teamx_console_token";

/// 上游 console 的目标地址（loopback 或 port-forward 出来的本地端口）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub host: String,
    pub port: u16,
    pub tls: bool,
}

impl Target {
    fn host_port(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// 解析 `http(s)://host[:port]`，仅允许 loopback（反代目标是本地转发口）。
pub fn parse_target(url: &str) -> Result<Target, String> {
    let trimmed = url.trim();
    let (tls, rest) = if let Some(r) = trimmed.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = trimmed.strip_prefix("http://") {
        (false, r)
    } else {
        return Err("console target must start with http:// or https://".to_string());
    };
    let hostport = rest.split('/').next().unwrap_or("");
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().map_err(|_| format!("bad port in {url}"))?),
        None => (hostport.to_string(), if tls { 443 } else { 80 }),
    };
    if host != "127.0.0.1" && !host.eq_ignore_ascii_case("localhost") && host != "::1" {
        return Err(format!("console target must be loopback, got `{host}`"));
    }
    Ok(Target { host, port, tls })
}

/// 席位 id → `member16`（= 去分隔符后的字母数字前 16 位，与 `k8s::short_id` 同口径）。
pub fn member16(member_id: &str) -> String {
    crate::k8s::short_id(member_id)
}

/// `/c/<member16>` 前缀。
pub fn console_prefix(member16: &str) -> String {
    format!("/c/{member16}")
}

/// 从请求路径剥离 `/c/<member16>` 前缀，返回上游路径（无前导斜杠；根为空串）。
pub fn strip_prefix(member16: &str, path: &str) -> String {
    let marker = console_prefix(member16);
    match path.strip_prefix(&marker) {
        Some(t) => t.trim_start_matches('/').to_string(),
        None => String::new(),
    }
}

/// 上游请求路径（含 query）。
pub fn upstream_path(rest: &str, query: Option<&str>) -> String {
    let p = if rest.is_empty() { "/".to_string() } else { format!("/{rest}") };
    match query {
        Some(q) if !q.is_empty() => format!("{p}?{q}"),
        _ => p,
    }
}

/// 在 HTML 的 `<head>` 后注入 `<base href="/c/<member16>/">`。
///
/// 这是“方案 (a)：让 console 支持前缀”的注入点：console 页面/打印页的 JS 读取该 base
/// 后，所有 `fetch`/`EventSource`/`window.open` 都会带上 `/c/<member16>` 前缀。
/// 找不到 `<head>` 时兜底前置（页面仍可用，只是 base 可能不生效）。
pub fn inject_base_href(html: &str, member16: &str) -> String {
    let tag = format!("<base href=\"/c/{member16}/\">");
    let lower = html.to_ascii_lowercase();
    if let Some(i) = lower.find("<head") {
        if let Some(gt) = html[i..].find('>') {
            let at = i + gt + 1;
            let mut out = String::with_capacity(html.len() + tag.len());
            out.push_str(&html[..at]);
            out.push_str(&tag);
            out.push_str(&html[at..]);
            return out;
        }
    }
    format!("{tag}{html}")
}

/// 把上游根相对 `Location` 重写到 `/c/<member16>` 前缀内，避免浏览器跳出前缀。
pub fn rewrite_location(loc: &str, member16: &str) -> String {
    let prefix = console_prefix(member16);
    if loc.starts_with('/') && !loc.starts_with("//") && !loc.starts_with(&prefix) {
        format!("{prefix}{loc}")
    } else {
        loc.to_string()
    }
}

// ---------------------------------------------------------------------------
// 上游拨号（port-forward / 自签 TLS）
// ---------------------------------------------------------------------------

/// 进程级 console port-forward 注册表：member_id → (child, port)。
static CONSOLE_FORWARDS: OnceLock<Mutex<HashMap<String, (std::process::Child, u16)>>> = OnceLock::new();

/// 确保到成员 console ClusterIP Service 的本地通道可用，返回本地端口。
///
/// 与既有 `provision::ensure_forward`（租户 server）同策略：TCP 可连即复用；否则重开。
/// 失败时**明确报错**（不静默），调用方据此回 502。
pub fn ensure_forward(
    team_id: &str,
    member_id: &str,
    kubeconfig: Option<&str>,
    context: Option<&str>,
) -> Result<u16, String> {
    let reg = CONSOLE_FORWARDS.get_or_init(|| Mutex::new(HashMap::new()));
    {
        let mut g = reg.lock().map_err(|_| "console forward registry poisoned".to_string())?;
        if let Some((child, port)) = g.get_mut(member_id) {
            if tcp_open(*port) {
                return Ok(*port);
            }
            let _ = child.kill();
            g.remove(member_id);
        }
    }
    let ns = crate::k8s::namespace_for(team_id);
    let svc = crate::k8s::console_svc_name(member_id);
    let port = crate::k8s::Kubectl::free_port()?;
    let kubectl = crate::k8s::Kubectl::new(kubeconfig.map(str::to_string), context.map(str::to_string));
    let mut child = kubectl.port_forward(&ns, &svc, port, 9528)?;
    let mut ok = false;
    for _ in 0..40 {
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
        if tcp_open(port) {
            ok = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    if !ok {
        let _ = child.kill();
        return Err(format!(
            "cannot reach member console (port-forward to {ns}/svc/{svc} not ready; is the member Pod running and kubeconfig valid?)"
        ));
    }
    reg.lock()
        .map_err(|_| "console forward registry poisoned".to_string())?
        .insert(member_id.to_string(), (child, port));
    Ok(port)
}

fn tcp_open(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        std::time::Duration::from_millis(400),
    )
    .is_ok()
}

pub mod tls {
    //! 跳过证书校验的 rustls 客户端（成员 console 自签）。
    //!
    //! **为什么安全可接受**：拨号目标是 `kubectl port-forward` 出来的
    //! `127.0.0.1:<随机端口>` 或显式的 loopback 测试目标；流量不出本机，
    //! MITM 面极小。这不是“信任任意公网证书”，边界详见报告。

    use std::sync::Arc;

    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{DigitallySignedStruct, SignatureScheme};

    #[derive(Debug)]
    struct AcceptAnyCert;

    impl ServerCertVerifier for AcceptAnyCert {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(
                message,
                cert,
                dss,
                &rustls::crypto::ring::default_provider().signature_verification_algorithms,
            )
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(
                message,
                cert,
                dss,
                &rustls::crypto::ring::default_provider().signature_verification_algorithms,
            )
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            rustls::crypto::ring::default_provider()
                .signature_verification_algorithms
                .supported_schemes()
        }
    }

    pub fn client_config() -> Arc<rustls::ClientConfig> {
        Arc::new(
            rustls::ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(AcceptAnyCert))
                .with_no_client_auth(),
        )
    }
}

async fn plain_connect(t: &Target) -> Result<tokio::net::TcpStream, String> {
    tokio::net::TcpStream::connect((t.host.as_str(), t.port))
        .await
        .map_err(|e| format!("connect {}: {e}", t.host_port()))
}

async fn tls_connect(t: &Target) -> Result<tokio_rustls::client::TlsStream<tokio::net::TcpStream>, String> {
    use tokio_rustls::TlsConnector;
    let tcp = plain_connect(t).await?;
    let name = rustls::pki_types::ServerName::try_from(t.host.clone())
        .map_err(|e| format!("invalid server name {}: {e}", t.host))?;
    let connector = TlsConnector::from(tls::client_config());
    connector
        .connect(name, tcp)
        .await
        .map_err(|e| format!("tls handshake {}: {e}", t.host_port()))
}

// ---------------------------------------------------------------------------
// HTTP 反代
// ---------------------------------------------------------------------------

/// 反代一个普通 HTTP 请求到成员 console。
///
/// `path` 已是上游路径（含 query，形如 `/api/me`）；`body` 已读出（有上限）。
/// 对 HTML 响应注入 `<base>`；`Set-Cookie` 一律丢弃；`Location` 重写到前缀内。
pub async fn forward_http(
    target: &Target,
    method: Method,
    path: String,
    content_type: Option<String>,
    body: Bytes,
    token: &str,
    member16: &str,
) -> Response<Body> {
    if target.tls {
        match tls_connect(target).await {
            Ok(io) => forward_http_io(io, target, method, path, content_type, body, token, member16).await,
            Err(e) => (StatusCode::BAD_GATEWAY, format!("member console unreachable: {e}")).into_response(),
        }
    } else {
        match plain_connect(target).await {
            Ok(io) => forward_http_io(io, target, method, path, content_type, body, token, member16).await,
            Err(e) => (StatusCode::BAD_GATEWAY, format!("member console unreachable: {e}")).into_response(),
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn forward_http_io<S>(
    io: S,
    target: &Target,
    method: Method,
    path: String,
    content_type: Option<String>,
    body: Bytes,
    token: &str,
    member16: &str,
) -> Response<Body>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let mut reader = BufReader::new(io);
    // HTTP/1.0：上游不会用 chunked，响应体靠 Content-Length 或连接关闭界定，
    // 因此 SSE 等无 Content-Length 的流可以“读原始字节直到 EOF”地透传。
    let mut head = format!(
        "{} {} HTTP/1.0\r\nHost: {}\r\nCookie: {}={}\r\n",
        method,
        path,
        target.host,
        CONSOLE_TOKEN_COOKIE,
        token
    );
    if !body.is_empty() {
        let ct = content_type.unwrap_or_else(|| "application/json".to_string());
        head.push_str(&format!("Content-Type: {ct}\r\nContent-Length: {}\r\n", body.len()));
    }
    head.push_str("Connection: close\r\n\r\n");
    if reader.get_mut().write_all(head.as_bytes()).await.is_err() {
        return (StatusCode::BAD_GATEWAY, "member console write failed").into_response();
    }
    if !body.is_empty() && reader.get_mut().write_all(&body).await.is_err() {
        return (StatusCode::BAD_GATEWAY, "member console body write failed").into_response();
    }
    let (status, headers) = match read_head(&mut reader).await {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_GATEWAY, format!("member console bad response: {e}")).into_response(),
    };
    let head_request = method == Method::HEAD;
    let content_len: Option<usize> = headers.iter().find_map(|(k, v)| {
        if k.eq_ignore_ascii_case("content-length") {
            v.trim().parse().ok()
        } else {
            None
        }
    });
    match content_len {
        Some(n) if n <= 32 * 1024 * 1024 => {
            let mut buf = vec![0u8; n];
            let mut read = 0usize;
            while read < n {
                match reader.read(&mut buf[read..]).await {
                    Ok(0) | Err(_) => break,
                    Ok(got) => read += got,
                }
            }
            buf.truncate(read);
            build_response(status, &headers, Bytes::from(buf), member16, head_request)
        }
        // 无 Content-Length（SSE / 流式）→ 原样流式透传（axum 重新分块）。
        None => build_streaming(status, &headers, reader, member16, head_request),
        // Content-Length 过大：不缓冲整段，按流式透传（该响应通常不会走 HTML 注入）。
        Some(_) => build_streaming(status, &headers, reader, member16, head_request),
    }
}

/// 读 HTTP 响应头（状态行 + 头字段直到空行）。
async fn read_head<S: tokio::io::AsyncRead + Unpin>(
    reader: &mut BufReader<S>,
) -> Result<(u16, Vec<(String, String)>), String> {
    use tokio::time::{timeout, Duration};
    let mut line = String::new();
    if timeout(Duration::from_secs(10), reader.read_line(&mut line)).await.is_err() {
        return Err("timeout reading status line".to_string());
    }
    if line.is_empty() {
        return Err("empty response".to_string());
    }
    let status: u16 = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("bad status line: {line:?}"))?;
    let mut headers = Vec::new();
    loop {
        let mut l = String::new();
        match timeout(Duration::from_secs(10), reader.read_line(&mut l)).await {
            Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
            Ok(Ok(_)) => {}
        }
        if l == "\r\n" || l == "\n" || l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
        if headers.len() > 200 {
            break;
        }
    }
    Ok((status, headers))
}

/// 跳过 hop-by-hop / 认证泄露头：判断某响应头是否必须丢弃。
fn strip_header(k: &str) -> bool {
    matches!(
        k.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "transfer-encoding"
            | "content-length"
            | "upgrade"
            | "set-cookie"
            | "x-frame-options"
            | "content-security-policy"
            | "frame-ancestors"
    )
}

fn apply_headers(
    mut builder: axum::http::response::Builder,
    headers: &[(String, String)],
    member16: &str,
) -> (axum::http::response::Builder, bool) {
    let mut is_html = false;
    for (k, v) in headers {
        if strip_header(k) {
            continue;
        }
        if k.eq_ignore_ascii_case("location") {
            builder = builder.header("location", rewrite_location(v, member16));
            continue;
        }
        if k.eq_ignore_ascii_case("content-type") && v.to_ascii_lowercase().contains("text/html") {
            is_html = true;
        }
        if let (Ok(name), Ok(val)) = (
            header::HeaderName::from_bytes(k.as_bytes()),
            header::HeaderValue::from_str(v),
        ) {
            builder = builder.header(name, val);
        }
    }
    (builder, is_html)
}

fn no_body_status(status: u16) -> bool {
    matches!(status, 204 | 304)
}

fn build_response(
    status: u16,
    headers: &[(String, String)],
    body: Bytes,
    member16: &str,
    head_request: bool,
) -> Response<Body> {
    let (builder, is_html) = apply_headers(Response::builder().status(status), headers, member16);
    let body = if head_request || no_body_status(status) {
        Body::empty()
    } else if is_html {
        Body::from(inject_base_href(&String::from_utf8_lossy(&body), member16))
    } else {
        Body::from(body)
    };
    builder.body(body).unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn build_streaming<S>(
    status: u16,
    headers: &[(String, String)],
    reader: BufReader<S>,
    member16: &str,
    head_request: bool,
) -> Response<Body>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (builder, _is_html) = apply_headers(Response::builder().status(status), headers, member16);
    if head_request || no_body_status(status) {
        return builder.body(Body::empty()).unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    let stream = futures_util::stream::unfold(reader, |mut r| async move {
        let mut buf = vec![0u8; 16 * 1024];
        match r.read(&mut buf).await {
            Ok(0) | Err(_) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok::<Bytes, std::io::Error>(Bytes::from(buf)), r))
            }
        }
    });
    builder
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

// ---------------------------------------------------------------------------
// WebSocket 反代（含 https + 自签）
// ---------------------------------------------------------------------------

/// 反代一个 WebSocket 连接到成员 console（自签 TLS 也支持）。
///
/// 用 `tokio-tungstenite::client_async` 在已建好的（可 TLS）流上完成上游握手，
/// 两端各自做 RFC6455 帧编解码，因此不会出现“帧套帧”错误。
pub async fn forward_ws(socket: WebSocket, target: Target, path: String, token: String) {
    if target.tls {
        match tls_connect(&target).await {
            Ok(io) => relay_ws_io(io, &target, path, token, socket).await,
            Err(e) => eprintln!("teamx console proxy: upstream websocket connect failed: {e}"),
        }
    } else {
        match plain_connect(&target).await {
            Ok(io) => relay_ws_io(io, &target, path, token, socket).await,
            Err(e) => eprintln!("teamx console proxy: upstream websocket connect failed: {e}"),
        }
    }
}

async fn relay_ws_io<S>(
    io: S,
    target: &Target,
    path: String,
    token: String,
    socket: WebSocket,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http as thttp;
    use tokio_tungstenite::tungstenite::Message as TMsg;

    let scheme = if target.tls { "wss" } else { "ws" };
    let uri = format!("{scheme}://{}{}", target.host_port(), path);
    // `&str::into_client_request` 会自动补 Host/Connection/Upgrade/Sec-WebSocket-Key/Version。
    let mut req = match uri.as_str().into_client_request() {
        Ok(r) => r,
        Err(_) => return,
    };
    if let Ok(v) = thttp::HeaderValue::from_str(&format!("{}={}", CONSOLE_TOKEN_COOKIE, token)) {
        req.headers_mut().insert("cookie", v);
    }
    let up = match tokio_tungstenite::client_async(req, io).await {
        Ok((ws, _resp)) => ws,
        Err(e) => {
            eprintln!("teamx console proxy: websocket upstream handshake failed: {e}");
            return;
        }
    };

    let (mut up_sink, mut up_stream) = up.split();
    let (mut down_sink, mut down_stream) = socket.split();

    let to_down = tokio::spawn(async move {
        while let Some(Ok(m)) = up_stream.next().await {
            let out = match m {
                TMsg::Text(t) => WsMessage::Text(t.into()),
                TMsg::Binary(b) => WsMessage::Binary(Bytes::from(b)),
                TMsg::Ping(p) => WsMessage::Ping(Bytes::from(p)),
                TMsg::Pong(p) => WsMessage::Pong(Bytes::from(p)),
                TMsg::Close(_) => {
                    let _ = down_sink.send(WsMessage::Close(None)).await;
                    break;
                }
                TMsg::Frame(_) => continue,
            };
            if down_sink.send(out).await.is_err() {
                break;
            }
        }
        let _ = down_sink.close().await;
    });

    while let Some(Ok(m)) = down_stream.next().await {
        let out = match m {
            WsMessage::Text(t) => TMsg::Text(t.as_str().to_string()),
            WsMessage::Binary(b) => TMsg::Binary(b.to_vec()),
            WsMessage::Ping(p) => TMsg::Ping(p.to_vec()),
            WsMessage::Pong(p) => TMsg::Pong(p.to_vec()),
            WsMessage::Close(_) => {
                let _ = up_sink.close().await;
                break;
            }
        };
        if up_sink.send(out).await.is_err() {
            break;
        }
    }
    to_down.abort();
}

// ---------------------------------------------------------------------------
// 平台路由入口（由 platform_console 调用）
// ---------------------------------------------------------------------------

/// 处理一个 `/c/<member16>/...` 的普通 HTTP 请求。
///
/// 鉴权/席位解析/token 派生在 `platform_console` 侧完成后，把目标、上游路径与已读
/// 请求体交给本函数。`member16` 用于注入 `<base>` 与重写 `Location`。
pub async fn serve_http(
    target: Target,
    member16: String,
    method: Method,
    path: String,
    headers: HeaderMap,
    request: Request<Body>,
    token: String,
) -> Response<Body> {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let body = match to_bytes(request.into_body(), 8 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return (StatusCode::BAD_REQUEST, "request body too large").into_response(),
    };
    forward_http(&target, method, path, content_type, body, &token, &member16).await
}

/// 处理一个 `/c/<member16>/...` 的 WebSocket upgrade。
pub async fn serve_ws(target: Target, path: String, token: String, ws: WebSocketUpgrade) -> Response<Body> {
    ws.on_upgrade(move |socket| forward_ws(socket, target, path, token)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member16_matches_short_id() {
        let seat = "474a5a6b-2440-4c1c-b24c-4d2044589b65";
        assert_eq!(member16(seat), "474a5a6b24404c1c");
        assert_eq!(member16(seat), crate::k8s::short_id(seat));
        assert_eq!(member16(""), "unknown000000000");
    }

    #[test]
    fn strip_prefix_handles_root_and_rest() {
        let m = "474a5a6b24404c1c";
        assert_eq!(strip_prefix(m, "/c/474a5a6b24404c1c"), "");
        assert_eq!(strip_prefix(m, "/c/474a5a6b24404c1c/"), "");
        assert_eq!(strip_prefix(m, "/c/474a5a6b24404c1c/api/me"), "api/me");
        assert_eq!(strip_prefix(m, "/c/474a5a6b24404c1c/print/discussion"), "print/discussion");
        // 不是本前缀 → 空（调用方已按路由保证 member16 正确）
        assert_eq!(strip_prefix(m, "/other/api"), "");
        // 不误伤：另一个前缀相似但不是同一 member
        assert_eq!(strip_prefix(m, "/c/474a5a6b24404c1d/api"), "");
    }

    #[test]
    fn upstream_path_keeps_query() {
        assert_eq!(upstream_path("", None), "/");
        assert_eq!(upstream_path("api/me", None), "/api/me");
        assert_eq!(upstream_path("api/me", Some("")), "/api/me");
        assert_eq!(upstream_path("api/file", Some("path=a%2Fb")), "/api/file?path=a%2Fb");
    }

    #[test]
    fn inject_base_before_head_close() {
        let html = "<!doctype html><html><head><meta charset=\"utf-8\"><title>T</title></head><body>x</body></html>";
        let out = inject_base_href(html, "abc123");
        assert!(out.contains("<base href=\"/c/abc123/\">"));
        let base_at = out.find("<base").unwrap();
        let title_at = out.find("<title").unwrap();
        assert!(base_at < title_at, "base 必须在 head 内容之前生效");
        // 无 head 时兜底前置
        let out2 = inject_base_href("<p>hi</p>", "abc123");
        assert!(out2.starts_with("<base href=\"/c/abc123/\">"));
    }

    #[test]
    fn location_rewrite_stays_in_prefix() {
        assert_eq!(rewrite_location("/", "m16"), "/c/m16/");
        assert_eq!(rewrite_location("/login", "m16"), "/c/m16/login");
        assert_eq!(rewrite_location("https://x/y", "m16"), "https://x/y");
        assert_eq!(rewrite_location("//cdn/x", "m16"), "//cdn/x");
        // 已带前缀 → 不重复
        assert_eq!(rewrite_location("/c/m16/api", "m16"), "/c/m16/api");
    }

    #[test]
    fn parse_target_is_loopback_only() {
        let t = parse_target("https://127.0.0.1:4433").unwrap();
        assert_eq!(t, Target { host: "127.0.0.1".into(), port: 4433, tls: true });
        assert!(parse_target("http://localhost:8080").is_ok());
        assert!(parse_target("https://example.com:443").is_err(), "禁止非 loopback 目标");
        assert!(parse_target("ftp://127.0.0.1").is_err());
    }

    #[test]
    fn tls_client_config_builds_without_verification() {
        // 只要构造成功即可（真实自签握手在 platform_console 的 E2E 里验证）。
        let _cfg = tls::client_config();
    }

    #[test]
    fn strip_header_drops_cookie_and_hop_by_hop() {
        assert!(strip_header("Set-Cookie"));
        assert!(strip_header("content-length"));
        assert!(strip_header("Transfer-Encoding"));
        assert!(!strip_header("Content-Type"));
        assert!(!strip_header("Location"));
    }

    /// 前缀方案 (a) 依赖 console 页面读取 `<base href>`：锁定 JS 里有 TXB/TXU 且
    /// 不用会与 highlight.js 全局 `U` 冲突的 `const U`。
    #[test]
    fn console_pages_are_prefix_aware() {
        let page = crate::console_html::page_html();
        assert!(page.contains("const TXU="), "主页面必须支持反代前缀");
        assert!(!page.contains("const U="), "不得再声明全局 const U（与内嵌 hljs 的 function U 冲突）");
        assert!(page.contains("fetch(TXU("), "API 调用必须带前缀");
        assert!(page.contains("new EventSource(TXU("), "SSE 必须带前缀");
        assert!(page.contains("U(\"/api/cloud/") || page.contains("TXU(\"/api/cloud/"), "云代理路径必须带前缀");
        let print = crate::console_html::print_discussion_html();
        assert!(print.contains("const TXU="), "打印页也必须支持反代前缀");
        assert!(print.contains("fetch(TXU("), "打印页 API 调用必须带前缀");
    }
}
