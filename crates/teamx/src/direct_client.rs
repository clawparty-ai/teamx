//! Direct-connect client (P1): provider `listen` + consumer `ping`.
//!
//! Provider (`teamx direct listen`): runs a rustls mTLS listener on a random
//! port, advertises it as a direct candidate, and validates each peer's
//! one-time pairing ticket against the server (`direct.ticket.check`).
//!
//! Consumer (`teamx direct ping`): probes the peer (which issues the ticket),
//! connects to the best candidate, presents the ticket, and measures RTTs.
//!
//! TLS identity: both sides present **member certificates signed by the team
//! CA** (the same letters used for server RPC). Member certificates carry no
//! SANs, so the consumer's verifier is name-agnostic and chain-only; the
//! identity binding comes from the one-time pairing ticket (server-audited,
//! `direct.rejected` on failure), not from a hostname — there is no hostname
//! for a member-to-member endpoint.

use std::net::UdpSocket;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, ServerConfig};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use crate::tunnel_client::{self, MtlsMaterial};

// ---------------------------------------------------------------------------
// TLS configs
// ---------------------------------------------------------------------------

/// Server-side (provider listener) TLS config: member cert as identity, team
/// CA as the client trust anchor. Peer certs are member certs signed by the
/// same CA, so the handshake proves "same team" before the ticket check.
pub fn listener_config(mtls: &MtlsMaterial) -> Result<Arc<ServerConfig>, String> {
    let mut roots = RootCertStore::empty();
    for c in &mtls.ca {
        roots
            .add(c.clone())
            .map_err(|e| format!("add CA to roots: {e}"))?;
    }
    let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
        .build()
        .map_err(|e| format!("client verifier: {e}"))?;
    let cert_chain = vec![mtls.cert.clone()];
    let key = mtls.key.clone_key();
    let config = ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(cert_chain, key)
        .map_err(|e| format!("listener config: {e}"))?;
    Ok(Arc::new(config))
}

/// Client-side (consumer ping) verifier: validates the provider chain against
/// the team CA but **ignores the server name** — a member-to-member endpoint
/// has no hostname, and member certificates carry no SANs. Identity binding is
/// carried by the one-time pairing ticket instead (validated server-side, so a
/// member impersonating the provider is caught by the server's audit trail).
#[derive(Debug)]
pub struct CaChainVerifier {
    roots: Arc<RootCertStore>,
    provider: Arc<CryptoProvider>,
    /// EKU required of the END ENTITY under verification: `client_auth` for
    /// member-cert listeners (direct connect), `server_auth` for the real
    /// mTLS `serve` (gateway process B connects to it).
    server_eku: bool,
}

impl CaChainVerifier {
    pub fn new(mtls: &MtlsMaterial) -> Result<Arc<Self>, String> {
        Self::with_eku(mtls, false)
    }

    /// Name-agnostic verifier requiring the **serverAuth** EKU on the end
    /// entity — for clients dialing the real `serve` (its server cert is a
    /// server cert, unlike the member-cert listeners of direct connect).
    pub fn with_server_eku(mtls: &MtlsMaterial) -> Result<Arc<Self>, String> {
        Self::with_eku(mtls, true)
    }

    fn with_eku(mtls: &MtlsMaterial, server_eku: bool) -> Result<Arc<Self>, String> {
        let mut roots = RootCertStore::empty();
        for c in &mtls.ca {
            roots.add(c.clone()).map_err(|e| format!("add CA: {e}"))?;
        }
        let provider = rustls::crypto::ring::default_provider();
        Ok(Arc::new(Self {
            roots: Arc::new(roots),
            provider: Arc::new(provider),
            server_eku,
        }))
    }
}

impl ServerCertVerifier for CaChainVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        // Name-agnostic chain validation — the same call shape rustls's own
        // `verify_server_cert_signed_by_trust_anchor_impl` uses, but with the
        // client_auth EKU (member certs are client certs) and NO name check:
        // there is no hostname for a member-to-member endpoint. Identity
        // binding is carried by the one-time pairing ticket (server-audited).
        let cert = webpki::EndEntityCert::try_from(end_entity)
            .map_err(|e| rustls::Error::General(format!("webpki parse: {e}")))?;
        let usage = if self.server_eku {
            webpki::KeyUsage::server_auth()
        } else {
            webpki::KeyUsage::client_auth()
        };
        cert.verify_for_usage(
            self.provider.signature_verification_algorithms.all,
            &self.roots.roots,
            intermediates,
            now,
            usage,
            None,
            None,
        )
        .map_err(|e| rustls::Error::General(format!("webpki chain: {e}")))?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// Client TLS config for the direct connection: member client cert + CA-only
