//! tun_dns.rs — fake-ip DNS for the tun0 proxy.
//!
//! Runs a tiny UDP DNS responder that answers A queries with a fake IP from
//! the reserved range (default 198.18.0.0/15) and keeps a `fake_ip -> domain`
//! map. When a TCP connection to a fake IP arrives on the tun device,
//! `tun_socks` resolves the original domain via `FakeIpDns::lookup` so the
//! exit can dial by hostname (its own resolver).

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// Minimum TTL for fake answers (helps caches).
const FAKE_TTL: u32 = 60;

/// Reserves one fake IP per domain, deduplicated.
pub struct FakeIpDns {
    net_base: Ipv4Addr,
    prefix: u8,
    /// fake_ip (as u32) -> domain
    by_ip: Mutex<HashMap<u32, String>>,
    /// domain -> fake_ip
    by_domain: Mutex<HashMap<String, u32>>,
    next: AtomicU32,
}

impl FakeIpDns {
    /// `base` is the network address (e.g. 198.18.0.0); `prefix` the CIDR
    /// (e.g. 15). Host bits start at offset 1 (base itself is the gateway).
    pub fn new(base: Ipv4Addr, prefix: u8) -> Arc<FakeIpDns> {
        let base_u32 = u32::from(base);
        let start = base_u32 + 1; // skip the gateway (base)
        Arc::new(FakeIpDns {
            net_base: base,
            prefix,
            by_ip: Mutex::new(HashMap::new()),
            by_domain: Mutex::new(HashMap::new()),
            next: AtomicU32::new(start),
        })
    }

    /// Return the fake IP for a domain, allocating one if needed.
    pub fn alloc(&self, domain: &str) -> Ipv4Addr {
        let d = domain.to_ascii_lowercase();
        if let Some(ip) = self.by_domain.lock().unwrap().get(&d) {
            return Ipv4Addr::from(*ip);
        }
        let host_bits = 32 - self.prefix as u32;
        let base_u32 = u32::from(self.net_base);
        let mask_hosts = if host_bits >= 32 { 0xFFFF_FFFF } else { (1u32 << host_bits) - 1 };
        // pick next candidate, skip base and broadcast
        loop {
            let cand = self.next.fetch_add(1, Ordering::Relaxed);
            if cand == base_u32 || (cand & mask_hosts) == mask_hosts {
                continue;
            }
            let ip_u32 = cand;
            let mut ip_map = self.by_ip.lock().unwrap();
            let mut dom_map = self.by_domain.lock().unwrap();
            if let Some(existing) = ip_map.get(&ip_u32) {
                // collision with an existing mapping -> reuse that domain? just retry
                let _ = existing;
                drop(dom_map);
                drop(ip_map);
                continue;
            }
            ip_map.insert(ip_u32, d.clone());
            dom_map.insert(d.clone(), ip_u32);
            return Ipv4Addr::from(ip_u32);
        }
    }

    /// Map a fake IP back to its domain (if it is one).
    pub fn lookup(&self, ip: Ipv4Addr) -> Option<String> {
        self.by_ip.lock().unwrap().get(&u32::from(ip)).cloned()
    }

    /// Parse a DNS query and produce a fake-ip A-record response.
    /// Returns None for unsupported query types (CNAME, AAAA unless we fake v6).
    pub fn answer(&self, query: &[u8]) -> Option<Vec<u8>> {
        // Minimal DNS parsing: header (12 bytes) + question.
        if query.len() < 12 {
            return None;
        }
        let id = u16::from_be_bytes([query[0], query[1]]);
        // QR bit must be 0 (it's a query), opcode 0.
        let qdcount = u16::from_be_bytes([query[4], query[5]]);
        if qdcount == 0 {
            return None;
        }
        // Parse the QNAME.
        let mut pos = 12usize;
        let mut name = Vec::new();
        let mut ok = true;
        loop {
            if pos >= query.len() {
                ok = false;
                break;
            }
            let len = query[pos] as usize;
            if len == 0 {
                pos += 1;
                break;
            }
            if len & 0xC0 != 0 {
                // compression pointer in question is unusual; bail
                ok = false;
                break;
            }
            if pos + 1 + len > query.len() {
                ok = false;
                break;
            }
            if !name.is_empty() {
                name.push(b'.');
            }
            name.extend_from_slice(&query[pos + 1..pos + 1 + len]);
            pos += 1 + len;
        }
        if !ok || pos + 4 > query.len() {
            return None;
        }
        let qtype = u16::from_be_bytes([query[pos], query[pos + 1]]);
        // Only A records (1). (AAAA = 28 is out of scope for v1.)
        if qtype != 1 {
            return None;
        }
        let domain = String::from_utf8_lossy(&name).to_string();
        if domain.is_empty() {
            return None;
        }
        let fake = self.alloc(&domain);

        // Build the response: header + question + answer.
        let mut resp = Vec::with_capacity(64);
        resp.extend_from_slice(&id.to_be_bytes());
        // flags: 0x8180 = response, RD+RA, no error
        resp.extend_from_slice(&[0x81, 0x80]);
        resp.extend_from_slice(&[0x00, 0x01]); // QDCOUNT
        resp.extend_from_slice(&[0x00, 0x01]); // ANCOUNT
        resp.extend_from_slice(&[0x00, 0x00]); // NSCOUNT
        resp.extend_from_slice(&[0x00, 0x00]); // ARCOUNT
        // question (echo)
        resp.extend_from_slice(&query[12..pos + 4]);
        // answer: name pointer to 0xC00C
        resp.extend_from_slice(&[0xC0, 0x0C]);
        resp.extend_from_slice(&qtype.to_be_bytes()); // type A
        resp.extend_from_slice(&[0x00, 0x01]); // class IN
        resp.extend_from_slice(&FAKE_TTL.to_be_bytes()); // TTL
        resp.extend_from_slice(&[0x00, 0x04]); // RDLENGTH=4
        resp.extend_from_slice(&fake.octets());
        Some(resp)
    }
}

