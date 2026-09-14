//! IM gateway process B — **persistent-connection** runtime
//! (`teamx im-gateway run`).
//!
//! Feishu's long-connection event subscription is an **outbound** WebSocket:
//! the gateway dials Feishu, so a self-hosted / NAT deployment needs no public
//! callback URL, no domain and no certificate. This module owns that socket
//! (via the `lark-channel` SDK), normalizes each inbound event, signs a
//! delegation envelope and forwards the resulting action to `serve` over mTLS
//! (`gateway_client` / `direct_client`).
//!
//! Security (design §5.3, identity B): the gateway is **zero-privilege**. It
//! never sends a `member_id` — only `platform` / `im_user_id` / `team` plus the
//! HMAC delegation envelope. `serve` resolves the actor from `im_bindings` and
//! enforces the frozen method whitelist (fail-closed).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use lark_channel::config::ChannelConfig;
use lark_channel::event::{
    CardActionEvent, ChannelEvent, EventLoop, EventLoopOptions, OpenApiWebSocketEventConnector,
    ReceivedEvent,
};
use lark_channel::lark_openapi::{
    BoxFuture, HttpMethod, HttpRequest, HttpResponse, OpenApiClient, OpenApiTransport,
    TokioTungsteniteWebSocketTransport, WebSocketEventAck,
};
use lark_channel::message::NormalizedMessage;
use lark_channel::{Error as LarkError, Result as LarkResult};
use serde_json::{json, Value};

use crate::direct_client;
use crate::im::config::ImConfig;
use crate::im::providers::feishu::FeishuProvider;
use crate::im::providers::ImProvider;
use crate::im::types::{ImChannel, ImMessage};
use crate::tunnel_client::{self, MtlsMaterial};

/// Inputs for `teamx im-gateway run`.
pub struct RunArgs {
    /// Directory holding `service.crt` + `service.key` + `ca.crt`.
    pub cert_dir: PathBuf,
    /// Server base URL (`https://host:port`).
    pub server: String,
    /// Team scope (the gateway serves exactly one team).
    pub team: String,
    /// Provider platform (default `feishu`).
    pub platform: String,
    /// Feishu app id (falls back to `TEAMX_IM_FEISHU_APP_ID`).
    pub app_id: Option<String>,
    /// Feishu app secret (falls back to `TEAMX_IM_FEISHU_APP_SECRET`).
    pub app_secret: Option<String>,
}

/// Minimal blocking `ureq` transport adapted to `lark-channel`'s async
/// transport trait. Keeps the dependency footprint small (no `reqwest`) — the
/// only HTTP call this transport serves is the WS-endpoint handshake.
#[derive(Clone, Default)]
struct UreqTransport;

impl OpenApiTransport for UreqTransport {
    fn send_json(&self, request: HttpRequest) -> BoxFuture<'static, LarkResult<HttpResponse>> {
        Box::pin(async move {
            tokio::task::spawn_blocking(move || send_json_blocking(request))
                .await
                .map_err(|e| LarkError::Transport(format!("openapi join error: {e}")))?
        })
    }
}

fn send_json_blocking(request: HttpRequest) -> LarkResult<HttpResponse> {
    let HttpRequest {
        method,
        url,
        headers,
        body,
    } = request;
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(10))
        .build();
    let mut req = match method {
        HttpMethod::Get => agent.get(url.as_str()),
        HttpMethod::Post => agent.post(url.as_str()),
        HttpMethod::Put => agent.put(url.as_str()),
        HttpMethod::Patch => agent.patch(url.as_str()),
        HttpMethod::Delete => agent.delete(url.as_str()),
    };
    for (name, value) in &headers {
        req = req.set(name, value);
    }
    let outcome = if body.is_null() {
        req.call()
    } else {
        req.send_json(body)
    };
    match outcome {
        Ok(resp) => {
            let status = resp.status();
            let value = resp
                .into_json::<Value>()
                .map_err(|e| LarkError::Transport(format!("openapi decode: {e}")))?;
            Ok(HttpResponse { status, body: value })
        }
        // Preserve a non-2xx status so `parse_openapi_response` can surface it
        // (a transport-level error would hide the platform's message).
        Err(ureq::Error::Status(status, resp)) => {
            let value = resp.into_json::<Value>().unwrap_or(Value::Null);
            Ok(HttpResponse { status, body: value })
        }
        Err(e) => Err(LarkError::Transport(format!("openapi request failed: {e}"))),
    }
}

