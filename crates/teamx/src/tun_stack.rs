//! tun_stack.rs — smoltcp user-space TCP/IP stack over a TUN device.
//!
//! Wraps `TunDevice` as a `smoltcp::phy::Device` (Medium::Ip, no Ethernet
//! headers) and drives a `smoltcp::iface::Interface` in a poll loop. Inbound
//! connections (SYN from the host side via the fake-ip route) are completed
//! in user space; the caller (`tun_socks`) bridges each established TCP socket
//! to a teamx proxy exit.

use std::net::Ipv4Addr;

use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{self, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::tcp;
use smoltcp::time::Instant;
use smoltcp::wire::{HardwareAddress, IpAddress, IpCidr, IpEndpoint, Ipv4Address};

use crate::tun_dev::TunDevice;

/// MTU for the overlay interface (reduced to avoid fragmentation inside WS).
pub const TUN_MTU: u16 = 1280;

/// Default max concurrent TCP connections (overridable via --max-conns).
#[allow(dead_code)] // used as the documented default; CLI default_value_t mirrors it
pub const DEFAULT_MAX_CONNS: usize = 64;

/// smoltcp `Device` implementation backed by a raw TUN fd.
pub struct TunPhy {
    pub tun: TunDevice,
    rx_buf: Vec<u8>,
    tx_buf: Vec<u8>,
}

impl TunPhy {
    pub fn new(tun: TunDevice) -> Self {
        let mtu = tun.mtu as usize;
        TunPhy { tun, rx_buf: vec![0u8; mtu], tx_buf: vec![0u8; mtu] }
    }
}

pub struct Rx<'a>(&'a mut [u8]);
impl<'a> RxToken for Rx<'a> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(&self.0)
    }
}

pub struct Tx<'a>(&'a mut [u8]);
impl<'a> TxToken for Tx<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        f(&mut self.0[..len])
    }
}

impl phy::Device for TunPhy {
    type RxToken<'a> = Rx<'a> where Self: 'a;
    type TxToken<'a> = Tx<'a> where Self: 'a;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let n = self.tun.read_packet(&mut self.rx_buf)?;
        self.rx_buf.truncate(n);
        Some((Rx(&mut self.rx_buf), Tx(&mut self.tx_buf)))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(Tx(&mut self.tx_buf))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ip; // tun is L3: no Ethernet header, no ARP
        caps.max_transmission_unit = self.tun.mtu as usize;
        caps.max_burst_size = Some(1);
        caps
    }
}

/// Configuration for the user-space stack.
pub struct StackConfig {
    pub tun_ip: Ipv4Addr,
    /// fake-ip network (e.g. 198.18.0.0/15) whose traffic we intercept.
    pub fake_net: (Ipv4Addr, u8),
    pub max_conns: usize,
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

        Ok(TunStack { iface, phy, sockets, slots })
    }

    /// Drive one poll cycle. Returns the set of sockets whose bridge state
    /// needs attention (new established / newly readable / closed).
    pub fn poll(&mut self) {
        let now = Instant::now();
        let _ = self.iface.poll(now, &mut self.phy, &mut self.sockets);
        // Also pump maintenance timers (retransmit, keep-alive).
        self.iface.poll_maintenance(now);
    }

    /// Look for a socket that just became active (handshake completed) and is
    /// still untracked. Returns its handle + remote endpoint.
    pub fn take_new_connection(&mut self) -> Option<(SocketHandle, IpEndpoint)> {
        let mut found = None;
        for i in 0..self.slots.len() {
            let handle = self.slots[i].handle;
            let remote = self.sockets.get::<tcp::Socket>(handle).remote_endpoint();
            if remote.is_some() {
                let is_active = self.sockets.get::<tcp::Socket>(handle).is_active();
                let idle = matches!(self.slots[i].state, BridgeState::Idle);
                if is_active && idle {
                    found = Some((handle, remote.unwrap()));
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
            Err(smoltcp::socket::tcp::RecvError::Finished) => Err(()), // EOF
            Err(_) => Err(()),
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
    pub fn wait(&self, max_ms: u64) {
        let _ = phy::wait(
            self.phy.tun.as_raw_fd(),
            Some(smoltcp::time::Duration::from_millis(max_ms)),
        );
    }
}
