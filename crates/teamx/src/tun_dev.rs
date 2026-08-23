//! tun_dev.rs — cross-platform TUN device wrapper.
//!
//! Creates and manages the `tun0` virtual network interface:
//!   - macOS: `utunN` (auto-allocated), configured via ifconfig + route by the
//!     underlying `tun` crate (needs root).
//!   - Linux: `tunN` via /dev/net/tun + ioctl (needs root / CAP_NET_ADMIN).
//!
//! The wrapper exposes raw packet read/write (L3 IP packets, no Ethernet
//! header) plus the raw fd for smoltcp's `phy::wait` poll loop.

use std::io::{Read, Write};
use std::net::Ipv4Addr;
use std::os::fd::AsRawFd;
use tun::AbstractDevice;

/// A configured, up and running TUN device.
pub struct TunDevice {
    dev: tun::Device,
    /// Actual interface name (`utun3`, `tun0`, ...) — may differ from the
    /// requested name when the OS auto-allocates (macOS).
    pub name: String,
    /// The device's own IP on the fake-ip subnet.
    pub ip: Ipv4Addr,
    pub mtu: u16,
}

impl TunDevice {
    /// Create and bring up a TUN device.
    ///
    /// - `name`: optional requested name (`tun0` / `utun0`); macOS may allocate
    ///   a different `utunN`.
    /// - `ip` / `netmask`: the interface address + netmask.
    /// - `mtu`: interface MTU (recommend 1280 for overlay links).
    ///
    /// Requires root (euid 0) on both platforms; the caller should check
    /// privileges before calling (see `require_root`).
    pub fn create(name: Option<&str>, ip: Ipv4Addr, netmask: Ipv4Addr, mtu: u16) -> Result<TunDevice, String> {
        let mut config = tun::Configuration::default();
        if let Some(n) = name {
            config.tun_name(n);
        }
        config
            .address(ip)
            .netmask(netmask)
            .destination(ip) // peer = self for point-to-point
            .mtu(mtu)
            .up();

        #[cfg(target_os = "linux")]
        config.platform_config(|c| {
            c.ensure_root_privileges(true);
        });

        let dev = tun::create(&config).map_err(|e| format!("tun create: {e}"))?;
        let actual = dev.tun_name().unwrap_or_else(|_| name.unwrap_or("tun").to_string());
        Ok(TunDevice { dev, name: actual, ip, mtu })
    }

    /// Read one IP packet (non-blocking). Returns `None` when no packet is
    /// currently available (EWOULDBLOCK / EAGAIN).
    pub fn read_packet(&mut self, buf: &mut [u8]) -> Option<usize> {
        match self.dev.read(buf) {
            Ok(n) if n > 0 => Some(n),
            Ok(_) | Err(_) => None,
        }
    }

    /// Write one IP packet into the device (delivered to the host stack, i.e.
    /// the application side). Public API for direct packet injection.
    #[allow(dead_code)] // used by smoltcp via TunPhy; kept as a direct primitive
    pub fn write_packet(&mut self, packet: &[u8]) -> Result<(), String> {
        self.dev.write(packet).map(|_| ()).map_err(|e| format!("tun write: {e}"))
    }

    /// Raw fd for poll loops (smoltcp `phy::wait`).
    pub fn as_raw_fd(&self) -> i32 {
        self.dev.as_raw_fd()
    }
}

/// Check that the process is running with root privileges (required to create
/// a TUN device and to inject routes).
pub fn require_root() -> Result<(), String> {
    #[cfg(unix)]
    {
        // SAFETY: geteuid is safe.
        let uid = unsafe { libc::geteuid() };
        if uid != 0 {
            return Err(
                "tun0 requires root privileges — run with `sudo teamx tun0 start`".to_string(),
            );
        }
    }
    #[cfg(not(unix))]
    {
        return Err("tun0 is only supported on Linux and macOS".to_string());
    }
    Ok(())
}

/// Add a route pushing `net/cidr` through the TUN device.
pub fn add_route(net: Ipv4Addr, cidr: u8, dev: &str) -> Result<(), String> {
    let prefix = format!("{net}/{cidr}");
    #[cfg(target_os = "macos")]
    {
        let st = std::process::Command::new("route")
            .args(["-n", "add", "-net", &prefix, "-interface", dev])
            .status()
            .map_err(|e| format!("route add (macos): {e}"))?;
        if !st.success() {
            return Err(format!("route add failed: {prefix} -> {dev} (exit {:?})", st.code()));
        }
    }
    #[cfg(target_os = "linux")]
    {
        let st = std::process::Command::new("ip")
            .args(["route", "add", &prefix, "dev", dev])
            .status()
            .map_err(|e| format!("route add (linux): {e}"))?;
        if !st.success() {
            return Err(format!("route add failed: {prefix} -> {dev} (exit {:?})", st.code()));
        }
    }
    Ok(())
}

/// Remove the route for `net/cidr` through `dev`. Best-effort (ignores
/// "does not exist" errors).
pub fn del_route(net: Ipv4Addr, cidr: u8, dev: &str) -> Result<(), String> {
    let prefix = format!("{net}/{cidr}");
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("route")
            .args(["-n", "delete", "-net", &prefix, "-interface", dev])
            .status();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("ip")
            .args(["route", "del", &prefix, "dev", dev])
            .status();
    }
    Ok(())
}
