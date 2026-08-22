//! tun_socks.rs — bridge established user-space TCP sockets to teamx exits.
//!
//! Runs a single-threaded tokio runtime that owns the smoltcp stack (not
//! Send) plus the tunnel bridges. Every inbound connection whose handshake
//! completes in user space opens a `TunnelBridge` to the egress chosen by the
//! route table, and bytes are copied in both directions in the main loop.

use std::net::Ipv4Addr;
use std::sync::Arc;

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
    use tokio::time::{timeout, Duration};

    let tun = TunDevice::create(Some("tun0"), opts.tun_ip, opts.netmask, crate::tun_stack::TUN_MTU)?;
    println!("ok tun0: dev={} ip={} mtu={}", tun.name, tun.ip, tun.mtu);

    let cfg = StackConfig {
        tun_ip: opts.tun_ip,
        fake_net: opts.fake_net,
        max_conns: opts.max_conns,
    };
    let mut stack = TunStack::new(tun, &cfg)?;

    // OS-level route for the fake-ip net through the tun device.
    let dev_name = stack.phy.tun.name.clone();
    crate::tun_dev::add_route(opts.fake_net.0, opts.fake_net.1, &dev_name)?;
    println!("ok route: {}/{} -> {dev_name}", opts.fake_net.0, opts.fake_net.1);

    // Fake-ip DNS listener on a dedicated thread (UDP socket is Send).
    // Bind to 0.0.0.0:53 so it works regardless of tun address readiness.
    let dns = opts.dns.clone();
    let dns_addr = opts.tun_ip;
    let _dns_thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("dns runtime");
        if let Err(e) = rt.block_on(crate::tun_dns::serve_udp(dns, dns_addr)) {
            eprintln!("tun: fake-dns failed: {e}");
        }
    });

    println!(
        "ok tun0 proxy: default_exit={} routed={}",
        opts.default_exit,
        opts.routes.is_some()
    );

    let mut buf = [0u8; 65536];

    loop {
        stack.poll();

        // 1. New connections -> open a bridge per connection.
        while let Some((handle, remote)) = stack.take_new_connection() {
            let target = resolve_target(&remote, &opts.dns);
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

        // 3. Sleep briefly (poll loop cadence).
        let _ = timeout(Duration::from_millis(10), async {
            stack.wait(1);
        })
        .await;
    }
}

/// Resolve the SOCKS-style target for a remote endpoint: fake-ip -> domain.
fn resolve_target(remote: &IpEndpoint, dns: &FakeIpDns) -> crate::socks5::SocksTarget {
    let port = remote.port;
    if let smoltcp::wire::IpAddress::Ipv4(v4) = remote.addr {
        let v4 = Ipv4Addr::from(v4);
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
