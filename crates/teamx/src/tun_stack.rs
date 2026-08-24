//! tun_stack.rs — smoltcp user-space TCP/IP stack over a TUN device.
//!
//! Wraps `TunDevice` as a `smoltcp::phy::Device` (Medium::Ip, no Ethernet
//! headers) and drives a `smoltcp::iface::Interface` in a poll loop. Inbound
//! connections (SYN from the host side via the fake-ip route) are completed
//! in user space; the caller (`tun_socks`) bridges each established TCP socket
//! to a teamx proxy exit.

use std::net::Ipv4Addr;
use std::sync::Arc;

use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{self, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::{tcp, udp};
use smoltcp::time::Instant;
use smoltcp::wire::{HardwareAddress, IpAddress, IpCidr, IpEndpoint, Ipv4Address};

use crate::tun_dev::TunDevice;
use crate::tun_dns::FakeIpDns;

/// MTU for the overlay interface (reduced to avoid fragmentation inside WS).
pub const TUN_MTU: u16 = 1280;

/// Default max concurrent TCP connections (overridable via --max-conns).
#[allow(dead_code)] // used as the documented default; CLI default_value_t mirrors it
pub const DEFAULT_MAX_CONNS: usize = 64;

/// smoltcp `Device` implementation backed by a raw TUN fd.
pub struct TunPhy {
    pub tun: TunDevice,
    rx_buf: Vec<u8>,
    /// Reused TX scratch buffer (avoids a heap allocation per emitted packet).
    tx_buf: Vec<u8>,
}

impl TunPhy {
    pub fn new(tun: TunDevice) -> Self {
        let mtu = tun.mtu as usize;
        TunPhy { tun, rx_buf: vec![0u8; mtu], tx_buf: vec![0u8; mtu] }
    }
}

pub struct Rx<'a>(&'a [u8]);
impl<'a> RxToken for Rx<'a> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(self.0)
    }
}

/// Tx token that writes the emitted packet straight into the tun fd. This is
/// the critical part: smoltcp hands us the bytes via `f` and we must actually
/// send them back to the host stack, otherwise SYN-ACKs / DNS replies are
/// silently dropped. The scratch buffer is reused across packets.
pub struct Tx<'a>(&'a mut Vec<u8>, &'a mut TunDevice);
impl<'a> TxToken for Tx<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        self.0.clear();
        self.0.resize(len, 0);
        let result = f(&mut self.0[..]);
        let _ = self.1.write_packet(&self.0[..len]);
        result
    }
}

impl phy::Device for TunPhy {
    type RxToken<'a> = Rx<'a> where Self: 'a;
    type TxToken<'a> = Tx<'a> where Self: 'a;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let mut n = self.tun.read_packet(&mut self.rx_buf)?;
        // macOS utun sometimes truncates TCP SYN packets (timestamp option
        // dropped) but leaves the IPv4 total_len field at the full size. Pad
        // with zeroes (TCP option 0 = EOL) so smoltcp's length check passes.
        // Note: do NOT truncate rx_buf here — that would shrink the read buffer
        // and make subsequent reads return short packets.
        if n >= 20 && self.rx_buf[0] >> 4 == 4 {
            let total_len = u16::from_be_bytes([self.rx_buf[2], self.rx_buf[3]]) as usize;
            if total_len > n && total_len <= self.rx_buf.len() {
                self.rx_buf[n..total_len].fill(0);
                n = total_len;
            }
        }
        // Return a slice of exactly n bytes (rx_buf keeps its full capacity).
        Some((Rx(&self.rx_buf[..n]), Tx(&mut self.tx_buf, &mut self.tun)))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(Tx(&mut self.tx_buf, &mut self.tun))
    }
    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ip; // tun is L3: no Ethernet header, no ARP
        caps.max_transmission_unit = self.tun.mtu as usize;
        caps.max_burst_size = Some(1);
        // macOS tun delivers packets with checksums offloaded (UDP/TCP
        // checksum = 0 on RX), so skip RX verification. But we MUST compute
        // checksums on TX: TCP checksum=0 is rejected by the host stack.
        let mut csum = smoltcp::phy::ChecksumCapabilities::ignored();
        csum.ipv4 = smoltcp::phy::Checksum::Tx;
        csum.udp = smoltcp::phy::Checksum::Tx;
        csum.tcp = smoltcp::phy::Checksum::Tx;
        csum.icmpv4 = smoltcp::phy::Checksum::Tx;
        caps.checksum = csum;
        caps
    }
}