/// (name-agnostic) verifier. See `CaChainVerifier` for the threat model.
/// Client TLS config with an explicit server-cert verifier (gateway process
/// B dials `serve` with the serverAuth EKU requirement; direct connect uses
/// the client_auth variant).
pub fn client_config_with_verifier(
    mtls: &MtlsMaterial,
    verifier: Arc<CaChainVerifier>,
) -> Result<Arc<ClientConfig>, String> {
    let builder = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(vec![mtls.cert.clone()], mtls.key.clone_key())
        .map_err(|e| format!("gateway client config: {e}"))?;
    Ok(Arc::new(builder))
}

pub fn direct_client_config(mtls: &MtlsMaterial) -> Result<Arc<ClientConfig>, String> {
    let verifier = CaChainVerifier::new(mtls)?;
    let builder = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(vec![mtls.cert.clone()], mtls.key.clone_key())
        .map_err(|e| format!("direct client config: {e}"))?;
    Ok(Arc::new(builder))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Best-effort primary LAN IP: the outbound interface toward the server (UDP
/// connect does not send packets, it just picks a route).
pub fn detect_lan_ip(server_url: &str) -> Option<String> {
    let host = tunnel_client::host_of(server_url);
    let (h, p) = match host.rsplit_once(':') {
        Some((a, b)) => (a.to_string(), b.parse::<u16>().unwrap_or(443)),
        None => (host.clone(), 443),
    };
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect((h.as_str(), p)).ok()?;
    let local = sock.local_addr().ok()?;
    if local.ip().is_loopback() {
        return None;
    }
    Some(local.ip().to_string())
}

/// Async RPC with an explicit client config (usable inside a tokio runtime —
/// unlike `run_rpc`, which builds a nested one).
pub(crate) async fn rpc_async(
    server_url: &str,
    method: &str,
    args: serde_json::Value,
    config: &Arc<ClientConfig>,
) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({ "method": method, "args": args }).to_string();
    let resp = tunnel_client::https_post_shared(server_url, "/rpc", &body, config, 15).await?;
    serde_json::from_str(&resp).map_err(|e| format!("bad rpc response: {e}"))
}

fn extract_data(v: &serde_json::Value) -> &serde_json::Value {
    v.get("data").unwrap_or(v)
}

// ---------------------------------------------------------------------------
// Provider: `teamx direct listen`
// ---------------------------------------------------------------------------

/// Run the direct-connect listener (blocking; Ctrl-C to stop).
pub fn listen(
    server_url: &str,
    team: Option<&str>,
    session: &str,
    bind: Option<&str>,
    port: Option<u16>,
    lan_ips: &[String],
) -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    rt.block_on(listen_async(
        server_url, team, session, bind, port, lan_ips,
    ))
}

async fn listen_async(
    server_url: &str,
    team: Option<&str>,
    session: &str,
    bind: Option<&str>,
    port: Option<u16>,
    lan_ips: &[String],
) -> Result<(), String> {
    let mtls = tunnel_client::mtls_for(server_url)
        .ok_or_else(|| "no mTLS material (import a letter first)".to_string())?;
    let tls = listener_config(&mtls)?;
    // Standard client config for server RPCs (the server cert HAS SANs).
    let rpc_config = tunnel_client::client_config(&mtls)?;

    // Advertise: LAN IPs (auto-detected primary + overrides) + listener port.
    // The server resolves the caller's single team when `team` is null (same
    // rule as every direct.* RPC), so no client-side team lookup is needed.
    let mut ips: Vec<String> = lan_ips.to_vec();
    if ips.is_empty() {
        if let Some(ip) = detect_lan_ip(server_url) {
            ips.push(ip);
        }
    }
    if ips.is_empty() {
        return Err("direct listen: no LAN IP detected — pass --lan-ip".into());
    }

    let bind_addr = match bind {
        Some(b) => b.to_string(),
        None => format!("0.0.0.0:{}", port.unwrap_or(0)),
    };
    let listener = TcpListener::bind(&bind_addr)
        .await
        .map_err(|e| format!("bind {bind_addr}: {e}"))?;
    let actual_port = listener.local_addr().map_err(|e| e.to_string())?.port();

    rpc_async(
        server_url,
        "direct.advertise",
        serde_json::json!({ "lan_ips": ips, "listener_port": actual_port, "session": session, "team": team }),
        &rpc_config,
    )
    .await
    .map_err(|e| format!("advertise: {e}"))?;
    eprintln!(
        "teamx direct listener on tcp://{bind_addr} (port {actual_port}), advertising lan_ips={ips:?} — Ctrl-C to stop"
    );

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (tcp, peer) = match accepted {
                    Ok(x) => x,
                    Err(e) => { eprintln!("direct accept error: {e}"); continue; }
                };
                let tls = tls.clone();
                let rpc_config = rpc_config.clone();
                let server_url = server_url.to_string();
                tokio::spawn(async move {
                    if let Err(e) =
                        serve_one(tcp, peer.to_string(), &tls, server_url, rpc_config).await
                    {
                        eprintln!("direct session from {peer}: {e}");
                    }
                });
            }
            _ = tokio::signal::ctrl_c() => {
                eprintln!("direct listener stopping");
                return Ok(());
            }
        }
    }
}

