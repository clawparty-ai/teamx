//! Feishu (Lark) provider — Phase-1 implementation.
//!
//! Feishu is the Phase-1 platform because its **long-connection event
//! subscription is an outbound connection** (no public callback endpoint
//! needed), which fits self-hosted / NAT deployments (design §5.1).
//!
//! Live transport (landed by `im-p1-outbound-notifier`, docs/57): `send`
//! fetches a cached `tenant_access_token` and posts to
//! `im/v1/messages` (group = `chat_id`, direct = `open_id`). A platform
//! rate limit (HTTP 429) surfaces as an error containing `429` so the
//! notifier's backoff can respect it. Signature verification still lands
//! with the gateway (`im-p1-inbound-gateway-security`): `verify` requires a
//! signature to be present but does not yet validate it. All sends honour
//! `dry_run` (default **on**) — an unconfigured deployment never touches the
//! network.

use std::sync::Mutex;
use std::time::{Duration, UNIX_EPOCH};

use crate::im::config::ImConfig;
use crate::im::providers::ImProvider;
use crate::im::types::{ImChannel, ImDelegatedContext, ImEvent, ImMessage};

const FEISHU_BASE: &str = "https://open.feishu.cn/open-apis";
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// Feishu provider.
pub struct FeishuProvider {
    cfg: ImConfig,
    /// Cached tenant_access_token: (token, expires_at unix seconds).
    token: Mutex<Option<(String, i64)>>,
}

impl FeishuProvider {
    pub fn new(cfg: ImConfig) -> Self {
        Self { cfg, token: Mutex::new(None) }
    }

    /// Human-readable channel label for logs.
    fn channel_label(ch: &ImChannel) -> String {
        match ch {
            ImChannel::Group(id) => format!("group:{id}"),
            ImChannel::Direct(id) => format!("direct:{id}"),
        }
    }

    /// receive_id_type for the feishu messages API, per channel kind.
    fn receive_id_type(ch: &ImChannel) -> &'static str {
        match ch {
            ImChannel::Group(_) => "chat_id",
            ImChannel::Direct(_) => "open_id",
        }
    }

    fn receive_id(ch: &ImChannel) -> &str {
        match ch {
            ImChannel::Group(id) | ImChannel::Direct(id) => id,
        }
    }

    /// Fetch (and cache) a tenant_access_token. Token lives ~2 h on the
    /// platform; we refresh 5 min before expiry.
    fn tenant_token(&self) -> Result<String, String> {
        let now = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if let Some(tok) = self.token.lock().unwrap().as_ref() {
            if now < tok.1 - 300 {
                return Ok(tok.0.clone());
            }
        }
        let app_id = self
            .cfg
            .feishu_app_id
            .as_deref()
            .ok_or("feishu not configured: TEAMX_IM_FEISHU_APP_ID missing")?;
        let app_secret = self
            .cfg
            .feishu_app_secret
            .as_deref()
            .ok_or("feishu not configured: TEAMX_IM_FEISHU_APP_SECRET missing")?;
        let resp: serde_json::Value = ureq::post(&format!("{FEISHU_BASE}/auth/v3/tenant_access_token/internal"))
            .timeout(HTTP_TIMEOUT)
            .send_json(serde_json::json!({ "app_id": app_id, "app_secret": app_secret }))
            .map_err(|e| format!("feishu token request failed: {e}"))?
            .into_json()
            .map_err(|e| format!("feishu token decode failed: {e}"))?;
        let code = resp.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
        if code != 0 {
            return Err(format!("feishu token error code={code}: {}", resp.get("msg").and_then(|v| v.as_str()).unwrap_or("")));
        }
        let token = resp
            .get("tenant_access_token")
            .and_then(|v| v.as_str())
            .ok_or("feishu token response missing tenant_access_token")?
            .to_string();
        let expire = resp.get("expire").and_then(|v| v.as_i64()).unwrap_or(7200);
        *self.token.lock().unwrap() = Some((token.clone(), now + expire));
        Ok(token)
    }

    /// One feishu HTTP send. Maps HTTP 429 to an error containing "429".
    fn http_send(&self, token: &str, msg: &ImMessage) -> Result<(), String> {
        let id_type = Self::receive_id_type(&msg.channel);
        let url = format!("{FEISHU_BASE}/im/v1/messages?receive_id_type={id_type}");
        let content = serde_json::json!({ "text": msg.body }).to_string();
        let body = serde_json::json!({
            "receive_id": Self::receive_id(&msg.channel),
            "msg_type": "text",
            "content": content,
        });
        match ureq::post(&url)
            .timeout(HTTP_TIMEOUT)
            .set("Authorization", &format!("Bearer {token}"))
            .send_json(body)
        {
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(code, resp)) => {
                let snippet = resp
                    .into_string()
                    .unwrap_or_default()
                    .chars()
                    .take(200)
                    .collect::<String>();
                if code == 429 {
                    Err(format!("feishu send: 429 rate limited: {snippet}"))
                } else {
                    Err(format!("feishu send: http {code}: {snippet}"))
                }
            }
            Err(e) => Err(format!("feishu send: {e}")),
        }
    }
}