/// Configuration for the user-space stack.
pub struct StackConfig {
    pub tun_ip: Ipv4Addr,
    /// fake-ip network (e.g. 198.18.0.0/15) whose traffic we intercept.
    pub fake_net: (Ipv4Addr, u8),
    pub max_conns: usize,
    /// Fake-ip DNS responder shared with the proxy (used to answer DNS
    /// queries inside the tun stack itself; see UDP 53 handling).
    pub dns: Arc<FakeIpDns>,
}

/// Per-socket bridge bookkeeping.
pub enum BridgeState {
    Idle,          // listening slot, no connection yet
    Connecting,    // established, WS being opened
    Active,        // bridged, bytes flowing
}

pub struct SocketSlot {
    pub handle: SocketHandle,
    pub state: BridgeState,
    pub remote: Option<IpEndpoint>,
    /// Sender half to the bridge task (bytes tun->egress). None while idle.
    pub tx: Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>,
    /// Receiver half from the bridge (bytes egress->tun). None while idle.
    pub rx: Option<tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>>,
    /// EOF notification from the bridge (egress closed).
    pub eof: Option<tokio::sync::watch::Receiver<bool>>,
    /// Close handle to abort the bridge.
    pub close: Option<tokio::sync::oneshot::Sender<()>>,
}

/// The running user-space TCP/IP stack.
pub struct TunStack {
    pub iface: Interface,
    pub phy: TunPhy,
    pub sockets: SocketSet<'static>,
    pub slots: Vec<SocketSlot>,
    /// UDP socket that answers fake-ip DNS queries inside the stack. Needed
    /// because on macOS packets destined to the tun gateway IP (198.18.0.1)
    /// are routed into the tun and never reach the host UDP stack.
    udp_dns: SocketHandle,
    dns: Arc<FakeIpDns>,
    tun_ip: Ipv4Addr,
}

impl TunStack {
    /// Build the interface + pre-allocated listening TCP sockets.
    pub fn new(tun: TunDevice, cfg: &StackConfig) -> Result<TunStack, String> {
        let mut phy = TunPhy::new(tun);

        let mut config = Config::new(HardwareAddress::Ip);
        config.random_seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0xDEAD_BEEF);

        let mut iface = Interface::new(config, &mut phy, Instant::now());
        let tun_ip = cfg.tun_ip;
        iface.update_ip_addrs(|addrs| {
            addrs.push(IpCidr::new(IpAddress::Ipv4(Ipv4Address::from(tun_ip)), 24)).ok();
        });
        // Route the whole fake-ip net through our own tun address so AnyIP
        // accepts any destination in that range.
        iface.routes_mut().update(|routes| {
            routes.push(smoltcp::iface::Route {
                cidr: IpCidr::new(
                    IpAddress::Ipv4(Ipv4Address::from(cfg.fake_net.0)),
                    cfg.fake_net.1,
                ),
                via_router: IpAddress::Ipv4(Ipv4Address::from(tun_ip)),
                preferred_until: None,
                expires_at: None,
            }).ok();
        });
        iface.set_any_ip(true);

        let max = cfg.max_conns.max(1);
        let mut sockets = SocketSet::new(vec![]);
        let mut slots = Vec::with_capacity(max);
        for _ in 0..max {
            let rx = tcp::SocketBuffer::new(vec![0u8; 65536]);
            let tx = tcp::SocketBuffer::new(vec![0u8; 65536]);
            let mut sock = tcp::Socket::new(rx, tx);
            sock.set_nagle_enabled(false);
            // listen on any port (0 = wildcard); poll() completes handshakes
            sock.listen(0).map_err(|e| format!("tcp listen: {e}"))?;
            let h = sockets.add(sock);
            slots.push(SocketSlot {
                handle: h,
                state: BridgeState::Idle,
                remote: None,
                tx: None,
                rx: None,
                eof: None,
                close: None,
            });
        }