/// One provider-side session: TLS → read ticket → validate → pong loop.
async fn serve_one(
    tcp: tokio::net::TcpStream,
    peer_addr: String,
    tls: &Arc<ServerConfig>,
    server_url: String,
    rpc_config: Arc<ClientConfig>,
) -> Result<(), String> {
    let acceptor = tokio_rustls::TlsAcceptor::from(tls.clone());
    let tls_stream = acceptor
        .accept(tcp)
        .await
        .map_err(|e| format!("tls handshake: {e}"))?;

    let (mut reader, mut writer) = tokio::io::split(tls_stream);
    // First line: the one-time pairing ticket issued to this peer's prober.
    let mut line = String::new();
    BufReader::new(&mut reader)
        .read_line(&mut line)
        .await
        .map_err(|e| format!("read ticket: {e}"))?;
    let ticket = line.trim().to_string();
    if ticket.is_empty() {
        return Err("empty ticket line".into());
    }

    // Validate server-side (one-time, bound to this provider + team). The RPC
    // fails with HTTP 400 + {ok:false,error} on rejection; write a rejection
    // line to the peer either way so it fails fast and clearly.
    let check = rpc_async(
        &server_url,
        "direct.ticket.check",
        serde_json::json!({ "ticket": ticket }),
        &rpc_config,
    )
    .await;
    let (requester, reason) = match check {
        Ok(v) => {
            let data = extract_data(&v);
            match data.get("ok").and_then(|x| x.as_bool()) {
                Some(true) => (
                    data.get("requester_id")
                        .and_then(|x| x.as_str())
                        .unwrap_or("?")
                        .to_string(),
                    None,
                ),
                _ => (
                    String::new(),
                    Some(
                        data.get("error")
                            .and_then(|x| x.as_str())
                            .unwrap_or("ticket rejected")
                            .to_string(),
                    ),
                ),
            }
        }
        Err(e) => (String::new(), Some(e)),
    };
    if let Some(reason) = reason {
        let _ = writer
            .write_all(format!("rejected {reason}\n").as_bytes())
            .await;
        return Err(format!("ticket rejected by server: {reason}"));
    }
    writer
        .write_all(format!("ok {requester}\n").as_bytes())
        .await
        .map_err(|e| format!("write ok: {e}"))?;
    eprintln!("direct session established with {requester} ({peer_addr})");

    // Ping/pong loop: echo the sequence back so the consumer measures RTT.
    let mut lines = BufReader::new(reader).lines();
    while let Some(l) = lines.next_line().await.map_err(|e| format!("read ping: {e}"))? {
        let l = l.trim().to_string();
        if l.is_empty() || l == "bye" {
            break;
        }
        if let Some(seq) = l.strip_prefix("ping ") {
            writer
                .write_all(format!("pong {seq}\n").as_bytes())
                .await
                .map_err(|e| format!("write pong: {e}"))?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Consumer: `teamx direct ping`
// ---------------------------------------------------------------------------

/// End-to-end direct ping (blocking): probe → ticket → connect → validate →
/// RTT measurements.
pub fn ping(
    server_url: &str,
    team: Option<&str>,
    session: &str,
    peer: &str,
    count: u32,
    timeout_ms: u64,
) -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    rt.block_on(ping_async(server_url, team, session, peer, count, timeout_ms))
}

async fn ping_async(
    server_url: &str,
    team: Option<&str>,
    session: &str,
    peer: &str,
    count: u32,
    timeout_ms: u64,
) -> Result<(), String> {
    let mtls = tunnel_client::mtls_for(server_url)
        .ok_or_else(|| "no mTLS material (import a letter first)".to_string())?;
    let client_tls = direct_client_config(&mtls)?;
    // Standard client config for server RPCs (the server cert HAS SANs).
    let rpc_config = tunnel_client::client_config(&mtls)?;

    // 1) Candidates for the peer (same team only, server-checked).
    let c = rpc_async(
        server_url,
        "direct.candidates",
        serde_json::json!({ "member_id": peer, "session": session, "team": team }),
        &rpc_config,
    )
    .await
    .map_err(|e| format!("candidates: {e}"))?;
    let data = extract_data(&c);
    let addrs: Vec<String> = data
        .get("candidates")
        .and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    if addrs.is_empty() {
        return Err(format!("peer {peer} has no direct candidates"));
    }

    // 2) Probe: connectivity + one-time pairing ticket.
    let p = rpc_async(
        server_url,
        "direct.probe",
        serde_json::json!({ "member_id": peer, "timeout_ms": timeout_ms, "session": session, "team": team }),
        &rpc_config,
    )
    .await
    .map_err(|e| format!("probe: {e}"))?;
    let pdata = extract_data(&p);
    if pdata.get("reachable").and_then(|x| x.as_bool()) != Some(true) {
        return Err(format!("peer {peer} not reachable directly: {p}"));
    }
    let ticket = pdata
        .get("ticket")
        .and_then(|x| x.as_str())
        .ok_or_else(|| "probe succeeded but issued no ticket".to_string())?
        .to_string();

    // 3) Connect to the best candidate (probe already reported RTTs; try in
    //    the reported order — server orders LAN first, then reflexive).
    let results = pdata
        .get("results")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    let ordered: Vec<String> = results
        .iter()
        .filter(|r| r.get("ok").and_then(|x| x.as_bool()) == Some(true))
        .filter_map(|r| r.get("addr").and_then(|x| x.as_str().map(String::from)))
        .collect();
    let ordered: Vec<String> = if ordered.is_empty() { addrs.clone() } else { ordered };

    let mut last_err = String::new();
    for addr in &ordered {
        match ping_one(addr, &ticket, count, client_tls.clone(), peer).await {
            Ok(rtts) => {
                println!("direct ping {peer} via {addr}: {} exchanges", rtts.len());
                for (i, rtt) in rtts.iter().enumerate() {
                    println!("  seq={i} rtt={rtt}ms");
                }
                if let (Some(first), Some(last)) = (rtts.first(), rtts.last()) {
                    println!(
                        "  min={}ms max={}ms",
                        rtts.iter().min().unwrap_or(first),
                        rtts.iter().max().unwrap_or(last)
                    );
                }
                return Ok(());
            }
            Err(e) => last_err = e,
        }
    }
    Err(format!("direct ping failed on all candidates: {last_err}"))
}

async fn ping_one(
    addr: &str,
    ticket: &str,
    count: u32,
    client_tls: Arc<ClientConfig>,
    peer: &str,
) -> Result<Vec<u128>, String> {
    let sock: std::net::SocketAddr = addr
        .parse()
        .map_err(|_| format!("candidate {addr} is not a socket address"))?;
    let tcp = tokio::net::TcpStream::connect(sock)
        .await
        .map_err(|e| format!("connect {addr}: {e}"))?;
    let server_name = ServerName::IpAddress(sock.ip().into());
    let connector = tokio_rustls::TlsConnector::from(client_tls);
    let tls = connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| format!("tls to {addr}: {e}"))?;

    // Present the pairing ticket; the provider validates it server-side.
    let (mut reader, mut writer) = tokio::io::split(tls);
    writer
        .write_all(format!("{ticket}\n").as_bytes())
        .await
        .map_err(|e| format!("write ticket: {e}"))?;
    let mut line = String::new();
    BufReader::new(&mut reader)
        .read_line(&mut line)
        .await
        .map_err(|e| format!("read ack: {e}"))?;
    let ack = line.trim();
    if !ack.starts_with("ok ") {
        return Err(format!("ticket rejected by provider: {ack} (peer {peer})"));
    }

    // Ping/pong RTTs.
    let mut rtts = Vec::new();
    for i in 0..count {
        let t0 = std::time::Instant::now();
        writer
            .write_all(format!("ping {i}\n").as_bytes())
            .await
            .map_err(|e| format!("write ping: {e}"))?;
        let mut line = String::new();
        BufReader::new(&mut reader)
            .read_line(&mut line)
            .await
            .map_err(|e| format!("read pong: {e}"))?;
        let pong = line.trim();
        if pong != format!("pong {i}") {
            return Err(format!("bad pong: {pong:?}"));
        }
        rtts.push(t0.elapsed().as_millis());
    }
    writer
        .write_all(b"bye\n")
        .await
        .map_err(|e| format!("write bye: {e}"))?;
    Ok(rtts)
}