/// Everything one inbound event needs to reach `serve` and answer the user.
#[derive(Clone)]
struct GatewayRuntime {
    server: String,
    team: String,
    platform: String,
    /// Provider shared secret used to sign the delegation envelope.
    secret: String,
    tls: Arc<rustls::ClientConfig>,
    provider: Arc<FeishuProvider>,
}

impl GatewayRuntime {
    /// Sign a delegation envelope and call `serve` over mTLS.
    async fn call(
        &self,
        method: &str,
        extra: Value,
        im_user_id: &str,
        im_msg_id: &str,
    ) -> Result<Value, String> {
        let ctx = crate::im::gateway_client::build_delegation(&self.platform, im_user_id, &self.secret);
        let mut args = json!({
            "platform": ctx.platform,
            "im_user_id": ctx.im_user_id,
            "im_team": ctx.im_team,
            "signature": ctx.signature,
            "timestamp": ctx.timestamp,
            "nonce": ctx.nonce,
            "team": self.team,
            "im_msg_id": im_msg_id,
        });
        if let Value::Object(fields) = extra {
            for (k, v) in fields {
                args[k] = v;
            }
        }
        direct_client::rpc_async(&self.server, method, args, &self.tls).await
    }

    /// Best-effort outbound reply (never fails the event handler).
    fn reply(&self, chat_id: &str, body: &str) {
        if chat_id.is_empty() {
            return;
        }
        let msg = ImMessage {
            channel: ImChannel::Group(chat_id.to_string()),
            title: Some("teamx".to_string()),
            body: body.to_string(),
            markdown: false,
        };
        if let Err(e) = self.provider.send(&msg) {
            eprintln!("[im-gateway] reply to {chat_id} failed: {e}");
        }
    }
}

/// Run the persistent-connection gateway until the process is stopped.
pub fn run(args: RunArgs) -> Result<Value, String> {
    let mtls = load_mtls(&args.cert_dir)?;
    let verifier = direct_client::CaChainVerifier::with_server_eku(&mtls)?;
    let tls = direct_client::client_config_with_verifier(&mtls, verifier)?;

    let env = ImConfig::from_env();
    let app_id = args
        .app_id
        .clone()
        .or_else(|| env.feishu_app_id.clone())
        .ok_or("im-gateway run: app_id missing (pass --app-id or set TEAMX_IM_FEISHU_APP_ID)")?;
    let app_secret = args
        .app_secret
        .clone()
        .or_else(|| env.feishu_app_secret.clone())
        .ok_or("im-gateway run: app_secret missing (pass --app-secret or set TEAMX_IM_FEISHU_APP_SECRET)")?;

    // The provider is only used for outbound replies; live (never dry-run here —
    // the gateway exists to talk to Feishu).
    let provider = Arc::new(FeishuProvider::new(ImConfig {
        feishu_app_id: Some(app_id.clone()),
        feishu_app_secret: Some(app_secret.clone()),
        dry_run: env.dry_run,
        notify_groups: Vec::new(),
    }));

    let runtime = GatewayRuntime {
        server: args.server.clone(),
        team: args.team.clone(),
        platform: args.platform.clone(),
        secret: app_secret.clone(),
        tls,
        provider,
    };

    let channel = ChannelConfig::new(app_id, app_secret);
    let openapi = OpenApiClient::new(channel, UreqTransport);
    let connector =
        OpenApiWebSocketEventConnector::new(openapi, TokioTungsteniteWebSocketTransport::new());

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("im-gateway runtime: {e}"))?;

    rt.block_on(async move {
        let options = EventLoopOptions::new()
            .with_unlimited_reconnects()
            .with_reconnect_delay(Duration::from_secs(3));
        let mut event_loop = EventLoop::with_options(connector, options);

        eprintln!(
            "[im-gateway] connecting to feishu long-connection (team={}, server={})",
            runtime.team, runtime.server
        );
        let exit = event_loop
            .run(move |received: ReceivedEvent| {
                let runtime = runtime.clone();
                async move {
                    if let Err(e) = handle_event(&runtime, received.event).await {
                        // Business failures must not tear down the socket: log
                        // and ack OK so the loop keeps receiving.
                        eprintln!("[im-gateway] event handling failed: {e}");
                    }
                    Ok(WebSocketEventAck::ok())
                }
            })
            .await
            .map_err(|e| format!("im-gateway event loop: {e}"))?;
        Ok(json!({ "ok": true, "exit": format!("{exit:?}") }))
    })
}

