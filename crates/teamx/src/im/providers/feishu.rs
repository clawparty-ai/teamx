//! Feishu (Lark) provider — Phase-1 implementation.
//!
//! Feishu is the Phase-1 platform because its **long-connection event
//! subscription is an outbound connection** (no public callback endpoint
//! needed), which fits self-hosted / NAT deployments (design §5.1).
//!
//! This file lands the provider primitives. The live long-connection client
//! and HTTP send transport are wired by `im-p1-outbound-notifier` /
//! `im-p1-inbound-gateway-security`; here they are dry-run / stubbed so the
//! seam is exercisable and testable today.

use crate::im::config::ImConfig;
use crate::im::providers::ImProvider;
use crate::im::types::{ImChannel, ImDelegatedContext, ImEvent, ImMessage};

/// Feishu provider.
pub struct FeishuProvider {
    cfg: ImConfig,
}

impl FeishuProvider {
    pub fn new(cfg: ImConfig) -> Self {
        Self { cfg }
    }

    /// Human-readable channel label for logs.
    fn channel_label(ch: &ImChannel) -> String {
        match ch {
            ImChannel::Group(id) => format!("group:{id}"),
            ImChannel::Direct(id) => format!("direct:{id}"),
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
        if !self.cfg.feishu_configured() {
            return Err("feishu not configured: TEAMX_IM_FEISHU_APP_ID / _APP_SECRET missing".into());
        }
        // Live transport is landed by im-p1-outbound-notifier.
        Err("feishu send: live transport not wired yet (see im-p1-outbound-notifier)".into())
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
