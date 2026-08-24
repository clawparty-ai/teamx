//! dns_proxy.rs — local DNS server (127.0.0.1:53) for transparent proxying.
//!
//! The system resolver is pointed at this loopback listener. Queries for
//! domains the routing rules intercept are resolved through a teamx proxy exit
//! (whose resolver is uncensored), the resulting real IPs get host routes on
//! the tun device, and the real IPs are returned to the client. Everything else
//! is forwarded to the upstream (system) DNS unchanged.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::tun_dns::{build_a_response, parse_dns_query};

/// Cached resolution for one domain.
struct CacheEntry {
    ips: Vec<Ipv4Addr>,
    expires_at: Instant,
}

/// Spawn the local DNS proxy on a dedicated blocking thread.
///
/// * `server_url` / `exit` — teamx server + proxy exit used for uncensored
///   resolution of intercepted domains.
/// * `patterns` — domain patterns (exact / `*.suffix`) to intercept.
/// * `ip_map` — shared `ip -> domain` map (also used by tun0 to dial by name).
/// * `dev` — tun device name for the host routes.
/// * `upstream` — system DNS servers used to forward non-intercepted domains.
pub fn spawn(
    server_url: String,
    exit: String,
    patterns: Vec<String>,
    ip_map: Arc<Mutex<HashMap<Ipv4Addr, String>>>,
    dev: String,
    upstream: Vec<SocketAddr>,
) -> std::io::Result<()> {
    let sock = UdpSocket::bind("127.0.0.1:53")?;
    std::thread::spawn(move || {
        // TTL cache so repeated queries for the same domain don't each pay the
        // full server→exit round trip (~1 s).
        let mut cache: HashMap<String, CacheEntry> = HashMap::new();
        let mut buf = [0u8; 4096];
        loop {
            let (n, peer) = match sock.recv_from(&mut buf) {
                Ok(x) => x,
                Err(_) => continue,
            };
            let query = buf[..n].to_vec();
            let Some((name, qtype, _)) = parse_dns_query(&query) else {
                continue;
            };
            // Only A queries are handled; others are forwarded upstream.
            if qtype != 1 {
                forward_upstream(&sock, &query, peer, &upstream);
                continue;
            }
            let intercepted = patterns.iter().any(|p| domain_matches(p, &name));
            if intercepted {
                // Serve from cache when fresh; otherwise resolve via the exit,
                // route the IPs, and answer with the real addresses.
                let addrs = match cache.get(&name) {
                    Some(e) if e.expires_at > Instant::now() => e.ips.clone(),
                    _ => {
                        cache.remove(&name);
                        let ips = crate::tunnel_client::resolve_dns(&server_url, &exit, &name);
                        let mut resolved: Vec<Ipv4Addr> = Vec::new();
                        for s in &ips {
                            if let Ok(ip) = s.parse::<Ipv4Addr>() {
                                crate::tun_dev::add_ip_route(ip, &dev).ok();
                                ip_map.lock().unwrap().insert(ip, name.clone());
                                resolved.push(ip);
                            }
                        }
                        if !resolved.is_empty() {
                            cache.insert(
                                name.clone(),
                                CacheEntry {
                                    ips: resolved.clone(),
                                    expires_at: Instant::now() + Duration::from_secs(60),
                                },
                            );
                        }
                        resolved
                    }
                };
                if !addrs.is_empty() {
                    if let Some(resp) = build_a_response(&query, &addrs) {
                        let _ = sock.send_to(&resp, peer);
                    }
                }
            } else {
                forward_upstream(&sock, &query, peer, &upstream);
            }
        }
    });
    Ok(())
}

/// Forward a DNS query to the upstream servers and relay the first response
/// back to `peer`. Best-effort.
fn forward_upstream(sock: &UdpSocket, query: &[u8], peer: SocketAddr, upstream: &[SocketAddr]) {
    let Ok(tmp) = UdpSocket::bind("0.0.0.0:0") else {
        return;
    };
    let _ = tmp.set_read_timeout(Some(std::time::Duration::from_secs(3)));
    let mut resp = [0u8; 4096];
    for dns in upstream {
        if tmp.send_to(query, dns).is_err() {
            continue;
        }
        if let Ok((n, _)) = tmp.recv_from(&mut resp) {
            let _ = sock.send_to(&resp[..n], peer);
            return;
        }
    }
}

/// Match a domain against an exact (`example.com`) or suffix (`*.domain`)
/// pattern, case-insensitively.
fn domain_matches(pattern: &str, domain: &str) -> bool {
    let d = domain.to_ascii_lowercase();
    if let Some(suffix) = pattern.strip_prefix("*.") {
        let suf = format!(".{}", suffix.to_ascii_lowercase());
        d.len() > suf.len() && d.ends_with(&suf)
    } else {
        d == pattern.to_ascii_lowercase()
    }
}
