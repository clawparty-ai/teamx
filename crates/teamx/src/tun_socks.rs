//! tun_socks.rs — bridge established user-space TCP sockets to teamx exits.
//!
//! Runs a single-threaded tokio runtime that owns the smoltcp stack (not
//! Send) plus the tunnel bridges. Every inbound connection whose handshake
//! completes in user space opens a `TunnelBridge` to the egress chosen by the
//! route table, and bytes are copied in both directions in the main loop.

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};

use smoltcp::iface::SocketHandle;
use smoltcp::wire::IpEndpoint;
use crate::routes::RouteTable;
use crate::tun_dev::TunDevice;
use crate::tun_dns::FakeIpDns;
use crate::tun_stack::{BridgeState, StackConfig, TunStack};

/// Options for `teamx tun0 start`.
pub struct TunOptions {
    pub server_url: String,
    pub tun_ip: Ipv4Addr,
    pub netmask: Ipv4Addr,
    pub fake_net: (Ipv4Addr, u8),
    pub max_conns: usize,
    pub default_exit: String,
    pub routes: Option<RouteTable>,
    pub dns: Arc<FakeIpDns>,
    /// Enable fake-ip DNS hijacking (system DNS -> tun gateway). Default off.
    pub fake_dns: bool,
}

/// Blocking entrypoint (mirrors `tunnel_client::socks5_proxy`): builds a
/// single-thread runtime and runs the tun proxy forever.
pub fn tun_proxy(opts: TunOptions) -> Result<serde_json::Value, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    rt.block_on(run_tun_proxy(opts))?;
    Ok(serde_json::json!({ "ok": true }))
}