async fn handle_event(rt: &GatewayRuntime, event: ChannelEvent) -> Result<(), String> {
    match event {
        ChannelEvent::Message(msg) => handle_message(rt, *msg).await,
        ChannelEvent::CardAction(action) => handle_card_action(rt, *action).await,
        // Unknown event types are subscribed-but-unhandled: ignore quietly.
        ChannelEvent::Unknown { .. } => Ok(()),
    }
}

async fn handle_message(rt: &GatewayRuntime, msg: NormalizedMessage) -> Result<(), String> {
    let text = msg.text.trim().to_string();
    if text.is_empty() {
        return Ok(());
    }
    let reply = match parse_command(&text) {
        Some((method, extra)) => {
            match rt.call(method, extra, &msg.sender_id, &msg.message_id).await {
                Ok(v) => render_reply(&v),
                Err(e) => format!("❌ {e}"),
            }
        }
        None => HELP_TEXT.to_string(),
    };
    rt.reply(&msg.chat_id, &reply);
    Ok(())
}

async fn handle_card_action(rt: &GatewayRuntime, action: CardActionEvent) -> Result<(), String> {
    let operator = action.operator.open_id.clone().unwrap_or_default();
    if operator.is_empty() {
        return Err("card action without an operator open_id".to_string());
    }
    let value = &action.action.value;
    let card_action = value.get("action").and_then(Value::as_str).unwrap_or("");
    let task_id = value.get("task_id").and_then(Value::as_str).unwrap_or("");
    if card_action.is_empty() || task_id.is_empty() {
        return Err("card action payload missing `action`/`task_id`".to_string());
    }
    // A button carrying a `confirm_token` is the SECOND click (execute); any
    // other button is the FIRST click (request a token).
    let (method, extra) = match value.get("confirm_token").and_then(Value::as_str) {
        Some(token) => (
            "im.card.execute",
            json!({ "action": card_action, "task_id": task_id, "confirm_token": token }),
        ),
        None => (
            "im.card.request",
            json!({ "action": card_action, "task_id": task_id }),
        ),
    };
    let event_id = action.context.event_id.clone();
    let reply = match rt.call(method, extra, &operator, &event_id).await {
        Ok(v) => render_reply(&v),
        Err(e) => format!("❌ {e}"),
    };
    // Card interactions happen inside a chat; answer there when we know it.
    let chat_id = action
        .card_context
        .as_ref()
        .and_then(|c| c.open_chat_id.clone())
        .unwrap_or_default();
    rt.reply(&chat_id, &reply);
    Ok(())
}

/// The frozen Phase-2 command surface (keyword → delegated RPC).
fn parse_command(text: &str) -> Option<(&'static str, Value)> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let clean = |tok: &str| -> String {
        tok.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
            .to_string()
    };
    // `绑定 <code>` / `bind <code>` — the one-time code is the authorization.
    for (i, tok) in tokens.iter().enumerate() {
        let t = clean(tok);
        if t == "绑定" || t.eq_ignore_ascii_case("bind") {
            if let Some(code) = tokens.get(i + 1) {
                return Some(("im.bind", json!({ "code": clean(code) })));
            }
        }
    }
    // Read-only queries.
    for tok in &tokens {
        let t = clean(tok);
        let kind = if t == "状态" || t.eq_ignore_ascii_case("status") {
            "status"
        } else if t == "任务" || t.eq_ignore_ascii_case("tasks") || t.eq_ignore_ascii_case("task") {
            "tasks"
        } else if t == "目标" || t.eq_ignore_ascii_case("goal") {
            "goal"
        } else if t == "讨论" || t.eq_ignore_ascii_case("discussions") || t.eq_ignore_ascii_case("discussion") {
            "discussions"
        } else if t == "问题" || t.eq_ignore_ascii_case("questions") || t.eq_ignore_ascii_case("question") {
            "questions"
        } else {
            continue;
        };
        return Some(("im.query", json!({ "kind": kind })));
    }
    None
}