        // UDP socket that answers fake-ip DNS (port 53) inside the stack.
        let udp_rx = udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY], vec![0u8; 2048]);
        let udp_tx = udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY], vec![0u8; 2048]);
        let mut udp_sock = udp::Socket::new(udp_rx, udp_tx);
        let tun_ip = cfg.tun_ip;
        let _ = udp_sock
            .bind(IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::from(tun_ip)), 53))
            .map_err(|e| format!("udp dns bind: {e}"))?;
        let udp_dns = sockets.add(udp_sock);

        Ok(TunStack { iface, phy, sockets, slots, udp_dns, dns: cfg.dns.clone(), tun_ip })
    }

    /// Drive one poll cycle. Returns the set of sockets whose bridge state
    /// needs attention (new established / newly readable / closed).
    pub fn poll(&mut self) {
        let now = Instant::now();
        let _ = self.iface.poll(now, &mut self.phy, &mut self.sockets);
        // Also pump maintenance timers (retransmit, keep-alive).
        self.iface.poll_maintenance(now);

        // Answer fake-ip DNS queries inside the stack (port 53 UDP).
        self.poll_udp_dns();
    }
    /// Drain UDP 53 packets received on the tun and reply with fake-ip
    /// answers. This keeps DNS working even though the tun interface absorbs
    /// all traffic to the gateway IP (macOS behavior).
    fn poll_udp_dns(&mut self) {
        let dns = self.dns.clone();
        let tun_ip = self.tun_ip;
        let mut handled = 0;
        loop {
            let recv = {
                let sock = self.sockets.get_mut::<udp::Socket>(self.udp_dns);
                sock.recv()
            };
            let (data, endpoint) = match recv {
                Ok((data, endpoint)) => (data.to_vec(), endpoint),
                Err(_) => break,
            };
            handled += 1;
            if let Some(resp) = dns.answer(&data) {
                // smoltcp would treat a reply to the tun's own IP as
                // "local delivery" and never write it back to the tun fd
                // (the peer address is the gateway itself on macOS). So build
                // the raw IPv4+UDP packet and write it to the tun directly.
                if let Some(pkt) = build_dns_response(&resp, &endpoint.endpoint, tun_ip) {
                    let _ = self.phy.tun.write_packet(&pkt);
                }
            }
            // Re-bind in case the socket dropped the endpoint after recv.
            let sock = self.sockets.get_mut::<udp::Socket>(self.udp_dns);
            if sock.endpoint().port == 0 {
                let _ = sock
                    .bind(IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::from(tun_ip)), 53));
            }
            if handled > 4 { break; }
        }
    }

    /// Look for a socket that just completed its handshake (ESTABLISHED) and
    /// is still untracked. Returns its handle + the *destination* endpoint (the
    /// fake-ip the client dialed, from `local_endpoint`), which the caller uses
    /// to map back to the original domain.
    pub fn take_new_connection(&mut self) -> Option<(SocketHandle, IpEndpoint)> {
        let mut found = None;
        for i in 0..self.slots.len() {
            let handle = self.slots[i].handle;
            let dst = self.sockets.get::<tcp::Socket>(handle).local_endpoint();
            if dst.is_some() {
                let established = matches!(
                    self.sockets.get::<tcp::Socket>(handle).state(),
                    smoltcp::socket::tcp::State::Established
                );
                let idle = matches!(self.slots[i].state, BridgeState::Idle);
                if established && idle {
                    found = Some((handle, dst.unwrap()));
                    break;
                }
            }
        }
        if let Some((h, r)) = found {
            let i = self.slots.iter().position(|s| s.handle == h).unwrap();
            self.slots[i].state = BridgeState::Connecting;
            self.slots[i].remote = Some(r);
            Some((h, r))
        } else {
            None
        }
    }

    /// Read available bytes from the given socket (tun->egress direction).
    pub fn recv_from_socket(&mut self, handle: SocketHandle, buf: &mut [u8]) -> Result<usize, ()> {
        match self.sockets.get_mut::<tcp::Socket>(handle).recv_slice(buf) {
            Ok(n) => Ok(n),
            Err(smoltcp::socket::tcp::RecvError::Finished) => Err(()), // EOF (peer closed)
            // InvalidState (e.g. handshake not yet complete) is NOT EOF; report
            // "no data yet" so the caller doesn't reset the socket.
            Err(_) => Ok(0),
        }
    }

    /// Send bytes into the socket (egress->tun direction). Returns bytes queued.
    pub fn send_to_socket(&mut self, handle: SocketHandle, data: &[u8]) -> Result<usize, ()> {
        match self.sockets.get_mut::<tcp::Socket>(handle).send_slice(data) {
            Ok(n) => Ok(n),
            Err(_) => Err(()),
        }
    }

    /// Whether the remote half has FIN'd (tun side done sending).
    pub fn remote_fin(&mut self, handle: SocketHandle) -> bool {
        let s = self.sockets.get::<tcp::Socket>(handle);
        !s.may_recv() && !s.can_recv()
    }

    /// Whether the socket can accept more data (egress->tun not backpressured).
    pub fn can_send(&mut self, handle: SocketHandle) -> bool {
        self.sockets.get::<tcp::Socket>(handle).can_send()
    }

    /// Close the socket's send half (send FIN to the host side).
    pub fn close_socket(&mut self, handle: SocketHandle) {
        self.sockets.get_mut::<tcp::Socket>(handle).close();
    }

    /// Hard-close a socket and return its slot to listening state.
    pub fn reset_socket(&mut self, handle: SocketHandle) {
        let s = self.sockets.get_mut::<tcp::Socket>(handle);
        s.abort();
        s.listen(0).ok();
        let i = self.slots.iter().position(|x| x.handle == handle).unwrap();
        self.slots[i].state = BridgeState::Idle;
        self.slots[i].remote = None;
        self.slots[i].tx = None;
        self.slots[i].rx = None;
        self.slots[i].eof = None;
        self.slots[i].close = None;
    }

    /// Sleep until the stack has work to do (or at most `max` ms).
    #[allow(dead_code)] // replaced by an async sleep in the poll loop; kept as a primitive
    pub fn wait(&self, max_ms: u64) {
        let _ = phy::wait(
            self.phy.tun.as_raw_fd(),
            Some(smoltcp::time::Duration::from_millis(max_ms)),
        );
    }
}

