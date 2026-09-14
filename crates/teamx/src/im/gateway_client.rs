//! Gateway process B (client side): a standalone `teamx im-gateway` process
//! that connects to `serve` with a `service:im-gateway:<instance>`
//! certificate (short TTL) and forwards delegated IM actions (design §5.3
//! identity B, task `im-p2-gateway-process-b`).
//!
//! The delegation header carries `platform / im_user / im_team` plus
//! signature + timestamp + one-time nonce — **never a member_id**: the
//! server resolves the actor from the binding table and enforces the frozen
//! method whitelist (fail-closed).

use std::path::PathBuf;

use serde_json::{json, Value};

use crate::direct_client;
use crate::im::config::ImConfig;
use crate::im::types::ImDelegatedContext;
use crate::tunnel_client::{self, MtlsMaterial};

/// Inputs for one `im-gateway check` round trip.
pub struct CheckArgs {
    /// Directory holding `service.crt` + `service.key` + `ca.crt`.
    pub cert_dir: PathBuf,
    /// Server base URL (`https://host:port`).
    pub server: String,
    /// Team scope (optional when the instance has exactly one team).
    pub team: Option<String>,
    pub platform: String,
    pub im_user: String,
    /// Query kind for the whitelisted read face (`im.query`).
    pub kind: String,
}

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

/// Build a signed delegation context (timestamp = now, one-time nonce).
pub fn build_delegation(platform: &str, im_user: &str, secret: &str) -> ImDelegatedContext {
    let mut ctx = ImDelegatedContext {
        platform: platform.to_string(),
        im_user_id: im_user.to_string(),
        im_team: None,
        signature: None,
        timestamp: chrono::Utc::now().timestamp(),
        nonce: uuid::Uuid::new_v4().to_string(),
    };
    ctx.signature = Some(crate::im::gateway::sign_delegation(secret, &ctx));
    ctx
}

/// One end-to-end round trip: service cert mTLS → whitelist → delegation
/// verify → binding resolve → whitelisted read. Blocking.
pub fn check(args: CheckArgs) -> Result<Value, String> {
    let mtls = load_mtls(&args.cert_dir)?;
    let cn = read_cn(&args.cert_dir)?;
    // The gateway dials the REAL `serve` (server cert = serverAuth EKU),
    // unlike direct connect's member-cert listeners.
    let verifier = direct_client::CaChainVerifier::with_server_eku(&mtls)?;
    let config = direct_client::client_config_with_verifier(&mtls, verifier)?;

    let cfg = ImConfig::from_env();
    let secret = cfg
        .secret_for(&args.platform)
        .ok_or_else(|| format!(
            "no provider secret in env for `{}` (set TEAMX_IM_FEISHU_APP_SECRET)",
            args.platform
        ))?;
    let ctx = build_delegation(&args.platform, &args.im_user, secret);

    let mut rpc_args = json!({
        "platform": ctx.platform,
        "im_user_id": ctx.im_user_id,
        "im_team": ctx.im_team,
        "signature": ctx.signature,
        "timestamp": ctx.timestamp,
        "nonce": ctx.nonce,
        "kind": args.kind,
    });
    if let Some(t) = &args.team {
        rpc_args["team"] = json!(t);
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    let resp = rt.block_on(direct_client::rpc_async(&args.server, "im.query", rpc_args, &config))?;

    Ok(json!({
        "ok": true,
        "cn": cn,
        "delegation": { "platform": ctx.platform, "im_user": ctx.im_user_id, "nonce": ctx.nonce },
        "response": resp,
    }))
}

fn read_cn(cert_dir: &std::path::Path) -> Result<String, String> {
    let pem = std::fs::read_to_string(cert_dir.join("service.crt"))
        .map_err(|e| format!("read service.crt: {e}"))?;
    let der = rustls_pemfile::certs(&mut pem.as_bytes())
        .next()
        .ok_or("service.crt: empty")?
        .map_err(|e| format!("parse service.crt: {e}"))?;
    let (_, cert) = x509_parser::parse_x509_certificate(&der).map_err(|e| format!("parse cert: {e}"))?;
    // x509_parser exposes the CN via the subject's iter_rdn/attr API.
    let cn = cert
        .subject()
        .iter_rdn()
        .flat_map(|rdn| rdn.iter())
        .find(|atv| atv.attr_type().to_string() == "2.5.4.3")
        .map(|atv| String::from_utf8_lossy(atv.attr_value().data).to_string())
        .unwrap_or_default();
    Ok(cn)
}