/// Render an RPC envelope as a short chat reply.
fn render_reply(value: &Value) -> String {
    if value.get("ok").and_then(Value::as_bool) == Some(false) {
        if let Some(err) = value.get("error").and_then(Value::as_str) {
            return format!("❌ {err}");
        }
    }
    let data = value.get("data").unwrap_or(value);
    let rendered = serde_json::to_string_pretty(data).unwrap_or_else(|_| data.to_string());
    if rendered.chars().count() > 1500 {
        let truncated: String = rendered.chars().take(1500).collect();
        format!("{truncated}\n…（已截断）")
    } else {
        rendered
    }
}

const HELP_TEXT: &str = "teamx 机器人可用命令：\n\
• 绑定 <code> — 绑定你的 IM 身份（code 由团队成员生成）\n\
• 状态 / status — 查看团队与目标状态\n\
• 任务 / tasks — 查看任务列表\n\
• 目标 / goal — 查看目标\n\
• 讨论 / discussions — 查看讨论\n\
• 问题 / questions — 查看向你提出的问题";

fn load_mtls(cert_dir: &std::path::Path) -> Result<MtlsMaterial, String> {
    let cert_pem = std::fs::read_to_string(cert_dir.join("service.crt"))
        .map_err(|e| format!("read service.crt: {e}"))?;
    let key_pem = std::fs::read_to_string(cert_dir.join("service.key"))
        .map_err(|e| format!("read service.key: {e}"))?;
    let ca_pem = std::fs::read_to_string(cert_dir.join("ca.crt"))
        .map_err(|e| format!("read ca.crt: {e}"))?;
    tunnel_client::material_from_pem(&cert_pem, &key_pem, &ca_pem, "im-gateway")
        .ok_or_else(|| "im-gateway: invalid service certificate material".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_command_binds_with_a_code() {
        let (method, args) = parse_command("绑定 ABC123").unwrap();
        assert_eq!(method, "im.bind");
        assert_eq!(args["code"], json!("ABC123"));
        let (method, args) = parse_command("bind xyz-9").unwrap();
        assert_eq!(method, "im.bind");
        assert_eq!(args["code"], json!("xyz-9"));
    }

    #[test]
    fn parse_command_maps_queries() {
        for (text, kind) in [
            ("状态", "status"),
            ("status", "status"),
            ("任务", "tasks"),
            ("tasks", "tasks"),
            ("目标", "goal"),
            ("讨论", "discussions"),
            ("问题", "questions"),
        ] {
            let (method, args) = parse_command(text).unwrap();
            assert_eq!(method, "im.query", "for {text}");
            assert_eq!(args["kind"], json!(kind), "for {text}");
        }
    }

    #[test]
    fn parse_command_handles_mention_prefix_and_noise() {
        // Feishu renders a mention as a placeholder token before the command.
        let (method, args) = parse_command("@_user_1 任务").unwrap();
        assert_eq!(method, "im.query");
        assert_eq!(args["kind"], json!("tasks"));
        // Trailing punctuation is stripped.
        let (method, args) = parse_command("@_user_1 状态？").unwrap();
        assert_eq!(method, "im.query");
        assert_eq!(args["kind"], json!("status"));
    }

    #[test]
    fn parse_command_returns_none_for_chatter() {
        assert!(parse_command("hello there").is_none());
        assert!(parse_command("").is_none());
        assert!(parse_command("绑定").is_none(), "a bind without a code is not a command");
    }

    #[test]
    fn render_reply_surfaces_errors_and_truncates() {
        assert_eq!(render_reply(&json!({"ok": false, "error": "boom"})), "❌ boom");
        let long = json!({ "ok": true, "data": { "blob": "x".repeat(4000) } });
        let rendered = render_reply(&long);
        assert!(rendered.contains("已截断"));
        assert!(rendered.chars().count() < 1700);
    }
}