/// Build a raw IPv4 + UDP + DNS payload packet to inject back into the tun.
///
/// On macOS the tun gateway IP (198.18.0.1) is also the source address the OS
/// uses for packets it routes into the tun, so a DNS reply's destination ends
/// up equal to the gateway IP. smoltcp treats that as "local delivery" and
/// never transmits it, so we build and write the packet ourselves.
fn build_dns_response(
    dns_payload: &[u8],
    peer: &IpEndpoint,
    tun_ip: Ipv4Addr,
) -> Option<Vec<u8>> {
    let peer_ip = match peer.addr {
        IpAddress::Ipv4(a) => Ipv4Addr::from(a),
        _ => return None,
    };
    let src_port = 53u16;
    let dst_port = peer.port;

    let udp_len = 8usize + dns_payload.len();
    let total_len = 20usize + udp_len;

    let mut pkt = Vec::with_capacity(total_len);

    // --- IPv4 header (20 bytes) ---
    pkt.push(0x45); // version 4, IHL 5
    pkt.push(0x00); // TOS
    pkt.extend_from_slice(&(total_len as u16).to_be_bytes());
    pkt.extend_from_slice(&0u16.to_be_bytes()); // ID
    pkt.extend_from_slice(&0x4000u16.to_be_bytes()); // flags=DF, offset 0
    pkt.push(64); // TTL
    pkt.push(17); // protocol = UDP
    pkt.extend_from_slice(&[0x00, 0x00]); // checksum placeholder
    pkt.extend_from_slice(&tun_ip.octets()); // src
    pkt.extend_from_slice(&peer_ip.octets()); // dst

    // IPv4 header checksum (over the 20-byte header).
    let csum = ipv4_checksum(&pkt[..20]);
    pkt[10] = (csum >> 8) as u8;
    pkt[11] = (csum & 0xff) as u8;

    // --- UDP header (8 bytes) ---
    pkt.extend_from_slice(&src_port.to_be_bytes());
    pkt.extend_from_slice(&dst_port.to_be_bytes());
    pkt.extend_from_slice(&(udp_len as u16).to_be_bytes());
    pkt.extend_from_slice(&[0x00, 0x00]); // UDP checksum = 0 (optional in IPv4)

    // --- DNS payload ---
    pkt.extend_from_slice(dns_payload);

    Some(pkt)
}

/// One's-complement checksum over 16-bit words.
fn ipv4_checksum(header: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < header.len() {
        sum += u16::from_be_bytes([header[i], header[i + 1]]) as u32;
        i += 2;
    }
    if i < header.len() {
        sum += (header[i] as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}