/// Serve the fake-ip DNS responder on port 53 (UDP).
/// Binds the tun gateway IP first (e.g. 198.18.0.1:53) so it does not clash
/// with systemd-resolved / other services on 0.0.0.0:53; falls back to
/// 0.0.0.0:53 if the specific IP bind fails.
pub async fn serve_udp(dns: Arc<FakeIpDns>, bind_ip: Ipv4Addr) -> Result<(), String> {
    use tokio::net::UdpSocket;

    let addr = SocketAddr::from((bind_ip, 53));
    let sock = match UdpSocket::bind(addr).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("tun: fake-dns bind {addr} failed ({e}); trying 0.0.0.0:53");
            let fallback = SocketAddr::from((Ipv4Addr::UNSPECIFIED, 53));
            UdpSocket::bind(fallback).await.map_err(|e2| format!("dns bind fallback {fallback}: {e2}"))?
        }
    };
    println!("ok fake-dns: listening udp://{addr}");
    let mut buf = [0u8; 512];
    loop {
        let (n, peer) = sock.recv_from(&mut buf).await.map_err(|e| format!("dns recv: {e}"))?;
        if let Some(resp) = dns.answer(&buf[..n]) {
            let _ = sock.send_to(&resp, peer).await;
        }
        // Unsupported queries (AAAA etc.) are silently dropped (no answer).
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dns() -> Arc<FakeIpDns> {
        FakeIpDns::new(Ipv4Addr::new(198, 18, 0, 0), 15)
    }

    #[test]
    fn alloc_and_lookup_roundtrip() {
        let d = dns();
        let ip = d.alloc("example.com");
        assert!(d.lookup(ip).is_some());
        assert_eq!(d.lookup(ip).unwrap(), "example.com");
        // alloc same domain -> same fake ip
        let ip2 = d.alloc("example.com");
        assert_eq!(ip, ip2);
    }

    #[test]
    fn alloc_distinct_domains_get_distinct_ips() {
        let d = dns();
        let a = d.alloc("a.example");
        let b = d.alloc("b.example");
        assert_ne!(a, b);
    }

    #[test]
    fn lookup_unknown_ip_returns_none() {
        let d = dns();
        assert!(d.lookup(Ipv4Addr::new(1, 2, 3, 4)).is_none());
    }

    #[test]
    fn alloc_ips_in_fake_range() {
        let d = dns();
        let base = u32::from(Ipv4Addr::new(198, 18, 0, 0));
        for i in 0..100 {
            let ip = d.alloc(&format!("host{i}.example"));
            let v = u32::from(ip);
            assert!(v >= base && v < base + (1 << 17), "ip {ip} out of 198.18.0.0/15");
        }
    }

    #[test]
    fn dns_answer_a_record() {
        let d = dns();
        // Build a query for example.com (A).
        let mut q = Vec::new();
        q.extend_from_slice(&[0x12, 0x34]); // id
        q.extend_from_slice(&[0x01, 0x00]); // flags: RD
        q.extend_from_slice(&[0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]); // counts
        q.extend_from_slice(&[7]); q.extend_from_slice(b"example");
        q.extend_from_slice(&[3]); q.extend_from_slice(b"com");
        q.extend_from_slice(&[0]);
        q.extend_from_slice(&[0x00, 0x01]); // A
        q.extend_from_slice(&[0x00, 0x01]); // IN

        let resp = d.answer(&q).expect("should answer");
        assert_eq!(&resp[0..2], &[0x12, 0x34]); // id echoed
        assert_eq!(resp[2], 0x81); // QR=1, opcode 0
        assert_eq!(u16::from_be_bytes([resp[6], resp[7]]), 1); // ANCOUNT=1
        // answer RDATA = the fake IP
        let rdata = &resp[resp.len() - 4..];
        let fake = Ipv4Addr::new(rdata[0], rdata[1], rdata[2], rdata[3]);
        assert!(d.lookup(fake).is_some());
        assert_eq!(d.lookup(fake).unwrap(), "example.com");
    }

    #[test]
    fn dns_answer_unsupported_type_none() {
        let d = dns();
        let mut q = Vec::new();
        q.extend_from_slice(&[0x00, 0x01]);
        q.extend_from_slice(&[0x01, 0x00]);
        q.extend_from_slice(&[0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        q.extend_from_slice(&[1]); q.extend_from_slice(b"a");
        q.extend_from_slice(&[0]);
        q.extend_from_slice(&[0x00, 0x1c]); // AAAA
        q.extend_from_slice(&[0x00, 0x01]);
        assert!(d.answer(&q).is_none());
    }
}