/// Main async loop. Runs on a current-thread runtime so the smoltcp stack
/// (non-Send) and the tunnel bridges share one task.
pub async fn run_tun_proxy(opts: TunOptions) -> Result<(), String> {
    use tokio::time::Duration;

    // macOS TUN devices must be named `utunN` (or left auto-allocated); the
    // name "tun0" is Linux-only. Let macOS pick a free utun automatically.
    #[cfg(target_os = "macos")]
    let tun = TunDevice::create(None, opts.tun_ip, opts.netmask, crate::tun_stack::TUN_MTU)?;
    #[cfg(not(target_os = "macos"))]
    let tun = TunDevice::create(Some("tun0"), opts.tun_ip, opts.netmask, crate::tun_stack::TUN_MTU)?;
    println!("ok tun0: dev={} ip={} mtu={}", tun.name, tun.ip, tun.mtu);

    let cfg = StackConfig {
        tun_ip: opts.tun_ip,
        fake_net: opts.fake_net,
        max_conns: opts.max_conns,
        dns: opts.dns.clone(),
    };
    let mut stack = TunStack::new(tun, &cfg)?;

    // OS-level route for the fake-ip net through the tun device.
    let dev_name = stack.phy.tun.name.clone();
    crate::tun_dev::add_route(opts.fake_net.0, opts.fake_net.1, &dev_name)?;
    println!("ok route: {}/{} -> {dev_name}", opts.fake_net.0, opts.fake_net.1);

    println!(
        "ok tun0 proxy: default_exit={} routed={}",
        opts.default_exit,
        opts.routes.is_some()
    );

    // Point system DNS at the fake-ip gateway (transparent proxying) only when
    // explicitly enabled via --fake-dns. Requires root, which `teamx tun0 start`
    // already enforces.
    let mut ip_map_ref: Option<Arc<Mutex<HashMap<Ipv4Addr, String>>>> = None;
    if opts.fake_dns {
        match crate::tun_dev::set_system_dns(&opts.tun_ip.to_string()) {
            Ok(()) => println!("ok system dns: -> {}", opts.tun_ip),
            Err(e) => eprintln!("tun: set system DNS failed: {e}"),
        }
    } else if let Some(table) = &opts.routes {
        // IP-routing mode (default): run a local DNS proxy on 127.0.0.1:53 that
        // resolves proxied domains through the exit (uncensored) and adds host
        // routes, then point system DNS at it. CIDR rules become network routes
        // directly (covers large CDN ranges like Google).
        let patterns: Vec<String> = table.intercept_patterns();
        let domains: Vec<String> = patterns
            .iter()
            .map(|p| p.strip_prefix("*.").unwrap_or(p).to_string())
            .collect();
        let cidrs: Vec<(Ipv4Addr, u8)> = table.intercept_cidrs();
        let ip_map: Arc<Mutex<HashMap<Ipv4Addr, String>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let dev = dev_name.clone();

        // Explicit CIDR network routes (static, covers large CDN ranges).
        for (net, prefix) in &cidrs {
            crate::tun_dev::add_route(*net, *prefix, &dev).ok();
        }

        // Local DNS proxy: proxied domains -> exit resolution, others -> system
        // DNS. Point system DNS at it so apps transparently route.
        if !patterns.is_empty() {
            let upstream: Vec<std::net::SocketAddr> = crate::tun_dev::system_dns_servers()
                .into_iter()
                .map(|ip| std::net::SocketAddr::new(std::net::IpAddr::V4(ip), 53))
                .collect();
            match crate::dns_proxy::spawn(
                opts.server_url.clone(),
                opts.default_exit.clone(),
                patterns.clone(),
                ip_map.clone(),
                dev.clone(),
                upstream,
            ) {
                Ok(()) => {
                    if let Err(e) = crate::tun_dev::set_system_dns_single("127.0.0.1") {
                        eprintln!("tun: set system DNS (127.0.0.1) failed: {e}");
                    } else {
                        println!("ok dns-proxy: 127.0.0.1:53 (exit={})", opts.default_exit);
                    }
                }
                Err(e) => eprintln!("tun: dns-proxy bind failed: {e}"),
            }
        }

        // Also refresh domain->IP host routes periodically (covers domains whose
        // queries bypass the proxy, and keeps routes fresh as CDN IPs rotate).
        if !domains.is_empty() {
            let m = ip_map.clone();
            let d = dev.clone();
            tokio::spawn(async move {
                ip_route_loop(&domains, &cidrs, &d, m).await;
            });
        }
        ip_map_ref = Some(ip_map);
    }

    let mut buf = [0u8; 65536];

    loop {
        stack.poll();

        // 1. New connections -> open a bridge per connection.
        while let Some((handle, remote)) = stack.take_new_connection() {
            let target = resolve_target(&remote, &opts.dns, ip_map_ref.as_ref());
            let exit = match &opts.routes {
                Some(t) => t.resolve(&target.host).to_string(),
                None => opts.default_exit.clone(),
            };
            let target_str = format!("{}:{}", target.host, target.port);
            let bridge = match crate::tunnel_client::open_tunnel_bridge(&opts.server_url, &exit, &target_str).await {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("tun: bridge to exit `{exit}` ({target_str}) failed: {e}");
                    stack.reset_socket(handle);
                    continue;
                }
            };
            let slot_idx = stack.slots.iter().position(|s| s.handle == handle).unwrap();
            stack.slots[slot_idx].state = BridgeState::Active;
            stack.slots[slot_idx].tx = Some(bridge.tx.clone());
            // Store the rx side for pumping in the main loop.
            stack.slots[slot_idx].rx = Some(bridge.rx);
            stack.slots[slot_idx].eof = Some(bridge.eof);
            // Remember the close sender for teardown.
            stack.slots[slot_idx].close = Some(bridge.close);
            println!("tun: bridge up exit={exit} target={target_str}");
        }

        // 2. Pump bytes for active sockets.
        pump_active(&mut stack, &mut buf)?;

        // 3. Yield so spawned tasks (bridge pumps) get scheduled. NOTE: must be
        // an async await — a synchronous blocking wait here would starve the
        // current_thread runtime and the bridge tasks would never run.
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// Resolve the SOCKS-style target for a remote endpoint. Prefers the original
/// domain (from the IP-routing `ip -> domain` map, then the fake-ip DNS map)
/// so the exit can dial by hostname and preserve TLS SNI; falls back to the
/// raw IP otherwise.
fn resolve_target(
    remote: &IpEndpoint,
    dns: &FakeIpDns,
    ip_map: Option<&Arc<Mutex<HashMap<Ipv4Addr, String>>>>,
) -> crate::socks5::SocksTarget {
    let port = remote.port;
    if let smoltcp::wire::IpAddress::Ipv4(v4) = remote.addr {
        let v4 = Ipv4Addr::from(v4);
        // IP-routing mode: real IP -> domain (preserves SNI for TLS).
        if let Some(m) = ip_map {
            if let Some(domain) = m.lock().unwrap().get(&v4) {
                return crate::socks5::SocksTarget { host: domain.clone(), port };
            }
        }
        // fake-dns mode: fake IP -> domain.
        if let Some(domain) = dns.lookup(v4) {
            return crate::socks5::SocksTarget { host: domain, port };
        }
        return crate::socks5::SocksTarget { host: v4.to_string(), port };
    }
    crate::socks5::SocksTarget { host: remote.addr.to_string(), port }
}

/// Pump bytes between active smoltcp sockets and their bridges.
///
/// Direction A (tun->egress): read from the smoltcp socket, push to bridge.tx.
/// Direction B (egress->tun): drain bridge.rx into the smoltcp socket.
/// EOF on either side closes the socket / bridge accordingly.
fn pump_active(stack: &mut TunStack, buf: &mut [u8]) -> Result<(), String> {
    use tokio::sync::mpsc;
    let handles: Vec<SocketHandle> = stack
        .slots
        .iter()
        .filter(|s| matches!(s.state, BridgeState::Active))
        .map(|s| s.handle)
        .collect();

    for h in handles {
        let idx = stack.slots.iter().position(|s| s.handle == h).unwrap();
        // Direction B first: drain what the exit sent us.
        {
            // Pull all queued bytes out of rx first (release the borrow), then
            // feed them into the smoltcp socket.
            let mut drained: Vec<Vec<u8>> = Vec::new();
            let mut disconnected = false;
            if let Some(rx) = &mut stack.slots[idx].rx {
                loop {
                    match rx.try_recv() {
                        Ok(bytes) => drained.push(bytes),
                        Err(mpsc::error::TryRecvError::Empty) => break,
                        Err(mpsc::error::TryRecvError::Disconnected) => {
                            disconnected = true;
                            break;
                        }
                    }
                }
            }
            for bytes in &drained {
                let mut off = 0;
                while off < bytes.len() {
                    if !stack.can_send(h) {
                        break;
                    }
                    match stack.send_to_socket(h, &bytes[off..]) {
                        Ok(0) => break,
                        Ok(n) => off += n,
                        Err(()) => break,
                    }
                }
            }
            if disconnected {
                stack.close_socket(h);
            }
        }

        // Direction A: read tun-side data, push to the exit.
        loop {
            match stack.recv_from_socket(h, buf) {
                Ok(0) => break,
                Ok(n) => {
                    let slot = &stack.slots[idx];
                    if let Some(tx) = &slot.tx {
                        let _ = tx.send(buf[..n].to_vec());
                    }
                    if n < buf.len() {
                        break;
                    }
                }
                Err(()) => {
                    // tun side EOF: mark closing, tell exit.
                    stack.reset_socket(h);
                    break;
                }
            }
        }

        // Remote (tun side) sent FIN and drained: close our send half so the
        // exit sees EOF and can tear the stream down.
        if stack.remote_fin(h) && matches!(stack.slots[idx].state, BridgeState::Active) {
            stack.close_socket(h);
        }
    }
    Ok(())
}

/// Periodically resolve the proxied domains to their real IPs and add host
/// routes through the tun device, recording the `ip -> domain` map so inbound
/// connections can dial by hostname (preserving TLS SNI). Refreshes every few
/// minutes because CDN IPs rotate. Runs as a spawned task on the tun runtime.
async fn ip_route_loop(
    domains: &[String],
    cidrs: &[(Ipv4Addr, u8)],
    dev: &str,
    ip_map: Arc<Mutex<HashMap<Ipv4Addr, String>>>,
) {
    // Add explicit CIDR network routes once (static, covers large CDN ranges).
    for (net, prefix) in cidrs {
        crate::tun_dev::add_route(*net, *prefix, dev).ok();
    }

    let mut known: Vec<Ipv4Addr> = Vec::new();
    loop {
        let resolved = resolve_domains(domains).await;
        let mut ips = Vec::new();
        {
            let mut m = ip_map.lock().unwrap();
            for (ip, domain) in &resolved {
                ips.push(*ip);
                m.insert(*ip, domain.clone());
            }
        }
        // Add routes for newly-seen IPs.
        for ip in &ips {
            if !known.contains(ip) {
                crate::tun_dev::add_ip_route(*ip, dev).ok();
            }
        }
        // Drop routes for IPs that no longer resolve.
        for ip in &known {
            if !ips.contains(ip) {
                crate::tun_dev::del_ip_route(*ip, dev);
            }
        }
        known = ips;
        tokio::time::sleep(tokio::time::Duration::from_secs(300)).await;
    }
}

/// Resolve a set of domains to their `(ip, domain)` pairs via the system
/// resolver.
async fn resolve_domains(domains: &[String]) -> Vec<(Ipv4Addr, String)> {
    let mut out = Vec::new();
    for d in domains {
        if let Ok(addrs) = tokio::net::lookup_host((d.as_str(), 443)).await {
            for a in addrs {
                if let std::net::IpAddr::V4(v4) = a.ip() {
                    out.push((v4, d.clone()));
                }
            }
        }
    }
    out.sort_by_key(|(ip, _)| *ip);
    out.dedup();
    out
}