impl ImProvider for FeishuProvider {
    fn platform(&self) -> &'static str {
        "feishu"
    }

    fn send(&self, msg: &ImMessage) -> Result<(), String> {
        let to = Self::channel_label(&msg.channel);
        if self.cfg.dry_run {
            // Never touches the network in dry-run: safe by default.
            eprintln!(
                "[im:feishu][dry-run] -> {to} ({}) {}",
                if msg.markdown { "card" } else { "text" },
                msg.body.chars().take(120).collect::<String>()
            );
            return Ok(());
        }
        let token = self.tenant_token()?;
        self.http_send(&token, msg)
    }

    fn verify(&self, ctx: &ImDelegatedContext) -> Result<(), String> {
        if ctx.platform != "feishu" {
            return Err(format!("unexpected platform `{}` (expected feishu)", ctx.platform));
        }
        // Full signature/timestamp/nonce verification lands with the gateway
        // (im-p1-inbound-gateway-security); here we require a signature to be
        // present so an unsigned context can never be treated as verified.
        if ctx.signature.as_deref().unwrap_or("").is_empty() {
            return Err("feishu verify: missing signature".into());
        }
        Ok(())
    }

    fn parse_event(&self, raw: &str) -> Result<ImEvent, String> {
        let v: serde_json::Value = serde_json::from_str(raw).map_err(|e| format!("feishu parse: {e}"))?;
        let get = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        Ok(ImEvent {
            platform: "feishu".to_string(),
            // `message_id` (message events) or `event_id` (callback envelope).
            im_msg_id: if get("message_id").is_empty() { get("event_id") } else { get("message_id") },
            im_user_id: if get("open_id").is_empty() { get("user_id") } else { get("open_id") },
            text: get("text"),
        })
    }

    fn resolve_identity(&self, ctx: &ImDelegatedContext) -> Result<String, String> {
        if ctx.im_user_id.trim().is_empty() {
            return Err("feishu resolve_identity: empty im_user_id".into());
        }
        Ok(ctx.im_user_id.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(dry_run: bool) -> FeishuProvider {
        FeishuProvider::new(ImConfig {
            feishu_app_id: Some("cli_x".into()),
            feishu_app_secret: Some("secret".into()),
            dry_run,
            ..Default::default()
        })
    }

    #[test]
    fn dry_run_send_is_ok_and_never_errors() {
        let p = provider(true);
        let msg = ImMessage {
            channel: ImChannel::Group("oc_1".into()),
            title: None,
            body: "hello".into(),
            markdown: true,
        };
        assert_eq!(p.platform(), "feishu");
        assert!(p.send(&msg).is_ok());
    }

    #[test]
    fn live_send_without_config_errors() {
        let p = FeishuProvider::new(ImConfig { dry_run: false, ..Default::default() });
        let msg = ImMessage { channel: ImChannel::Direct("ou_1".into()), title: None, body: "x".into(), markdown: false };
        assert!(p.send(&msg).is_err());
    }

    #[test]
    fn parse_event_normalizes_fields() {
        let p = provider(true);
        let ev = p.parse_event(r#"{"message_id":"om_1","open_id":"ou_1","text":"/team status"}"#).unwrap();
        assert_eq!(ev.platform, "feishu");
        assert_eq!(ev.im_msg_id, "om_1");
        assert_eq!(ev.im_user_id, "ou_1");
        assert_eq!(ev.text, "/team status");
        // falls back to event_id / user_id
        let ev2 = p.parse_event(r#"{"event_id":"ev_9","user_id":"u_9"}"#).unwrap();
        assert_eq!(ev2.im_msg_id, "ev_9");
        assert_eq!(ev2.im_user_id, "u_9");
    }

    #[test]
    fn verify_requires_signature_and_platform() {
        let p = provider(true);
        let mut ctx = ImDelegatedContext { platform: "feishu".into(), im_user_id: "ou_1".into(), ..Default::default() };
        assert!(p.verify(&ctx).is_err(), "unsigned context must be rejected");
        ctx.signature = Some("sig".into());
        assert!(p.verify(&ctx).is_ok());
        ctx.platform = "wecom".into();
        assert!(p.verify(&ctx).is_err(), "wrong platform must be rejected");
    }

    #[test]
    fn resolve_identity_rejects_empty() {
        let p = provider(true);
        assert!(p.resolve_identity(&ImDelegatedContext { platform: "feishu".into(), ..Default::default() }).is_err());
        assert_eq!(
            p.resolve_identity(&ImDelegatedContext { platform: "feishu".into(), im_user_id: "ou_1".into(), ..Default::default() }).unwrap(),
            "ou_1"
        );
    }

    #[test]
    fn config_defaults_to_dry_run() {
        // No env set in the test process for the IM keys -> dry_run defaults true.
        let c = ImConfig::default();
        assert!(!c.feishu_configured());
        assert!(!c.dry_run); // Default derives false; from_env() is the safe path.
        let from_env = ImConfig::from_env();
        // `from_env` must default dry_run=true when the var is absent.
        if std::env::var("TEAMX_IM_DRY_RUN").is_err() {
            assert!(from_env.dry_run);
        }
    }
}
